#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then set +x; fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
fixture_directory="$repository_root/fixtures/api/live/$dokploy_version"
sanitizer="$script_directory/sanitize-fixture.jq"
if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before capturing the Backup contract." >&2
    exit 1
fi

umask 077
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/backup-contract-$run_id"
publish_directory="$workspace/publish"
candidate_fixture_root="$workspace/candidate/api/live"
candidate_versioned_fixture_directory="$candidate_fixture_root/$dokploy_version"
published_fixture_backup="$workspace/published-fixtures.backup"
secret_canary_file="$workspace/backup-canaries"
mkdir -p "$publish_directory"
chmod 700 "$workspace" "$publish_directory"
: >"$secret_canary_file"
chmod 600 "$secret_canary_file"

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

project_name="backup-sdk-contract-$run_id"
destination_name="backup-sdk-contract-$run_id"
updated_destination_name="backup-sdk-contract-updated-$run_id"
tripwire_name="backup-contract-tripwire-${run_id//[^a-zA-Z0-9]/}"
tripwire_image="$dokploy_image"
project_id=""
destination_id=""
updated_destination_id=""
mutation_attempted=false
cleanup_confirmed=false
capture_succeeded=false
publication_started=false
publication_complete=false

urlencode() { jq -nr --arg value "$1" '$value | @uri'; }

api_request() {
    local method="$1" endpoint="$2" destination="$3" body_file="${4:-}"
    local arguments=(
        --silent --show-error --output "$destination" --write-out '%{http_code}'
        --header "@$auth_header_file" --request "$method"
    )
    if [[ -n "$body_file" ]]; then
        arguments+=(--header 'Content-Type: application/json' --data-binary "@$body_file")
    fi
    curl "${arguments[@]}" "$base_url/api/$endpoint"
}

require_status() {
    if [[ "$1" != "$2" ]]; then
        echo "$3 returned HTTP $1; expected HTTP $2." >&2
        return 1
    fi
}

recover_destination_id() {
    local name="$1" response_status count
    response_status="$(api_request GET destination.all "$workspace/destination-all.recovery.json")" ||
        response_status=""
    if [[ "$response_status" != 200 ]]; then return; fi
    count="$(jq --arg name "$name" '[.[] | select(.name == $name)] | length' \
        "$workspace/destination-all.recovery.json")" || count=""
    if [[ "$count" == 1 ]]; then
        jq -er --arg name "$name" '.[] | select(.name == $name) | .destinationId' \
            "$workspace/destination-all.recovery.json" || true
    elif [[ -n "$count" && "$count" != 0 ]]; then
        echo "Cleanup found multiple disposable destination candidates." >&2
    fi
}

remove_destination() {
    local id="$1" label="$2" response_status
    if [[ -z "$id" ]]; then return; fi
    jq -n --arg destinationId "$id" '{destinationId:$destinationId}' \
        >"$workspace/destination-remove.$label.request.json"
    response_status="$(api_request POST destination.remove \
        "$workspace/destination-remove.$label.json" \
        "$workspace/destination-remove.$label.request.json")"
    if [[ "$response_status" != 200 && "$response_status" != 404 ]]; then
        echo "Destination cleanup for $label returned HTTP $response_status." >&2
        return 1
    fi
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/backup-contract-*) ;;
        *) echo "Refusing to delete an unexpected Backup capture workspace." >&2; return 1 ;;
    esac
    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?" response_status verify_status
    local restore_ready=true
    trap - EXIT INT TERM
    set +e
    if [[ "$publication_started" == true && "$publication_complete" == false ]]; then
        if [[ -d "$published_fixture_backup" ]]; then
            if [[ -e "$fixture_directory" ]] \
                && ! mv "$fixture_directory" "$workspace/failed-publication"
            then
                echo "Could not quarantine the partially published fixture directory." >&2
                restore_ready=false
                exit_code=1
            fi
            if [[ "$restore_ready" == true && ! -e "$fixture_directory" ]]; then
                mv "$published_fixture_backup" "$fixture_directory" || exit_code=1
            else
                echo "Could not restore the previous tracked fixture directory." >&2
                exit_code=1
            fi
        else
            echo "Fixture publication was interrupted without a recoverable backup." >&2
            exit_code=1
        fi
    fi

    docker rm -f "$tripwire_name" >/dev/null 2>&1 || true
    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        if [[ -z "$project_id" ]]; then
            response_status="$(api_request GET project.all "$workspace/project-all.recovery.json")" ||
                response_status=""
            if [[ "$response_status" == 200 ]]; then
                project_id="$(jq -er --arg name "$project_name" '
                    [.[] | select(.name == $name)]
                    | if length == 1 then .[0].projectId else empty end
                ' "$workspace/project-all.recovery.json" 2>/dev/null)" || project_id=""
            fi
        fi
        if [[ -n "$project_id" ]]; then
            jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
                >"$workspace/project-remove.cleanup.request.json"
            response_status="$(api_request POST project.remove "$workspace/project-remove.cleanup.json" \
                "$workspace/project-remove.cleanup.request.json")"
            if [[ "$response_status" == 200 || "$response_status" == 404 ]]; then
                verify_status="$(api_request GET "project.one?projectId=$(urlencode "$project_id")" \
                    "$workspace/project-one.cleanup.json")"
                [[ "$verify_status" == 404 ]] || exit_code=1
            else
                exit_code=1
            fi
        else
            echo "Project cleanup could not recover the disposable identity." >&2
            exit_code=1
        fi
        if [[ -z "$destination_id" ]]; then
            destination_id="$(recover_destination_id "$destination_name")"
        fi
        if [[ -z "$updated_destination_id" ]]; then
            updated_destination_id="$(recover_destination_id "$updated_destination_name")"
        fi
        remove_destination "$destination_id" initial || exit_code=1
        remove_destination "$updated_destination_id" updated || exit_code=1
    fi
    if [[ "$capture_succeeded" == true ]]; then
        discard_private_workspace || exit_code=1
    else
        echo "Private Backup capture evidence remains in $workspace" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

