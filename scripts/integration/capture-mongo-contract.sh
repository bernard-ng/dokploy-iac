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
    echo "Run scripts/integration/up.sh before capturing the MongoDB contract." >&2
    exit 1
fi

umask 077
mkdir -p "$state_directory"
chmod 700 "$state_directory"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/mongo-contract-$run_id"
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
mongo_id=""
mongo_name="MongoDB Contract Test"
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

recover_mongo_id() {
    local encoded_environment_id
    local recovery_status
    local match_count

    if [[ -n "$mongo_id" || -z "$environment_id" ]]; then
        return
    fi

    encoded_environment_id="$(urlencode "$environment_id")"
    recovery_status="$(api_request \
        GET \
        "mongo.search?environmentId=$encoded_environment_id&limit=100&offset=0" \
        "$workspace/mongo-search.recovery.json")" || recovery_status=""

    if [[ "$recovery_status" != "200" ]]; then
        return
    fi

    match_count="$(jq --arg name "$mongo_name" \
        '[.items[]? | select(.name == $name)] | length' \
        "$workspace/mongo-search.recovery.json")" || match_count=""
    if [[ "$match_count" == "1" ]]; then
        mongo_id="$(jq -er --arg name "$mongo_name" \
            '.items[] | select(.name == $name) | .mongoId' \
            "$workspace/mongo-search.recovery.json")" || mongo_id=""
    elif [[ -n "$match_count" && "$match_count" != "0" ]]; then
        echo "Cleanup found multiple matching MongoDB databases; refusing ambiguous deletion." >&2
    fi
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/mongo-contract-*) ;;
        *)
            echo "Refusing to delete an unexpected MongoDB capture workspace." >&2
            return 1
            ;;
    esac

    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?"
    local cleanup_status
    local cleanup_body="$workspace/mongo-remove.cleanup.request.json"
    local project_cleanup_body="$workspace/project-remove.cleanup.request.json"

    trap - EXIT INT TERM
    set +e

    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        recover_mongo_id

        if [[ -n "$mongo_id" ]]; then
            jq -n --arg mongoId "$mongo_id" '{mongoId:$mongoId}' >"$cleanup_body"
            cleanup_status="$(api_request \
                POST \
                "mongo.remove" \
                "$workspace/mongo-remove.cleanup.json" \
                "$cleanup_body")"
            if [[ "$cleanup_status" != "200" ]]; then
                echo "MongoDB cleanup returned HTTP $cleanup_status; inspect the private workspace." >&2
                exit_code=1
            fi
        fi

        if [[ -n "$project_id" ]]; then
            jq -n --arg projectId "$project_id" '{projectId:$projectId}' >"$project_cleanup_body"
            cleanup_status="$(api_request \
                POST \
                "project.remove" \
                "$workspace/project-remove.cleanup.json" \
                "$project_cleanup_body")"
            if [[ "$cleanup_status" != "200" ]]; then
                echo "Project cleanup returned HTTP $cleanup_status; inspect the private workspace." >&2
                exit_code=1
            fi
        fi
    fi

    if [[ "$capture_succeeded" == true ]]; then
        discard_private_workspace || exit_code=1
    else
        echo "MongoDB capture evidence remains in $workspace" >&2
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

project_name="mongodb-sdk-contract-$run_id"
jq -n \
    --arg name "$project_name" \
    '{name:$name,description:"Disposable MongoDB SDK contract"}' \
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
    '{
        name:"MongoDB Contract Test",
        environmentId:$environmentId,
        databaseUser:"contract",
        databasePassword:$password,
        replicaSets:false,
        description:"Disposable MongoDB SDK contract",
        dockerImage:"mongo:8"
    }' \
    >"$workspace/mongo-create.request.json"
mongo_status="$(api_request \
    POST \
    "mongo.create" \
    "$workspace/mongo-create.json" \
    "$workspace/mongo-create.request.json")"
require_status "$mongo_status" "200" "mongo.create"
mongo_id="$(jq -er '.mongoId' "$workspace/mongo-create.json")"
encoded_mongo_id="$(urlencode "$mongo_id")"
encoded_environment_id="$(urlencode "$environment_id")"

