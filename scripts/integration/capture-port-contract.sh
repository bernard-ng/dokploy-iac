#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
fixture_directory="$repository_root/fixtures/api/live/$dokploy_version"
sanitizer="$script_directory/sanitize-fixture.jq"

if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before capturing the Port contract." >&2
    exit 1
fi

umask 077
mkdir -p "$state_directory"
chmod 700 "$state_directory"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/port-contract-$run_id"
publish_directory="$workspace/publish"
mkdir -p "$publish_directory"
chmod 700 "$workspace" "$publish_directory"

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

project_name="port-sdk-contract-$run_id"
project_id=""
environment_id=""
application_id=""
port_id=""
mutation_attempted=false
cleanup_confirmed=false
capture_succeeded=false
publication_started=false
publication_complete=false
published_fixture_backup="$workspace/published-fixtures.backup"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

api_request() {
    local method="$1"
    local endpoint="$2"
    local destination="$3"
    local body_file="${4:-}"
    local curl_arguments=(
        --silent
        --show-error
        --output "$destination"
        --write-out '%{http_code}'
        --header "@$auth_header_file"
        --request "$method"
    )

    if [[ -n "$body_file" ]]; then
        curl_arguments+=(
            --header 'Content-Type: application/json'
            --data-binary "@$body_file"
        )
    fi

    curl "${curl_arguments[@]}" "$base_url/api/$endpoint"
}

require_status() {
    local actual="$1"
    local expected="$2"
    local operation="$3"

    if [[ "$actual" != "$expected" ]]; then
        echo "$operation returned HTTP $actual; expected HTTP $expected." >&2
        return 1
    fi
}

recover_project_id() {
    local recovery_status
    local match_count

    if [[ -n "$project_id" || "$mutation_attempted" != true ]]; then
        return
    fi

    recovery_status="$(api_request GET "project.all" "$workspace/project-all.recovery.json")" \
        || recovery_status=""
    if [[ "$recovery_status" != "200" ]]; then
        return
    fi

    match_count="$(jq --arg name "$project_name" \
        '[.[] | select(.name == $name)] | length' \
        "$workspace/project-all.recovery.json")" || match_count=""
    if [[ "$match_count" == "1" ]]; then
        project_id="$(jq -er --arg name "$project_name" \
            '.[] | select(.name == $name) | .projectId' \
            "$workspace/project-all.recovery.json")" || project_id=""
    elif [[ -n "$match_count" && "$match_count" != "0" ]]; then
        echo "Cleanup found multiple disposable project candidates; refusing ambiguous removal." >&2
    fi
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/port-contract-*) ;;
        *)
            echo "Refusing to delete an unexpected Port capture workspace." >&2
            return 1
            ;;
    esac

    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?"
    local cleanup_status
    local verify_status
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
                if ! mv "$published_fixture_backup" "$fixture_directory"; then
                    echo "Could not restore the previous tracked fixture directory." >&2
                    exit_code=1
                fi
            else
                echo "Could not restore the previous tracked fixture directory." >&2
                exit_code=1
            fi
        else
            echo "Fixture publication was interrupted without a recoverable backup." >&2
            exit_code=1
        fi
    fi

    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        recover_project_id
        if [[ -n "$project_id" ]]; then
            jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
                >"$workspace/project-remove.cleanup.request.json"
            cleanup_status="$(api_request \
                POST \
                "project.remove" \
                "$workspace/project-remove.cleanup.json" \
                "$workspace/project-remove.cleanup.request.json")"
            if [[ "$cleanup_status" != "200" && "$cleanup_status" != "404" ]]; then
                echo "Project cleanup returned HTTP $cleanup_status; inspect the private workspace." >&2
                exit_code=1
            else
                verify_status="$(api_request \
                    GET \
                    "project.one?projectId=$(urlencode "$project_id")" \
                    "$workspace/project-one.cleanup.json")"
                if [[ "$verify_status" != "404" ]]; then
                    echo "Project cleanup could not prove authoritative absence." >&2
                    exit_code=1
                fi
            fi
        else
            echo "Project cleanup could not recover the disposable identity." >&2
            exit_code=1
        fi
    fi

    if [[ "$capture_succeeded" == true ]]; then
        discard_private_workspace || exit_code=1
    else
        echo "Port capture evidence remains in $workspace" >&2
    fi

    exit "$exit_code"
}
trap cleanup EXIT INT TERM