safe_backup_filter='{
    backupId,schedule,enabled,database,prefix,destinationId,keepLatestCount,
    includeEncryptionKey,backupType,databaseType,composeId,postgresId,mariadbId,
    mysqlId,mongoId,libsqlId,serviceName,metadata
}'

publish_backup() {
    local source="$1" destination="$2"
    jq "$safe_backup_filter" "$source" \
        | jq --sort-keys --indent 2 --from-file "$sanitizer" \
        >"$publish_directory/$destination"
}

publish_parent() {
    local source="$1" destination="$2"
    jq '
        {
            postgresId,
            applicationStatus,
            deployments,
            backups:[.backups[] | {
                backupId,schedule,enabled,database,prefix,destinationId,keepLatestCount,
                includeEncryptionKey,backupType,databaseType,composeId,postgresId,mariadbId,
                mysqlId,mongoId,libsqlId,serviceName,metadata
            }]
        }
    ' "$source" | jq --sort-keys --indent 2 --from-file "$sanitizer" \
        >"$publish_directory/$destination"
}

response_status="$(api_request GET settings.getDokployVersion "$workspace/version.json")"
require_status "$response_status" 200 settings.getDokployVersion
runtime_version="$(jq -er '.' "$workspace/version.json")"
if [[ "$runtime_version" != "$dokploy_version" ]]; then
    echo "Expected Dokploy $dokploy_version, received $runtime_version." >&2
    exit 1
fi

docker run --detach --rm \
    --network dokploy-iac-integration_default \
    --name "$tripwire_name" \
    --entrypoint node \
    "$tripwire_image" \
    -e 'require("net").createServer(socket => { console.log("TRIPWIRE_HIT"); socket.destroy(); }).listen(9000, "0.0.0.0", () => console.log("TRIPWIRE_READY")); setInterval(() => {}, 60000);' \
    >"$workspace/tripwire.container-id"
for _ in {1..30}; do
    if docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_READY; then break; fi
    sleep 1
done
if ! docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_READY; then
    echo "The local Backup tripwire did not become ready." >&2
    exit 1
fi

jq -n --arg name "$project_name" '{name:$name,description:"Disposable Backup SDK contract"}' \
    >"$workspace/project-create.request.json"
mutation_attempted=true
response_status="$(api_request POST project.create "$workspace/project-create.json" \
    "$workspace/project-create.request.json")"
require_status "$response_status" 200 project.create
project_id="$(jq -er '.project.projectId' "$workspace/project-create.json")"
environment_id="$(jq -er '.environment.environmentId' "$workspace/project-create.json")"

database_password="backup-contract-db-password-$run_id"
printf '%s\n' "$database_password" >>"$secret_canary_file"
jq -n --arg environmentId "$environment_id" --arg password "$database_password" '
    {
        name:"Backup Contract Postgres",databaseName:"postgres",databaseUser:"postgres",
        databasePassword:$password,environmentId:$environmentId,dockerImage:"postgres:18"
    }
' >"$workspace/postgres-create.request.json"
response_status="$(api_request POST postgres.create "$workspace/postgres-create.json" \
    "$workspace/postgres-create.request.json")"
require_status "$response_status" 200 postgres.create
postgres_id="$(jq -er '.postgresId' "$workspace/postgres-create.json")"

