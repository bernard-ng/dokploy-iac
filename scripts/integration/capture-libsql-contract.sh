#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
fixture_directory="$repository_root/fixtures/api/live/v0.30.6"
sanitizer="$script_directory/sanitize-fixture.jq"

if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before capturing the LibSQL contract." >&2
    exit 1
fi

umask 077
mkdir -p "$state_directory"
chmod 700 "$state_directory"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/libsql-contract-$run_id"
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
libsql_id=""
libsql_name="LibSQL Contract Test"
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

recover_libsql_id() {
    local recovery_status
    local match_count

    if [[ -n "$libsql_id" || -z "$project_id" || -z "$environment_id" ]]; then
        return
    fi

    recovery_status="$(api_request \
        GET \
        "project.one?projectId=$(urlencode "$project_id")" \
        "$workspace/project-one.recovery.json")" || recovery_status=""
    if [[ "$recovery_status" != "200" ]]; then
        return
    fi

    match_count="$(jq --arg environmentId "$environment_id" --arg name "$libsql_name" \
        '[.environments[]? | select(.environmentId == $environmentId) | .libsql[]?
            | select(.name == $name)] | length' \
        "$workspace/project-one.recovery.json")" || match_count=""
    if [[ "$match_count" == "1" ]]; then
        libsql_id="$(jq -er --arg environmentId "$environment_id" --arg name "$libsql_name" \
            '.environments[] | select(.environmentId == $environmentId) | .libsql[]
                | select(.name == $name) | .libsqlId' \
            "$workspace/project-one.recovery.json")" || libsql_id=""
    elif [[ -n "$match_count" && "$match_count" != "0" ]]; then
        echo "Cleanup found multiple matching LibSQL databases; refusing ambiguous child deletion." >&2
    fi
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/libsql-contract-*) ;;
        *)
            echo "Refusing to delete an unexpected LibSQL capture workspace." >&2
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
        recover_libsql_id

        if [[ -n "$libsql_id" ]]; then
            jq -n --arg libsqlId "$libsql_id" '{libsqlId:$libsqlId}' \
                >"$workspace/libsql-remove.cleanup.request.json"
            cleanup_status="$(api_request \
                POST \
                "libsql.remove" \
                "$workspace/libsql-remove.cleanup.json" \
                "$workspace/libsql-remove.cleanup.request.json")"
            if [[ "$cleanup_status" != "200" ]]; then
                echo "LibSQL cleanup returned HTTP $cleanup_status; inspect the private workspace." >&2
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
        echo "LibSQL capture evidence remains in $workspace" >&2
    fi

    exit "$exit_code"
}
trap cleanup EXIT INT TERM

runtime_status="$(api_request GET "settings.getDokployVersion" "$workspace/version.json")"
require_status "$runtime_status" "200" "settings.getDokployVersion"
runtime_version="$(jq -er '.' "$workspace/version.json")"
if [[ "$runtime_version" != "v0.30.6" ]]; then
    echo "Expected Dokploy v0.30.6, received $runtime_version." >&2
    exit 1
fi

project_name="libsql-sdk-contract-$run_id"
jq -n \
    --arg name "$project_name" \
    '{name:$name,description:"Disposable LibSQL SDK contract"}' \
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

database_password="$(openssl rand -hex 18)"
jq -n \
    --arg environmentId "$environment_id" \
    --arg password "$database_password" \
    --arg appName "libsql-contract-$run_id" \
    '{
        name:"LibSQL Contract Test",
        appName:$appName,
        dockerImage:"ghcr.io/tursodatabase/libsql-server:v0.24.32",
        environmentId:$environmentId,
        description:"Disposable LibSQL SDK contract",
        databaseUser:"contract",
        databasePassword:$password,
        sqldNode:"primary",
        sqldPrimaryUrl:null,
        enableNamespaces:false,
        serverId:null
    }' \
    >"$workspace/libsql-create.request.json"
create_status="$(api_request \
    POST \
    "libsql.create" \
    "$workspace/libsql-create.json" \
    "$workspace/libsql-create.request.json")"
require_status "$create_status" "200" "libsql.create"
if ! jq -e '. == true' "$workspace/libsql-create.json" >/dev/null; then
    echo "libsql.create did not return the pinned boolean success contract." >&2
    exit 1
fi

project_one_status="$(api_request \
    GET \
    "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.libsql-created.json")"
require_status "$project_one_status" "200" "project.one after LibSQL create"
environment_count="$(jq --arg environmentId "$environment_id" \
    '[.environments[]? | select(.environmentId == $environmentId)] | length' \
    "$workspace/project-one.libsql-created.json")"
match_count="$(jq --arg environmentId "$environment_id" --arg name "$libsql_name" \
    '[.environments[]? | select(.environmentId == $environmentId) | .libsql[]?
        | select(.name == $name)] | length' \
    "$workspace/project-one.libsql-created.json")"
if [[ "$environment_count" != "1" || "$match_count" != "1" ]]; then
    echo "Project topology could not establish one authoritative LibSQL identity." >&2
    exit 1
fi
libsql_id="$(jq -er --arg environmentId "$environment_id" --arg name "$libsql_name" \
    '.environments[] | select(.environmentId == $environmentId) | .libsql[]
        | select(.name == $name) | .libsqlId' \
    "$workspace/project-one.libsql-created.json")"
encoded_libsql_id="$(urlencode "$libsql_id")"

