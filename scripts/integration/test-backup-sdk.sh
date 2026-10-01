#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then set +x; fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=backup-evidence.sh
source "$script_directory/backup-evidence.sh"

api_key_file="$state_directory/api-key"
if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before the live Backup SDK test." >&2
    exit 1
fi

umask 077
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/backup-sdk-test-$run_id"
private_directory="$workspace/private"
evidence_directory="$workspace/evidence"
handshake_directory="$private_directory/handshake"
mkdir -p "$handshake_directory"
chmod 700 "$workspace" "$private_directory" "$handshake_directory"

auth_header_file="$private_directory/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

project_name="backup-sdk-live-$run_id"
destination_name="backup-sdk-live-$run_id"
updated_destination_name="backup-sdk-live-updated-$run_id"
tripwire_name="backup-sdk-tripwire-${run_id//[^a-zA-Z0-9]/}"
tripwire_image="dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
project_id=""
destination_id=""
updated_destination_id=""
mutation_attempted=false
cleanup_confirmed=false
test_succeeded=false
test_pid=""

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

recover_id_by_name() {
    local name="$1" field="$2"
    api_request GET destination.all "$private_directory/destination-all.recovery.json" >/dev/null || return
    jq -er --arg name "$name" --arg field "$field" '
        [.[] | select(.name == $name)]
        | if length == 1 then .[0][$field] else empty end
    ' "$private_directory/destination-all.recovery.json" || true
}

remove_destination() {
    local id="$1" label="$2" status
    if [[ -z "$id" ]]; then return; fi
    jq -n --arg destinationId "$id" '{destinationId:$destinationId}' \
        >"$private_directory/destination-remove.$label.request.json"
    status="$(api_request POST destination.remove \
        "$private_directory/destination-remove.$label.json" \
        "$private_directory/destination-remove.$label.request.json")"
    if [[ "$status" != 200 && "$status" != 404 ]]; then
        echo "Destination cleanup for $label returned HTTP $status." >&2
        return 1
    fi
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/backup-sdk-test-*) ;;
        *) echo "Refusing to delete an unexpected Backup SDK workspace." >&2; return 1 ;;
    esac
    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?" status tripwire_hit_count
    trap - EXIT INT TERM
    set +e
    if [[ -n "$test_pid" ]] && kill -0 "$test_pid" 2>/dev/null; then
        mkdir -p "$handshake_directory/continue"
        wait "$test_pid" || exit_code=1
    fi
    tripwire_hit_count="$(docker logs "$tripwire_name" 2>/dev/null | grep -c TRIPWIRE_HIT || true)"
    [[ "$tripwire_hit_count" =~ ^[0-9]+$ ]] || tripwire_hit_count=0
    docker rm -f "$tripwire_name" >/dev/null 2>&1 || true
    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        if [[ -z "$project_id" ]]; then
            api_request GET project.all "$private_directory/project-all.recovery.json" >/dev/null || true
            project_id="$(jq -er --arg name "$project_name" '
                [.[] | select(.name == $name)]
                | if length == 1 then .[0].projectId else empty end
            ' "$private_directory/project-all.recovery.json" 2>/dev/null)" || project_id=""
        fi
        if [[ -n "$project_id" ]]; then
            jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
                >"$private_directory/project-remove.cleanup.request.json"
            status="$(api_request POST project.remove "$private_directory/project-remove.cleanup.json" \
                "$private_directory/project-remove.cleanup.request.json")"
            [[ "$status" == 200 || "$status" == 404 ]] || exit_code=1
        fi
        if [[ -z "$destination_id" ]]; then
            destination_id="$(recover_id_by_name "$destination_name" destinationId)"
        fi
        if [[ -z "$updated_destination_id" ]]; then
            updated_destination_id="$(recover_id_by_name "$updated_destination_name" destinationId)"
        fi
        remove_destination "$destination_id" initial || exit_code=1
        remove_destination "$updated_destination_id" updated || exit_code=1
    fi
    if [[ "$test_succeeded" == true ]]; then
        backup_scrub_private_directory "$private_directory" || exit_code=1
        discard_private_workspace || exit_code=1
    else
        backup_publish_safe_failure_evidence \
            "$private_directory" "$evidence_directory" "$exit_code" \
            "$mutation_attempted" "$cleanup_confirmed" "$tripwire_hit_count" || exit_code=1
        backup_scrub_private_directory "$private_directory" || exit_code=1
        echo "Sanitized Backup SDK evidence remains in $evidence_directory" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

status="$(api_request GET settings.getDokployVersion "$private_directory/version.json")"
require_status "$status" 200 settings.getDokployVersion
jq -e '. == "v0.30.6"' "$private_directory/version.json" >/dev/null

docker run --detach --rm \
    --network dokploy-iac-integration_default \
    --name "$tripwire_name" \
    --entrypoint node \
    "$tripwire_image" \
    -e 'require("net").createServer(socket => { console.log("TRIPWIRE_HIT"); socket.destroy(); }).listen(9000, "0.0.0.0", () => console.log("TRIPWIRE_READY")); setInterval(() => {}, 60000);' \
    >"$private_directory/tripwire.container-id"
for _ in {1..30}; do
    if docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_READY; then break; fi
    sleep 1
done
if ! docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_READY; then
    echo "The local Backup tripwire did not become ready." >&2
    exit 1
fi

jq -n --arg name "$project_name" '{name:$name,description:"Disposable live Backup SDK test"}' \
    >"$private_directory/project-create.request.json"