one_status="$(api_request \
    GET \
    "mongo.one?mongoId=$encoded_mongo_id" \
    "$workspace/mongo-one.created.json")"
require_status "$one_status" "200" "mongo.one after create"
search_status="$(api_request \
    GET \
    "mongo.search?environmentId=$encoded_environment_id&limit=100&offset=0" \
    "$workspace/mongo-search.created.json")"
require_status "$search_status" "200" "mongo.search after create"

jq -n \
    --arg mongoId "$mongo_id" \
    '{
        mongoId:$mongoId,
        databaseUser:"contract_next",
        replicaSets:true,
        description:"Updated MongoDB SDK contract"
    }' \
    >"$workspace/mongo-update.request.json"
update_status="$(api_request \
    POST \
    "mongo.update" \
    "$workspace/mongo-update.json" \
    "$workspace/mongo-update.request.json")"
require_status "$update_status" "200" "mongo.update"

next_password="$(openssl rand -hex 18)"
jq -n \
    --arg mongoId "$mongo_id" \
    --arg password "$next_password" \
    '{mongoId:$mongoId,password:$password}' \
    >"$workspace/mongo-change-password.request.json"
password_status="$(api_request \
    POST \
    "mongo.changePassword" \
    "$workspace/mongo-change-password.idle.json" \
    "$workspace/mongo-change-password.request.json")"
require_status "$password_status" "400" "mongo.changePassword for an idle database"

one_status="$(api_request \
    GET \
    "mongo.one?mongoId=$encoded_mongo_id" \
    "$workspace/mongo-one.updated.json")"
require_status "$one_status" "200" "mongo.one after update"

jq -n --arg mongoId "$mongo_id" '{mongoId:$mongoId}' \
    >"$workspace/mongo-remove.request.json"
remove_status="$(api_request \
    POST \
    "mongo.remove" \
    "$workspace/mongo-remove.json" \
    "$workspace/mongo-remove.request.json")"
require_status "$remove_status" "200" "mongo.remove"
mongo_id=""

removed_one_status="$(api_request \
    GET \
    "mongo.one?mongoId=$encoded_mongo_id" \
    "$workspace/mongo-one.removed.json")"
require_status "$removed_one_status" "404" "mongo.one after remove"
removed_search_status="$(api_request \
    GET \
    "mongo.search?environmentId=$encoded_environment_id&limit=100&offset=0" \
    "$workspace/mongo-search.removed.json")"
require_status "$removed_search_status" "200" "mongo.search after remove"
if ! jq -e '.items == [] and .total == 0' "$workspace/mongo-search.removed.json" >/dev/null; then
    echo "MongoDB search did not prove authoritative cleanup." >&2
    exit 1
fi

project_one_status="$(api_request \
    GET \
    "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.mongo-removed.json")"
require_status "$project_one_status" "200" "project.one after MongoDB remove"
if ! jq -e '[.environments[]?.mongo[]?] | length == 0' \
    "$workspace/project-one.mongo-removed.json" >/dev/null
then
    echo "Project topology still contains the removed MongoDB database." >&2
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
    "mongo-create"
    "mongo-one.created"
    "mongo-search.created"
    "mongo-update"
    "mongo-change-password.idle"
    "mongo-one.updated"
    "mongo-remove"
    "mongo-one.removed"
    "mongo-search.removed"
    "project-one.mongo-removed"
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
    --argjson passwordStatus "$password_status" \
    --argjson oneStatus "$removed_one_status" \
    '{
        capturedAt:$capturedAt,
        role:$role,
        version:$version,
        image:$image,
        sanitized:true,
        deployed:false,
        idleCredentialChangeStatus:$passwordStatus,
        cleanupEvidence:{
            oneStatus:$oneStatus,
            searchEmpty:true,
            projectOneAbsent:true
        }
    }' \
    >"$publish_directory/mongo-contract.metadata.json"

mkdir -p "$fixture_directory"
for fixture in "$publish_directory"/*.json; do
    install -m 0644 "$fixture" "$fixture_directory/$(basename "$fixture")"
done

"$script_directory/check-fixtures.sh"
capture_succeeded=true

echo "Captured the sanitized MongoDB contract without deploying the database."
