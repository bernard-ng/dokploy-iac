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
    echo "Run scripts/integration/up.sh before capturing the Redirect contract." >&2
    exit 1
fi

umask 077
mkdir -p "$state_directory"
chmod 700 "$state_directory"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/redirect-contract-$run_id"
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

project_name="redirect-sdk-contract-$run_id"
project_id=""
environment_id=""
application_id=""
redirect_id=""
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
        "$state_directory"/redirect-contract-*) ;;
        *)
            echo "Refusing to delete an unexpected Redirect capture workspace." >&2
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
        echo "Redirect capture evidence remains in $workspace" >&2
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

jq -n \
    --arg name "$project_name" \
    '{name:$name,description:"Disposable Redirect SDK contract"}' \
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
    '{name:"Redirect Contract Application",environmentId:$environmentId}' \
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
if ! jq -e '.applicationStatus == "idle" and (.deployments | length) == 0 and (.redirects | length) == 0' \
    "$workspace/application-one.preflight.json" >/dev/null
then
    echo "The disposable application was deployed or had Redirects before capture." >&2
    exit 1
fi

jq -n --arg applicationId "$application_id" '
    {
        applicationId:$applicationId,
        regex:"^/legacy/(.*)$",
        replacement:"/current/$1",
        permanent:false
    }
' >"$workspace/redirect-create.request.json"
create_status="$(api_request \
    POST \
    "redirects.create" \
    "$workspace/redirect-create.json" \
    "$workspace/redirect-create.request.json")"
require_status "$create_status" "200" "redirects.create"
if ! jq -e '. == true' "$workspace/redirect-create.json" >/dev/null; then
    echo "redirects.create did not return its boolean acceptance contract." >&2
    exit 1
fi

created_parent_status="$(api_request \
    GET \
    "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.redirect-created.json")"
require_status "$created_parent_status" "200" "application.one after create"
match_count="$(jq '
    [.redirects[] | select(
        .regex == "^/legacy/(.*)$"
        and .replacement == "/current/$1"
        and .permanent == false
    )] | length
' "$workspace/application-one.redirect-created.json")"
if [[ "$match_count" != "1" ]]; then
    echo "application.one did not reveal exactly one matching created Redirect." >&2
    exit 1
fi
redirect_id="$(jq -er '
    .redirects[] | select(
        .regex == "^/legacy/(.*)$"
        and .replacement == "/current/$1"
        and .permanent == false
    ) | .redirectId
' "$workspace/application-one.redirect-created.json")"
encoded_redirect_id="$(urlencode "$redirect_id")"

created_one_status="$(api_request \
    GET \
    "redirects.one?redirectId=$encoded_redirect_id" \
    "$workspace/redirect-one.created.json")"
require_status "$created_one_status" "200" "redirects.one after create"
if ! jq -e --arg id "$redirect_id" --arg applicationId "$application_id" '
    .redirectId == $id
    and .applicationId == $applicationId
    and .regex == "^/legacy/(.*)$"
    and .replacement == "/current/$1"
    and .permanent == false
' "$workspace/redirect-one.created.json" >/dev/null; then
    echo "redirects.one did not agree with the authoritative created Redirect." >&2
    exit 1
fi

jq -n --arg redirectId "$redirect_id" '
    {
        redirectId:$redirectId,
        regex:"^/old/(.*)$",
        replacement:"/new/$1",
        permanent:true
    }
' >"$workspace/redirect-update.request.json"
update_status="$(api_request \
    POST \
    "redirects.update" \
    "$workspace/redirect-update.json" \
    "$workspace/redirect-update.request.json")"
require_status "$update_status" "200" "redirects.update"

updated_one_status="$(api_request \
    GET \
    "redirects.one?redirectId=$encoded_redirect_id" \
    "$workspace/redirect-one.updated.json")"
require_status "$updated_one_status" "200" "redirects.one after update"
if ! jq -e --arg id "$redirect_id" --arg applicationId "$application_id" '
    .redirectId == $id
    and .applicationId == $applicationId
    and .regex == "^/old/(.*)$"
    and .replacement == "/new/$1"
    and .permanent == true
' "$workspace/redirect-one.updated.json" >/dev/null; then
    echo "The all-field Redirect update did not persist." >&2
    exit 1
fi

updated_parent_status="$(api_request \
    GET \
    "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.redirect-updated.json")"
require_status "$updated_parent_status" "200" "application.one after update"
if ! jq -e --arg id "$redirect_id" --arg applicationId "$application_id" '
    .applicationId == $applicationId
    and (.redirects | length) == 1
    and .redirects[0].redirectId == $id
    and .redirects[0].applicationId == $applicationId
    and .redirects[0].regex == "^/old/(.*)$"
    and .redirects[0].replacement == "/new/$1"
    and .redirects[0].permanent == true
' "$workspace/application-one.redirect-updated.json" >/dev/null; then
    echo "application.one did not preserve the updated Redirect contract." >&2
    exit 1
fi

jq -n --arg redirectId "$redirect_id" '{redirectId:$redirectId}' \
    >"$workspace/redirect-delete.request.json"
delete_status="$(api_request \
    POST \
    "redirects.delete" \
    "$workspace/redirect-delete.json" \
    "$workspace/redirect-delete.request.json")"
require_status "$delete_status" "200" "redirects.delete"
redirect_id=""

deleted_one_status="$(api_request \
    GET \
    "redirects.one?redirectId=$encoded_redirect_id" \
    "$workspace/redirect-one.deleted.json")"
require_status "$deleted_one_status" "404" "redirects.one after delete"

removed_parent_status="$(api_request \
    GET \
    "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.redirect-deleted.json")"
require_status "$removed_parent_status" "200" "application.one after delete"
if ! jq -e '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.redirects | length) == 0
' "$workspace/application-one.redirect-deleted.json" >/dev/null; then
    echo "The undeployed application retained Redirect or deployment state." >&2
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
    "$workspace/project-one.redirect-deleted.json")"
require_status "$removed_project_status" "404" "project.one after cleanup"
project_id=""
cleanup_confirmed=true

declare -a fixture_sources=(
    "redirect-create"
    "application-one.redirect-created"
    "redirect-one.created"
    "redirect-update"
    "redirect-one.updated"
    "application-one.redirect-updated"
    "redirect-delete"
    "redirect-one.deleted"
    "application-one.redirect-deleted"
    "project-one.redirect-deleted"
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
        collisionKey:"application+regex",
        createIdentity:{setDifference:true,parentVerified:true,createStatus:$createStatus},
        update:{status:$updateStatus,allFieldsPersisted:true,parentVerified:true},
        deleteStatus:$deleteStatus,
        cleanupEvidence:{
            oneStatus:$oneStatus,
            applicationRedirectsEmpty:true,
            projectOneStatus:$projectOneStatus
        }
    }
' >"$publish_directory/redirect-contract.metadata.json"

mkdir -p "$fixture_directory"
for fixture in "$publish_directory"/*.json; do
    cp "$fixture" "$fixture_directory/$(basename "$fixture")"
done

"$script_directory/check-fixtures.sh"
capture_succeeded=true

echo "Captured the sanitized Redirect contract without deploying the application."