create_destination() {
    local name="$1" label="$2" access_key secret_key response_status
    access_key="backup-contract-access-$label-$run_id"
    secret_key="backup-contract-secret-$label-$run_id"
    printf '%s\n' "$access_key" "$secret_key" >>"$secret_canary_file"
    jq -n --arg name "$name" --arg endpoint "http://$tripwire_name:9000" \
        --arg access "$access_key" --arg secret "$secret_key" '
        {
            name:$name,provider:"Other",accessKey:$access,secretAccessKey:$secret,
            bucket:"backup-contract",region:"us-east-1",endpoint:$endpoint,
            additionalFlags:[]
        }
    ' >"$workspace/destination-create.$label.request.json"
    response_status="$(api_request POST destination.create \
        "$workspace/destination-create.$label.json" \
        "$workspace/destination-create.$label.request.json")"
    require_status "$response_status" 200 "destination.create $label"
    jq -er '.destinationId' "$workspace/destination-create.$label.json"
}

destination_id="$(create_destination "$destination_name" initial)"
updated_destination_id="$(create_destination "$updated_destination_name" updated)"

response_status="$(api_request GET "postgres.one?postgresId=$(urlencode "$postgres_id")" \
    "$workspace/postgres-one.backup-preflight.json")"
require_status "$response_status" 200 "postgres.one before Backup create"
jq -e '.applicationStatus == "idle" and (.deployments | length) == 0 and (.backups | length) == 0' \
    "$workspace/postgres-one.backup-preflight.json" >/dev/null

jq -n --arg postgresId "$postgres_id" --arg destinationId "$destination_id" '
    {
        schedule:"17 3 * * *",enabled:false,prefix:"/contract-created/",
        destinationId:$destinationId,keepLatestCount:2,database:"postgres",
        databaseType:"postgres",backupType:"database",serviceName:null,
        includeEncryptionKey:true,metadata:null,postgresId:$postgresId
    }
' >"$workspace/backup-create.request.json"
response_status="$(api_request POST backup.create "$workspace/backup-create.json" \
    "$workspace/backup-create.request.json")"
require_status "$response_status" 200 backup.create

response_status="$(api_request GET "postgres.one?postgresId=$(urlencode "$postgres_id")" \
    "$workspace/postgres-one.backup-created.json")"
require_status "$response_status" 200 "postgres.one after Backup create"
jq -e --arg destinationId "$destination_id" '
    .applicationStatus == "idle" and (.deployments | length) == 0
    and ([.backups[] | select(
        .destinationId == $destinationId and .enabled == false
        and .schedule == "17 3 * * *" and .prefix == "/contract-created/"
        and .database == "postgres" and .keepLatestCount == 2
        and .includeEncryptionKey == true and .backupType == "database"
        and .databaseType == "postgres" and (.deployments | length) == 0
    )] | length) == 1
' "$workspace/postgres-one.backup-created.json" >/dev/null
backup_id="$(jq -er --arg destinationId "$destination_id" '
    .backups[] | select(.destinationId == $destinationId) | .backupId
' "$workspace/postgres-one.backup-created.json")"

response_status="$(api_request GET "backup.one?backupId=$(urlencode "$backup_id")" \
    "$workspace/backup-one.created.json")"
require_status "$response_status" 200 "backup.one after create"

jq -n --arg backupId "$backup_id" --arg destinationId "$updated_destination_id" '
    {
        schedule:"*/1 * * * *",enabled:false,prefix:"/contract-updated/",
        backupId:$backupId,destinationId:$destinationId,database:"postgres-updated",
        keepLatestCount:3,serviceName:null,metadata:null,databaseType:"postgres",
        includeEncryptionKey:false
    }
' >"$workspace/backup-update.request.json"
response_status="$(api_request POST backup.update "$workspace/backup-update.json" \
    "$workspace/backup-update.request.json")"
require_status "$response_status" 200 backup.update

response_status="$(api_request GET "backup.one?backupId=$(urlencode "$backup_id")" \
    "$workspace/backup-one.updated.json")"
require_status "$response_status" 200 "backup.one after update"
response_status="$(api_request GET "postgres.one?postgresId=$(urlencode "$postgres_id")" \
    "$workspace/postgres-one.backup-updated.json")"
require_status "$response_status" 200 "postgres.one after Backup update"
jq -e --arg backupId "$backup_id" --arg destinationId "$updated_destination_id" '
    .applicationStatus == "idle" and (.deployments | length) == 0
    and ([.backups[] | select(
        .backupId == $backupId and .destinationId == $destinationId
        and .enabled == false and .schedule == "*/1 * * * *"
        and .prefix == "/contract-updated/" and .database == "postgres-updated"
        and .keepLatestCount == 3 and .includeEncryptionKey == false
        and (.deployments | length) == 0
    )] | length) == 1
' "$workspace/postgres-one.backup-updated.json" >/dev/null
if docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_HIT; then
    echo "The disabled Backup contacted its inert destination." >&2
    exit 1
