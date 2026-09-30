#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"

if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before the live Mount SDK test." >&2
    exit 1
fi

umask 077
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/mount-sdk-test-$run_id"
mkdir -p "$workspace"
chmod 700 "$workspace"

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

project_name="mount-sdk-live-$run_id"
volume_name="mount-sdk-live-$run_id"
project_id=""
environment_id=""
application_id=""
mutation_attempted=false
cleanup_confirmed=false
test_succeeded=false

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
        "$state_directory"/mount-sdk-test-*) ;;
        *)
            echo "Refusing to delete an unexpected Mount SDK test workspace." >&2
            return 1
            ;;
    esac

    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?"
    local cleanup_status
    local verify_status

    trap - EXIT INT TERM
    set +e

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
            if [[ "$cleanup_status" != "200" ]]; then
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

    if [[ "$test_succeeded" == true ]]; then
        discard_private_workspace || exit_code=1
    else
        echo "Mount SDK test evidence remains in $workspace" >&2
    fi

    exit "$exit_code"
}
trap cleanup EXIT INT TERM

version_status="$(api_request GET "settings.getDokployVersion" "$workspace/version.json")"
require_status "$version_status" "200" "settings.getDokployVersion"
if ! jq -e '. == "v0.30.6"' "$workspace/version.json" >/dev/null; then
    echo "The live Mount SDK test requires Dokploy v0.30.6." >&2
    exit 1
fi

jq -n \
    --arg name "$project_name" \
    '{name:$name,description:"Disposable live Mount SDK test"}' \
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
    '{name:"Live Mount SDK Target",environmentId:$environmentId}' \
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

cd "$repository_root"
DOKPLOY_MOUNT_LIVE_TEST=1 \
DOKPLOY_URL="$base_url" \
DOKPLOY_API_KEY="$(<"$api_key_file")" \
DOKPLOY_MOUNT_APPLICATION_ID="$application_id" \
DOKPLOY_MOUNT_VOLUME_NAME="$volume_name" \
    cargo test --package dokploy-sdk --test live_mount --locked -- \
        --ignored --exact live_mount_adapter_converges_without_deploying_its_target

list_status="$(api_request \
    GET \
    "mounts.listByServiceId?serviceType=application&serviceId=$(urlencode "$application_id")" \
    "$workspace/mount-list.after.json")"
require_status "$list_status" "200" "mounts.listByServiceId after SDK test"
if ! jq -e '. == []' "$workspace/mount-list.after.json" >/dev/null; then
    echo "The live SDK test left a Mount on its disposable target." >&2
    exit 1
fi

application_one_status="$(api_request \
    GET \
    "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.after.json")"
require_status "$application_one_status" "200" "application.one after SDK test"
if ! jq -e '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.mounts | length) == 0
' "$workspace/application-one.after.json" >/dev/null; then
    echo "The live SDK test deployed or retained state on its target." >&2
    exit 1
fi

jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
    >"$workspace/project-remove.request.json"
remove_status="$(api_request \
    POST \
    "project.remove" \
    "$workspace/project-remove.json" \
    "$workspace/project-remove.request.json")"
require_status "$remove_status" "200" "project.remove"

project_one_status="$(api_request \
    GET \
    "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.after.json")"
require_status "$project_one_status" "404" "project.one after cleanup"
project_id=""
cleanup_confirmed=true
test_succeeded=true

echo "Live Mount SDK create/read/list/update/remove passed with complete cleanup."
