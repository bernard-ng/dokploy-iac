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
    echo "Run scripts/integration/up.sh before capturing the Compose contract." >&2
    exit 1
fi

umask 077
mkdir -p "$state_directory"
chmod 700 "$state_directory"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/compose-contract-$run_id"
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

project_id=""
environment_id=""
compose_id=""
compose_name="Compose Contract Test"
mutation_attempted=false
cleanup_confirmed=false
capture_succeeded=false

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

recover_compose_id() {
    local recovery_status
    local match_count

    if [[ -n "$compose_id" || -z "$environment_id" ]]; then
        return
    fi

    recovery_status="$(api_request \
        GET \
        "compose.search?environmentId=$(urlencode "$environment_id")&limit=100&offset=0" \
        "$workspace/compose-search.recovery.json")" || recovery_status=""
    if [[ "$recovery_status" != "200" ]]; then
        return
    fi

    match_count="$(jq --arg environmentId "$environment_id" --arg name "$compose_name" \
        '[.items[]? | select(.environmentId == $environmentId and .name == $name)] | length' \
        "$workspace/compose-search.recovery.json")" || match_count=""
    if [[ "$match_count" == "1" ]]; then
        compose_id="$(jq -er --arg environmentId "$environment_id" --arg name "$compose_name" \
            '.items[] | select(.environmentId == $environmentId and .name == $name) | .composeId' \
            "$workspace/compose-search.recovery.json")" || compose_id=""
    elif [[ -n "$match_count" && "$match_count" != "0" ]]; then
        echo "Cleanup found multiple matching Compose records; refusing ambiguous deletion." >&2
    fi
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/compose-contract-*) ;;
        *)
            echo "Refusing to delete an unexpected Compose capture workspace." >&2
            return 1
            ;;
    esac

    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?"
    local cleanup_status

    trap - EXIT INT TERM
    set +e

    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        recover_compose_id

        if [[ -n "$compose_id" ]]; then
            jq -n --arg composeId "$compose_id" \
                '{composeId:$composeId,deleteVolumes:false}' \
                >"$workspace/compose-delete.cleanup.request.json"
            cleanup_status="$(api_request \
                POST \
                "compose.delete" \
                "$workspace/compose-delete.cleanup.json" \
                "$workspace/compose-delete.cleanup.request.json")"
            if [[ "$cleanup_status" != "200" ]]; then
                echo "Compose cleanup returned HTTP $cleanup_status; inspect the private workspace." >&2
                exit_code=1
            fi
        fi

        if [[ -n "$project_id" ]]; then
            jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
                >"$workspace/project-remove.cleanup.request.json"
            cleanup_status="$(api_request \
                POST \
                "project.remove" \
                "$workspace/project-remove.cleanup.json" \
                "$workspace/project-remove.cleanup.request.json")"
            if [[ "$cleanup_status" != "200" ]]; then
                echo "Project cleanup returned HTTP $cleanup_status; inspect the private workspace." >&2
                exit_code=1
            fi
        fi
    fi

    if [[ "$capture_succeeded" == true ]]; then
        discard_private_workspace || exit_code=1
    else
        echo "Compose capture evidence remains in $workspace" >&2
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

project_name="compose-sdk-contract-$run_id"
jq -n \
    --arg name "$project_name" \
    '{name:$name,description:"Disposable Compose SDK contract"}' \
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

preflight_status="$(api_request \
    GET \
    "compose.search?environmentId=$(urlencode "$environment_id")&limit=100&offset=0" \
    "$workspace/compose-search.preflight.json")"
require_status "$preflight_status" "200" "compose.search before create"
if ! jq -e '.items == [] and .total == 0' "$workspace/compose-search.preflight.json" >/dev/null; then
    echo "The disposable environment was not empty before Compose creation." >&2
    exit 1
fi

compose_file=$'services:\n  placeholder:\n    image: busybox:1.36.1\n    command: ["sh", "-c", "sleep infinity"]\n'
jq -n \
    --arg environmentId "$environment_id" \
    --arg appName "compose-contract-$run_id" \
    --arg composeFile "$compose_file" \
    '{
        name:"Compose Contract Test",
        description:"Disposable Compose SDK contract",
        environmentId:$environmentId,
        composeType:"docker-compose",
        appName:$appName,
        serverId:null,
        composeFile:$composeFile,
        sourceType:"raw"
    }' \
    >"$workspace/compose-create.request.json"
create_status="$(api_request \
    POST \
    "compose.create" \
    "$workspace/compose-create.json" \
    "$workspace/compose-create.request.json")"
require_status "$create_status" "200" "compose.create"
compose_id="$(jq -er --arg environmentId "$environment_id" '
    select(
        .composeId != null
        and .environmentId == $environmentId
        and .name == "Compose Contract Test"
        and .composeStatus == "idle"
    ) | .composeId
' "$workspace/compose-create.json")"
encoded_compose_id="$(urlencode "$compose_id")"

one_status="$(api_request \
    GET \
    "compose.one?composeId=$encoded_compose_id" \
    "$workspace/compose-one.created.json")"
