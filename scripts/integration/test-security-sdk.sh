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
    echo "Run scripts/integration/up.sh before the live Security SDK test." >&2
    exit 1
fi

umask 077
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/security-sdk-test-$run_id"
mkdir -p "$workspace"
chmod 700 "$workspace"

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

project_name="security-sdk-live-$run_id"
project_id=""
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
        --silent --show-error --output "$destination" --write-out '%{http_code}'
        --header "@$auth_header_file" --request "$method"
    )
    if [[ -n "$body_file" ]]; then
        curl_arguments+=(--header 'Content-Type: application/json' --data-binary "@$body_file")
    fi
    curl "${curl_arguments[@]}" "$base_url/api/$endpoint"
}

require_status() {
    if [[ "$1" != "$2" ]]; then
        echo "$3 returned HTTP $1; expected HTTP $2." >&2
        return 1
    fi
}

recover_project_id() {
    local status
    local count
    if [[ -n "$project_id" || "$mutation_attempted" != true ]]; then
        return
    fi
    status="$(api_request GET "project.all" "$workspace/project-all.recovery.json")" || status=""
    if [[ "$status" != "200" ]]; then
        return
    fi
    count="$(jq --arg name "$project_name" '[.[] | select(.name == $name)] | length' \
        "$workspace/project-all.recovery.json")" || count=""
    if [[ "$count" == "1" ]]; then
        project_id="$(jq -er --arg name "$project_name" \
            '.[] | select(.name == $name) | .projectId' \
            "$workspace/project-all.recovery.json")" || project_id=""
    elif [[ -n "$count" && "$count" != "0" ]]; then
        echo "Cleanup found multiple disposable project candidates." >&2
    fi
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/security-sdk-test-*) ;;
        *) echo "Refusing to delete an unexpected Security SDK workspace." >&2; return 1 ;;
    esac
    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?"
    local status
    local verify
    trap - EXIT INT TERM
    set +e
    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        recover_project_id
        if [[ -n "$project_id" ]]; then
            jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
                >"$workspace/project-remove.cleanup.request.json"
            status="$(api_request POST "project.remove" "$workspace/project-remove.cleanup.json" \
                "$workspace/project-remove.cleanup.request.json")"
            if [[ "$status" != "200" && "$status" != "404" ]]; then
                echo "Project cleanup returned HTTP $status; inspect the private workspace." >&2
                exit_code=1
            else
                verify="$(api_request GET "project.one?projectId=$(urlencode "$project_id")" \
                    "$workspace/project-one.cleanup.json")"
                if [[ "$verify" != "404" ]]; then
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
        echo "Security SDK test evidence remains in $workspace" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

version_status="$(api_request GET "settings.getDokployVersion" "$workspace/version.json")"
require_status "$version_status" "200" "settings.getDokployVersion"
if ! jq -e '. == "v0.30.6"' "$workspace/version.json" >/dev/null; then
    echo "The live Security SDK test requires Dokploy v0.30.6." >&2
    exit 1
fi

jq -n --arg name "$project_name" \
    '{name:$name,description:"Disposable live Security SDK test"}' \
    >"$workspace/project-create.request.json"
mutation_attempted=true
status="$(api_request POST "project.create" "$workspace/project-create.json" \
    "$workspace/project-create.request.json")"
require_status "$status" "200" "project.create"
project_id="$(jq -er '.project.projectId' "$workspace/project-create.json")"
environment_id="$(jq -er '.environment.environmentId' "$workspace/project-create.json")"

jq -n --arg environmentId "$environment_id" \
    '{name:"Live Security SDK Target",environmentId:$environmentId}' \
    >"$workspace/application-create.request.json"
status="$(api_request POST "application.create" "$workspace/application-create.json" \
    "$workspace/application-create.request.json")"
require_status "$status" "200" "application.create"
application_id="$(jq -er \
    'select(.applicationId != null and .applicationStatus == "idle") | .applicationId' \
    "$workspace/application-create.json")"

cd "$repository_root"
DOKPLOY_SECURITY_LIVE_TEST=1 \
DOKPLOY_URL="$base_url" \
DOKPLOY_API_KEY="$(<"$api_key_file")" \
DOKPLOY_SECURITY_APPLICATION_ID="$application_id" \
    cargo test --package dokploy-sdk --test live_security --locked -- \
        --ignored --exact live_security_adapter_converges_without_deploying_its_application

status="$(api_request GET "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.after.json")"
require_status "$status" "200" "application.one after SDK test"
if ! jq -e '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.security | length) == 0
' "$workspace/application-one.after.json" >/dev/null; then
    echo "The live SDK test deployed or retained Security state." >&2
    exit 1
fi

jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
    >"$workspace/project-remove.request.json"
status="$(api_request POST "project.remove" "$workspace/project-remove.json" \
    "$workspace/project-remove.request.json")"
require_status "$status" "200" "project.remove"
status="$(api_request GET "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.after.json")"
require_status "$status" "404" "project.one after cleanup"
project_id=""
cleanup_confirmed=true
test_succeeded=true

echo "Live Security SDK create/read/update/delete passed with complete cleanup."