runtime_status="$(api_request GET "settings.getDokployVersion" "$workspace/version.json")"
require_status "$runtime_status" "200" "settings.getDokployVersion"
runtime_version="$(jq -er '.' "$workspace/version.json")"
if [[ "$runtime_version" != "$dokploy_version" ]]; then
    echo "Expected Dokploy $dokploy_version, received $runtime_version." >&2
    exit 1
fi

jq -n \
    --arg name "$project_name" \
    '{name:$name,description:"Disposable Port SDK contract"}' \
    >"$workspace/project-create.request.json"
mutation_attempted=true
project_status="$(api_request \
    POST \
    "project.create" \
    "$workspace/project-create.json" \
    "$workspace/project-create.request.json")"
require_status "$project_status" "200" "project.create"
project_id="$(jq -er '.project.projectId' "$workspace/project-create.json")"
environment_id="$(jq -er '.environment.environmentId' "$workspace/project-create.json")"

jq -n \
    --arg environmentId "$environment_id" \
    '{name:"Port Contract Application",environmentId:$environmentId}' \
    >"$workspace/application-create.request.json"
application_status="$(api_request \
    POST \
    "application.create" \
    "$workspace/application-create.json" \
    "$workspace/application-create.request.json")"
require_status "$application_status" "200" "application.create"
application_id="$(jq -er '
    select(.applicationId != null and .applicationStatus == "idle") | .applicationId
' "$workspace/application-create.json")"

preflight_status="$(api_request \
    GET \
    "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.preflight.json")"
require_status "$preflight_status" "200" "application.one before create"
if ! jq -e '.applicationStatus == "idle" and (.deployments | length) == 0 and (.ports | length) == 0' \
    "$workspace/application-one.preflight.json" >/dev/null
then
    echo "The disposable application was deployed or had Ports before capture." >&2
    exit 1
fi

jq -n \
    --arg applicationId "$application_id" '
    {
        applicationId:$applicationId,
        publishedPort:18080,
        targetPort:8080,
        publishMode:"ingress",
        protocol:"tcp"
    }
' >"$workspace/port-create.request.json"
create_status="$(api_request \
    POST \
    "port.create" \
    "$workspace/port-create.json" \
    "$workspace/port-create.request.json")"
require_status "$create_status" "200" "port.create"
port_id="$(jq -er \
    --arg applicationId "$application_id" '
    select(
        .portId != null
        and .applicationId == $applicationId
        and .publishedPort == 18080
        and .targetPort == 8080
        and .publishMode == "ingress"
        and .protocol == "tcp"
    ) | .portId
' "$workspace/port-create.json")"
encoded_port_id="$(urlencode "$port_id")"

created_one_status="$(api_request \
    GET \
    "port.one?portId=$encoded_port_id" \
    "$workspace/port-one.created.json")"
require_status "$created_one_status" "200" "port.one after create"
if ! jq -e --arg id "$port_id" --arg applicationId "$application_id" '
    .portId == $id
    and .applicationId == $applicationId
    and .publishedPort == 18080
    and .targetPort == 8080
    and .publishMode == "ingress"
    and .protocol == "tcp"
' "$workspace/port-one.created.json" >/dev/null; then
    echo "port.one did not preserve the created Port contract." >&2
    exit 1
fi

created_parent_status="$(api_request \
    GET \
    "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.port-created.json")"
require_status "$created_parent_status" "200" "application.one after create"
if ! jq -e --arg id "$port_id" --arg applicationId "$application_id" '
    .applicationId == $applicationId
    and (.ports | length) == 1
    and .ports[0].portId == $id
    and .ports[0].applicationId == $applicationId
' "$workspace/application-one.port-created.json" >/dev/null; then
    echo "application.one did not authoritatively contain one created Port." >&2
    exit 1
fi

jq -n --arg portId "$port_id" '
    {
        portId:$portId,
        publishedPort:19090,
        targetPort:9090,
        publishMode:"host",
        protocol:"udp"
    }
' >"$workspace/port-update.request.json"
update_status="$(api_request \
    POST \
    "port.update" \
    "$workspace/port-update.json" \
    "$workspace/port-update.request.json")"
require_status "$update_status" "200" "port.update"

updated_one_status="$(api_request \
    GET \
    "port.one?portId=$encoded_port_id" \
    "$workspace/port-one.updated.json")"
require_status "$updated_one_status" "200" "port.one after update"
if ! jq -e --arg id "$port_id" --arg applicationId "$application_id" '
    .portId == $id
    and .applicationId == $applicationId
    and .publishedPort == 19090
    and .targetPort == 9090
    and .publishMode == "host"
    and .protocol == "udp"
' "$workspace/port-one.updated.json" >/dev/null; then
    echo "The all-field Port update did not persist." >&2
    exit 1