one_status="$(api_request \
    GET \
    "libsql.one?libsqlId=$encoded_libsql_id" \
    "$workspace/libsql-one.created.json")"
require_status "$one_status" "200" "libsql.one after create"
if ! jq -e --arg id "$libsql_id" --arg environmentId "$environment_id" \
    '.libsqlId == $id and .environmentId == $environmentId
        and .name == "LibSQL Contract Test" and .applicationStatus == "idle"
        and .serverId == null' \
    "$workspace/libsql-one.created.json" >/dev/null
then
    echo "libsql.one did not agree with the discovered idle identity." >&2
    exit 1
fi

jq -n \
    --arg libsqlId "$libsql_id" \
    '{
        libsqlId:$libsqlId,
        databaseUser:"contract_next",
        description:"Updated LibSQL SDK contract"
    }' \
    >"$workspace/libsql-update.request.json"
update_status="$(api_request \
    POST \
    "libsql.update" \
    "$workspace/libsql-update.json" \
    "$workspace/libsql-update.request.json")"
require_status "$update_status" "200" "libsql.update metadata"

updated_status="$(api_request \
    GET \
    "libsql.one?libsqlId=$encoded_libsql_id" \
    "$workspace/libsql-one.updated.json")"
require_status "$updated_status" "200" "libsql.one after metadata update"
if ! jq -e '
    .databaseUser == "contract_next"
    and .description == "Updated LibSQL SDK contract"
' "$workspace/libsql-one.updated.json" >/dev/null; then
    echo "LibSQL metadata update did not persist." >&2
    exit 1
fi

next_password="$(openssl rand -hex 18)"
jq -n \
    --arg libsqlId "$libsql_id" \
    --arg password "$next_password" \
    '{libsqlId:$libsqlId,databasePassword:$password}' \
    >"$workspace/libsql-change-password.request.json"
password_status="$(api_request \
    POST \
    "libsql.update" \
    "$workspace/libsql-change-password.json" \
    "$workspace/libsql-change-password.request.json")"
require_status "$password_status" "200" "libsql.update databasePassword"

password_one_status="$(api_request \
    GET \
    "libsql.one?libsqlId=$encoded_libsql_id" \
    "$workspace/libsql-one.password-updated.json")"
require_status "$password_one_status" "200" "libsql.one after credential update"
if ! jq -e --arg password "$next_password" '.databasePassword == $password' \
    "$workspace/libsql-one.password-updated.json" >/dev/null
then
    echo "LibSQL authentication credential update did not persist." >&2
    exit 1
fi

jq -n --arg libsqlId "$libsql_id" '{libsqlId:$libsqlId}' \
    >"$workspace/libsql-remove.request.json"
remove_status="$(api_request \
    POST \
    "libsql.remove" \
    "$workspace/libsql-remove.json" \
    "$workspace/libsql-remove.request.json")"
require_status "$remove_status" "200" "libsql.remove"
libsql_id=""

removed_one_status="$(api_request \
    GET \
    "libsql.one?libsqlId=$encoded_libsql_id" \
    "$workspace/libsql-one.removed.json")"
require_status "$removed_one_status" "404" "libsql.one after remove"

removed_project_status="$(api_request \
    GET \
    "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.libsql-removed.json")"
require_status "$removed_project_status" "200" "project.one after LibSQL remove"
if ! jq -e --arg environmentId "$environment_id" --arg name "$libsql_name" \
    '[.environments[]? | select(.environmentId == $environmentId) | .libsql[]?
        | select(.name == $name)] | length == 0' \
    "$workspace/project-one.libsql-removed.json" >/dev/null
then
    echo "Project topology still contains the removed LibSQL database." >&2
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
    "libsql-create"
    "project-one.libsql-created"
    "libsql-one.created"
    "libsql-update"
    "libsql-one.updated"
    "libsql-change-password"
    "libsql-one.password-updated"
    "libsql-remove"
    "libsql-one.removed"
    "project-one.libsql-removed"
)

for fixture_name in "${fixture_sources[@]}"; do
    jq --sort-keys --indent 2 \
        --from-file "$sanitizer" \
        "$workspace/$fixture_name.json" \
        >"$publish_directory/$fixture_name.owner.json"
done

jq -n --sort-keys --indent 2 \
    --arg capturedAt "2026-09-30" \
    --arg role "owner" \
    --arg version "$runtime_version" \
    --arg image "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8" \
    --argjson createStatus "$create_status" \
    --argjson passwordStatus "$password_status" \
    --argjson oneStatus "$removed_one_status" \
    '{
        capturedAt:$capturedAt,
        role:$role,
        version:$version,
        image:$image,
        sanitized:true,
        deployed:false,
        discovery:{
            source:"project.one",
            createResponse:"boolean",
            exactEnvironment:true,
            preflightAbsenceRequired:true,
            uniqueNameRequired:true,
            createStatus:$createStatus
        },
        credentialUpdateStatus:$passwordStatus,
        cleanupEvidence:{
            oneStatus:$oneStatus,
            projectOneAbsent:true
        }
    }' \
    >"$publish_directory/libsql-contract.metadata.json"

mkdir -p "$fixture_directory"
for fixture in "$publish_directory"/*.json; do
    cp "$fixture" "$fixture_directory/$(basename "$fixture")"
done

"$script_directory/check-fixtures.sh"
capture_succeeded=true

echo "Captured the sanitized LibSQL contract without deploying the database."