require_status "$one_status" "200" "compose.one after create"
if ! jq -e --arg id "$compose_id" --arg environmentId "$environment_id" \
    '.composeId == $id and .environmentId == $environmentId
        and .name == "Compose Contract Test" and .sourceType == "raw"
        and .composeType == "docker-compose" and .composeStatus == "idle"
        and .serverId == null and (.deployments | length) == 0' \
    "$workspace/compose-one.created.json" >/dev/null
then
    echo "compose.one did not preserve the new undeployed Compose identity." >&2
    exit 1
fi

search_status="$(api_request \
    GET \
    "compose.search?environmentId=$(urlencode "$environment_id")&limit=100&offset=0" \
    "$workspace/compose-search.created.json")"
require_status "$search_status" "200" "compose.search after create"
if ! jq -e --arg id "$compose_id" --arg environmentId "$environment_id" '
    .total == 1 and (.items | length) == 1
    and .items[0].composeId == $id
    and .items[0].environmentId == $environmentId
' "$workspace/compose-search.created.json" >/dev/null; then
    echo "compose.search did not return one exact created identity." >&2
    exit 1
fi

updated_compose_file=$'services:\n  placeholder:\n    image: busybox:1.36.1\n    command: ["sh", "-c", "sleep 86400"]\n'
jq -n \
    --arg composeId "$compose_id" \
    --arg composeFile "$updated_compose_file" \
    '{
        composeId:$composeId,
        name:"Compose Contract Updated",
        description:"Updated Compose SDK contract",
        composeFile:$composeFile
    }' \
    >"$workspace/compose-update.request.json"
update_status="$(api_request \
    POST \
    "compose.update" \
    "$workspace/compose-update.json" \
    "$workspace/compose-update.request.json")"
require_status "$update_status" "200" "compose.update"
compose_name="Compose Contract Updated"

updated_one_status="$(api_request \
    GET \
    "compose.one?composeId=$encoded_compose_id" \
    "$workspace/compose-one.updated.json")"
require_status "$updated_one_status" "200" "compose.one after update"
if ! jq -e --arg composeFile "$updated_compose_file" '
    .name == "Compose Contract Updated"
    and .description == "Updated Compose SDK contract"
    and .composeFile == $composeFile
    and .composeStatus == "idle"
    and (.deployments | length) == 0
' "$workspace/compose-one.updated.json" >/dev/null; then
    echo "Compose owned-field update did not persist without deployment." >&2
    exit 1
fi

jq -n --arg composeId "$compose_id" \
    '{composeId:$composeId,deleteVolumes:false}' \
    >"$workspace/compose-delete.request.json"
delete_status="$(api_request \
    POST \
    "compose.delete" \
    "$workspace/compose-delete.json" \
    "$workspace/compose-delete.request.json")"
require_status "$delete_status" "200" "compose.delete"
compose_id=""

deleted_one_status="$(api_request \
    GET \
    "compose.one?composeId=$encoded_compose_id" \
    "$workspace/compose-one.deleted.json")"
require_status "$deleted_one_status" "404" "compose.one after delete"

deleted_search_status="$(api_request \
    GET \
    "compose.search?environmentId=$(urlencode "$environment_id")&limit=100&offset=0" \
    "$workspace/compose-search.deleted.json")"
require_status "$deleted_search_status" "200" "compose.search after delete"
if ! jq -e '.items == [] and .total == 0' "$workspace/compose-search.deleted.json" >/dev/null; then
    echo "compose.search still contains the deleted Compose record." >&2
    exit 1
fi

deleted_project_status="$(api_request \
    GET \
    "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.compose-deleted.json")"
require_status "$deleted_project_status" "200" "project.one after Compose delete"
if ! jq -e '[.environments[]?.compose[]?] | length == 0' \
    "$workspace/project-one.compose-deleted.json" >/dev/null
then
    echo "Project topology still contains the deleted Compose record." >&2
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
project_id=""
cleanup_confirmed=true

declare -a fixture_sources=(
    "compose-create"
    "compose-one.created"
    "compose-search.created"
    "compose-update"
    "compose-one.updated"
    "compose-delete"
    "compose-one.deleted"
    "compose-search.deleted"
    "project-one.compose-deleted"
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
    '{
        capturedAt:$capturedAt,
        role:$role,
        version:$version,
        image:$image,
        sanitized:true,
        deployed:false,
        createIdentity:{
            direct:true,
            parentVerified:true,
            createStatus:$createStatus
        },
        update:{
            status:$updateStatus,
            composeFilePersisted:true
        },
        deleteStatus:$deleteStatus,
        cleanupEvidence:{
            oneStatus:$oneStatus,
            searchEmpty:true,
            projectOneAbsent:true
        }
    }' \
    >"$publish_directory/compose-contract.metadata.json"

mkdir -p "$fixture_directory"
for fixture in "$publish_directory"/*.json; do
    install -m 0644 "$fixture" "$fixture_directory/$(basename "$fixture")"
done

"$script_directory/check-fixtures.sh"
capture_succeeded=true

echo "Captured the sanitized Compose contract without deploying it."