fi

updated_parent_status="$(api_request \
    GET \
    "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.port-updated.json")"
require_status "$updated_parent_status" "200" "application.one after update"
if ! jq -e --arg id "$port_id" '
    (.ports | length) == 1
    and .ports[0].portId == $id
    and .ports[0].publishedPort == 19090
    and .ports[0].targetPort == 9090
    and .ports[0].publishMode == "host"
    and .ports[0].protocol == "udp"
' "$workspace/application-one.port-updated.json" >/dev/null; then
    echo "application.one did not preserve the updated Port contract." >&2
    exit 1
fi

jq -n --arg portId "$port_id" '{portId:$portId}' \
    >"$workspace/port-delete.request.json"
delete_status="$(api_request \
    POST \
    "port.delete" \
    "$workspace/port-delete.json" \
    "$workspace/port-delete.request.json")"
require_status "$delete_status" "200" "port.delete"
port_id=""

deleted_one_status="$(api_request \
    GET \
    "port.one?portId=$encoded_port_id" \
    "$workspace/port-one.deleted.json")"
require_status "$deleted_one_status" "400" "port.one after delete"

removed_parent_status="$(api_request \
    GET \
    "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.port-deleted.json")"
require_status "$removed_parent_status" "200" "application.one after delete"
if ! jq -e '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.ports | length) == 0
' "$workspace/application-one.port-deleted.json" >/dev/null; then
    echo "The undeployed application retained Port or deployment state." >&2
    exit 1
fi

jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
    >"$workspace/project-remove.request.json"
project_remove_status="$(api_request \
    POST \
    "project.remove" \
    "$workspace/project-remove.json" \
    "$workspace/project-remove.request.json")"
require_status "$project_remove_status" "200" "project.remove"

removed_project_status="$(api_request \
    GET \
    "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.port-deleted.json")"
require_status "$removed_project_status" "404" "project.one after cleanup"
project_id=""
cleanup_confirmed=true

declare -a fixture_sources=(
    "port-create"
    "port-one.created"
    "application-one.port-created"
    "port-update"
    "port-one.updated"
    "application-one.port-updated"
    "port-delete"
    "port-one.deleted"
    "application-one.port-deleted"
    "project-one.port-deleted"
)

for fixture_name in "${fixture_sources[@]}"; do
    jq --sort-keys --indent 2 \
        --from-file "$sanitizer" \
        "$workspace/$fixture_name.json" \
        >"$publish_directory/$fixture_name.owner.json"
done

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" \
    --arg role "owner" \
    --arg version "$runtime_version" \
    --arg image "$dokploy_image" \
    --argjson createStatus "$create_status" \
    --argjson updateStatus "$update_status" \
    --argjson deleteStatus "$delete_status" \
    --argjson oneStatus "$deleted_one_status" \
    --argjson projectOneStatus "$removed_project_status" '
    {
        capturedAt:$capturedAt,
        role:$role,
        version:$version,
        image:$image,
        sanitized:true,
        deployed:false,
        createIdentity:{direct:true,parentVerified:true,createStatus:$createStatus},
        update:{status:$updateStatus,allFieldsPersisted:true,parentVerified:true},
        deleteStatus:$deleteStatus,
        cleanupEvidence:{
            oneStatus:$oneStatus,
            applicationPortsEmpty:true,
            projectOneStatus:$projectOneStatus
        }
    }
' >"$publish_directory/port-contract.metadata.json"

candidate_fixture_root="$workspace/candidate/api/live"
candidate_versioned_fixture_directory="$candidate_fixture_root/$dokploy_version"
mkdir -p "$candidate_versioned_fixture_directory"
cp -R "$fixture_directory/." "$candidate_versioned_fixture_directory/"
for fixture in "$publish_directory"/*.json; do
    cp "$fixture" "$candidate_versioned_fixture_directory/$(basename "$fixture")"
done

find "$candidate_fixture_root" -type d -exec chmod 755 {} +
find "$candidate_fixture_root" -type f -exec chmod 644 {} +

if grep -R -F -q -f "$api_key_file" "$candidate_fixture_root"; then
    echo "Sanitized Port fixtures contain the local API key." >&2
    exit 1
fi

DOKPLOY_FIXTURE_DIRECTORY="$candidate_fixture_root" \
    "$script_directory/check-fixtures.sh"

publication_started=true
mv "$fixture_directory" "$published_fixture_backup"
mv "$candidate_versioned_fixture_directory" "$fixture_directory"
publication_complete=true
capture_succeeded=true

echo "Captured the sanitized Port contract without deploying the application."