fi

jq -n --arg backupId "$backup_id" '{backupId:$backupId}' \
    >"$workspace/backup-remove.request.json"
response_status="$(api_request POST backup.remove "$workspace/backup-remove.json" \
    "$workspace/backup-remove.request.json")"
require_status "$response_status" 200 backup.remove
response_status="$(api_request GET "backup.one?backupId=$(urlencode "$backup_id")" \
    "$workspace/backup-one.deleted.json")"
require_status "$response_status" 404 "backup.one after remove"
response_status="$(api_request GET "postgres.one?postgresId=$(urlencode "$postgres_id")" \
    "$workspace/postgres-one.backup-deleted.json")"
require_status "$response_status" 200 "postgres.one after Backup remove"
jq -e '.applicationStatus == "idle" and (.deployments | length) == 0 and (.backups | length) == 0' \
    "$workspace/postgres-one.backup-deleted.json" >/dev/null
if docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_HIT; then
    echo "The Backup lifecycle contacted its inert destination." >&2
    exit 1
fi

jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
    >"$workspace/project-remove.request.json"
response_status="$(api_request POST project.remove "$workspace/project-remove.json" \
    "$workspace/project-remove.request.json")"
require_status "$response_status" 200 project.remove
response_status="$(api_request GET "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.backup-deleted.json")"
require_status "$response_status" 404 "project.one after Backup cleanup"
project_id=""

remove_destination "$destination_id" initial
remove_destination "$updated_destination_id" updated
destination_id=""
updated_destination_id=""
docker rm -f "$tripwire_name" >/dev/null
cleanup_confirmed=true

publish_backup "$workspace/backup-one.created.json" backup-one.created.owner.json
publish_parent "$workspace/postgres-one.backup-created.json" postgres-one.backup-created.owner.json
publish_backup "$workspace/backup-one.updated.json" backup-one.updated.owner.json
publish_parent "$workspace/postgres-one.backup-updated.json" postgres-one.backup-updated.owner.json
jq --sort-keys --indent 2 --from-file "$sanitizer" "$workspace/backup-one.deleted.json" \
    >"$publish_directory/backup-one.deleted.owner.json"
publish_parent "$workspace/postgres-one.backup-deleted.json" postgres-one.backup-deleted.owner.json
jq --sort-keys --indent 2 --from-file "$sanitizer" "$workspace/project-one.backup-deleted.json" \
    >"$publish_directory/project-one.backup-deleted.owner.json"

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg role owner --arg version "$runtime_version" \
    --arg image "$tripwire_image" '
    {
        capturedAt:$capturedAt,role:$role,version:$version,image:$image,sanitized:true,
        deployed:false,executed:false,destinationContacted:false,enabled:false,
        supportedTargets:["postgres","mysql","mariadb","mongo","libsql"],
        unsupportedTargets:["compose","web-server"],
        authoritativeCollection:"target.one.backups",
        collisionKey:"target+destination+prefix+database+service",
        createIdentity:{setDifference:true,exactlyOneNewId:true,directVerified:true,parentVerified:true},
        update:{allMutableFieldsPersisted:true,targetImmutable:true,directVerified:true,parentVerified:true},
        cleanupEvidence:{oneStatus:404,targetBackupsEmpty:true,projectOneStatus:404},
        destination:{createdWithoutTestConnection:true,removed:true},
        endpoints:["backup.create","backup.one","backup.update","backup.remove"]
    }
' >"$publish_directory/backup-contract.metadata.json"

mkdir -p "$candidate_versioned_fixture_directory"
cp -R "$fixture_directory/." "$candidate_versioned_fixture_directory/"
for fixture in "$publish_directory"/*.json; do
    cp "$fixture" "$candidate_versioned_fixture_directory/$(basename "$fixture")"
done
find "$candidate_fixture_root" -type d -exec chmod 755 {} +
find "$candidate_fixture_root" -type f -exec chmod 644 {} +

if grep -R -F -q -f "$api_key_file" "$candidate_fixture_root"; then
    echo "Sanitized Backup fixtures contain the local API key." >&2
    exit 1
fi
if grep -R -F -q -f "$secret_canary_file" "$candidate_fixture_root"; then
    echo "Sanitized Backup fixtures contain a credential canary." >&2
    exit 1
fi

DOKPLOY_FIXTURE_DIRECTORY="$candidate_fixture_root" "$script_directory/check-fixtures.sh"
publication_started=true
mv "$fixture_directory" "$published_fixture_backup"
mv "$candidate_versioned_fixture_directory" "$fixture_directory"
publication_complete=true
capture_succeeded=true

echo "Captured the sanitized disabled Postgres Backup contract without deployment, execution, or destination traffic."