mutation_attempted=true
status="$(api_request POST project.create "$private_directory/project-create.json" \
    "$private_directory/project-create.request.json")"
require_status "$status" 200 project.create
project_id="$(jq -er '.project.projectId' "$private_directory/project-create.json")"
environment_id="$(jq -er '.environment.environmentId' "$private_directory/project-create.json")"

database_password="backup-db-password-$run_id"
jq -n --arg environmentId "$environment_id" --arg password "$database_password" '
    {
        name:"Live Backup Postgres",databaseName:"postgres",databaseUser:"postgres",
        databasePassword:$password,environmentId:$environmentId,dockerImage:"postgres:18"
    }
' >"$private_directory/postgres-create.request.json"
status="$(api_request POST postgres.create "$private_directory/postgres-create.json" \
    "$private_directory/postgres-create.request.json")"
require_status "$status" 200 postgres.create
postgres_id="$(jq -er '.postgresId' "$private_directory/postgres-create.json")"

create_destination() {
    local name="$1" output="$2"
    jq -n --arg name "$name" --arg endpoint "http://$tripwire_name:9000" \
        --arg access "backup-access-$run_id" --arg secret "backup-secret-$run_id" '
        {
            name:$name,provider:"Other",accessKey:$access,secretAccessKey:$secret,
            bucket:"backup-contract",region:"us-east-1",endpoint:$endpoint,
            additionalFlags:[]
        }
    ' >"$private_directory/destination-create.$output.request.json"
    status="$(api_request POST destination.create \
        "$private_directory/destination-create.$output.json" \
        "$private_directory/destination-create.$output.request.json")"
    require_status "$status" 200 "destination.create $output"
    jq -er '.destinationId' "$private_directory/destination-create.$output.json"
}

destination_id="$(create_destination "$destination_name" initial)"
updated_destination_id="$(create_destination "$updated_destination_name" updated)"

cd "$repository_root"
DOKPLOY_BACKUP_LIVE_TEST=1 \
DOKPLOY_URL="$base_url" \
DOKPLOY_API_KEY="$(<"$api_key_file")" \
DOKPLOY_BACKUP_POSTGRES_ID="$postgres_id" \
DOKPLOY_BACKUP_DESTINATION_ID="$destination_id" \
DOKPLOY_BACKUP_UPDATED_DESTINATION_ID="$updated_destination_id" \
DOKPLOY_BACKUP_HANDSHAKE_DIR="$handshake_directory" \
    cargo test --package dokploy-sdk --test live_backup --locked -- \
        --ignored --exact live_backup_adapter_converges_without_execution_or_deployment \
        >"$private_directory/live-test.log" 2>&1 &
test_pid="$!"

ready_directory=""
for _ in {1..120}; do
    ready_directory="$(find "$handshake_directory" -maxdepth 1 -type d -name 'ready-*' -print -quit)"
    if [[ -n "$ready_directory" ]]; then break; fi
    if ! kill -0 "$test_pid" 2>/dev/null; then
        wait "$test_pid" || true
        test_pid=""
        echo "The live Backup SDK test exited before validation; see sanitized retained evidence." >&2
        exit 1
    fi
    sleep 1
done
if [[ -z "$ready_directory" ]]; then
    echo "The live Backup SDK test did not reach its validation handshake." >&2
    exit 1
fi
backup_id="${ready_directory##*/ready-}"

status="$(api_request GET "postgres.one?postgresId=$(urlencode "$postgres_id")" \
    "$private_directory/postgres-one.updated.json")"
require_status "$status" 200 "postgres.one during disabled Backup"
if ! jq -e --arg backupId "$backup_id" --arg destinationId "$updated_destination_id" '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and ([.backups[] | select(
        .backupId == $backupId
        and .destinationId == $destinationId
        and .enabled == false
        and .schedule == "*/1 * * * *"
        and .prefix == "/live-updated/"
        and .database == "postgres-updated"
        and .keepLatestCount == 3
        and .includeEncryptionKey == false
        and .backupType == "database"
        and .databaseType == "postgres"
        and (.deployments | length) == 0
    )] | length) == 1
' "$private_directory/postgres-one.updated.json" >/dev/null; then
    echo "The disabled Backup was not persisted safely or showed execution activity." >&2
    exit 1
fi
if docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_HIT; then
    echo "The disabled Backup contacted the local tripwire." >&2
    exit 1
fi

mkdir "$handshake_directory/continue"
wait "$test_pid"
test_pid=""

status="$(api_request GET "postgres.one?postgresId=$(urlencode "$postgres_id")" \
    "$private_directory/postgres-one.after.json")"
require_status "$status" 200 "postgres.one after Backup delete"
if ! jq -e '.applicationStatus == "idle" and (.deployments | length) == 0 and (.backups | length) == 0' \
    "$private_directory/postgres-one.after.json" >/dev/null; then
    echo "The live Backup test retained state or deployed its target." >&2
    exit 1
fi
if docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_HIT; then
    echo "The live Backup lifecycle contacted the local tripwire." >&2
    exit 1
fi

jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
    >"$private_directory/project-remove.request.json"
status="$(api_request POST project.remove "$private_directory/project-remove.json" \
    "$private_directory/project-remove.request.json")"
require_status "$status" 200 project.remove
project_id=""

remove_destination "$destination_id" initial
remove_destination "$updated_destination_id" updated
destination_id=""
updated_destination_id=""

docker rm -f "$tripwire_name" >/dev/null
cleanup_confirmed=true
test_succeeded=true

echo "Live disabled Backup SDK lifecycle passed without deployment, execution, or S3 traffic."
