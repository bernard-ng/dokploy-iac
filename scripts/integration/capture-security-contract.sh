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
    echo "Run scripts/integration/up.sh before capturing the Security contract." >&2
    exit 1
fi

umask 077
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/security-contract-$run_id"
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

project_name="security-sdk-contract-$run_id"
project_id=""
application_id=""
security_id=""
mutation_attempted=false
cleanup_confirmed=false
capture_succeeded=false
created_password="security-created-$(openssl rand -hex 24)"
updated_password="security-updated-$(openssl rand -hex 24)"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

api_request() {
    local method="$1"
    local endpoint="$2"
    local destination="$3"
    local body_file="${4:-}"
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
        "$state_directory"/security-contract-*) ;;
        *) echo "Refusing to delete an unexpected Security capture workspace." >&2; return 1 ;;
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
    if [[ "$capture_succeeded" == true ]]; then
        discard_private_workspace || exit_code=1
    else
        echo "Security capture evidence remains in $workspace" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

status="$(api_request GET "settings.getDokployVersion" "$workspace/version.json")"
require_status "$status" "200" "settings.getDokployVersion"
runtime_version="$(jq -er '.' "$workspace/version.json")"
if [[ "$runtime_version" != "v0.30.6" ]]; then
    echo "Expected Dokploy v0.30.6, received $runtime_version." >&2
    exit 1
fi

jq -n --arg name "$project_name" \
    '{name:$name,description:"Disposable Security SDK contract"}' \
    >"$workspace/project-create.request.json"
mutation_attempted=true
status="$(api_request POST "project.create" "$workspace/project-create.json" \
    "$workspace/project-create.request.json")"
require_status "$status" "200" "project.create"
project_id="$(jq -er '.project.projectId' "$workspace/project-create.json")"
environment_id="$(jq -er '.environment.environmentId' "$workspace/project-create.json")"

jq -n --arg environmentId "$environment_id" \
    '{name:"Security Contract Application",environmentId:$environmentId}' \
    >"$workspace/application-create.request.json"
status="$(api_request POST "application.create" "$workspace/application-create.json" \
    "$workspace/application-create.request.json")"
require_status "$status" "200" "application.create"
application_id="$(jq -er \
    'select(.applicationId != null and .applicationStatus == "idle") | .applicationId' \
    "$workspace/application-create.json")"

status="$(api_request GET "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.preflight.json")"
require_status "$status" "200" "application.one before create"
if ! jq -e '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.security | length) == 0
' "$workspace/application-one.preflight.json" >/dev/null; then
    echo "The disposable application was deployed or had Security entries." >&2
    exit 1
fi

jq -n --arg applicationId "$application_id" --arg password "$created_password" \
    '{applicationId:$applicationId,username:"owner",password:$password}' \
    >"$workspace/security-create.request.json"
create_status="$(api_request POST "security.create" "$workspace/security-create.json" \
    "$workspace/security-create.request.json")"
require_status "$create_status" "200" "security.create"
if ! jq -e '. == true' "$workspace/security-create.json" >/dev/null; then
    echo "security.create did not return boolean acceptance." >&2
    exit 1
fi

status="$(api_request GET "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.security-created.json")"
require_status "$status" "200" "application.one after create"
if ! jq -e --arg applicationId "$application_id" --arg password "$created_password" '
    .applicationId == $applicationId
    and (.security | length) == 1
    and .security[0].applicationId == $applicationId
    and .security[0].username == "owner"
    and .security[0].password == $password
' "$workspace/application-one.security-created.json" >/dev/null; then
    echo "application.one did not reveal exactly one complete created Security entry." >&2
    exit 1
fi
security_id="$(jq -er '.security[0].securityId' \
    "$workspace/application-one.security-created.json")"
encoded_security_id="$(urlencode "$security_id")"

status="$(api_request GET "security.one?securityId=$encoded_security_id" \
    "$workspace/security-one.created.json")"
require_status "$status" "200" "security.one after create"
if ! jq -e --arg id "$security_id" --arg applicationId "$application_id" \
    --arg password "$created_password" '
    .securityId == $id
    and .applicationId == $applicationId
    and .username == "owner"
    and .password == $password
' "$workspace/security-one.created.json" >/dev/null; then
    echo "security.one did not agree with the created parent entry." >&2
    exit 1
fi

jq -n --arg securityId "$security_id" --arg password "$updated_password" \
    '{securityId:$securityId,username:"operator",password:$password}' \
    >"$workspace/security-update.request.json"
update_status="$(api_request POST "security.update" "$workspace/security-update.json" \
    "$workspace/security-update.request.json")"
require_status "$update_status" "200" "security.update"

status="$(api_request GET "security.one?securityId=$encoded_security_id" \
    "$workspace/security-one.updated.json")"
require_status "$status" "200" "security.one after update"
if ! jq -e --arg id "$security_id" --arg applicationId "$application_id" \
    --arg password "$updated_password" '
    .securityId == $id
    and .applicationId == $applicationId
    and .username == "operator"
    and .password == $password
' "$workspace/security-one.updated.json" >/dev/null; then
    echo "The complete Security update did not persist." >&2
    exit 1
fi

status="$(api_request GET "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.security-updated.json")"
require_status "$status" "200" "application.one after update"
if ! jq -e --arg id "$security_id" --arg applicationId "$application_id" \
    --arg password "$updated_password" '
    .applicationId == $applicationId
    and (.security | length) == 1
    and .security[0].securityId == $id
    and .security[0].applicationId == $applicationId
    and .security[0].username == "operator"
    and .security[0].password == $password
' "$workspace/application-one.security-updated.json" >/dev/null; then
    echo "application.one did not preserve the updated Security contract." >&2
    exit 1
fi

jq -n --arg securityId "$security_id" '{securityId:$securityId}' \
    >"$workspace/security-delete.request.json"
delete_status="$(api_request POST "security.delete" "$workspace/security-delete.json" \
    "$workspace/security-delete.request.json")"
require_status "$delete_status" "200" "security.delete"

deleted_one_status="$(api_request GET "security.one?securityId=$encoded_security_id" \
    "$workspace/security-one.deleted.json")"
require_status "$deleted_one_status" "404" "security.one after delete"
security_id=""

status="$(api_request GET "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.security-deleted.json")"
require_status "$status" "200" "application.one after delete"
if ! jq -e '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.security | length) == 0
' "$workspace/application-one.security-deleted.json" >/dev/null; then
    echo "The undeployed application retained Security or deployment state." >&2
    exit 1
fi

jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
    >"$workspace/project-remove.request.json"
status="$(api_request POST "project.remove" "$workspace/project-remove.json" \
    "$workspace/project-remove.request.json")"
require_status "$status" "200" "project.remove"
project_one_status="$(api_request GET "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.security-deleted.json")"
require_status "$project_one_status" "404" "project.one after cleanup"
project_id=""
cleanup_confirmed=true

fixtures=(
    security-create application-one.security-created security-one.created security-one.updated
    application-one.security-updated security-delete security-one.deleted
    application-one.security-deleted project-one.security-deleted
)
for name in "${fixtures[@]}"; do
    jq --sort-keys --indent 2 --from-file "$sanitizer" "$workspace/$name.json" \
        >"$publish_directory/$name.owner.json"
done

jq -n --sort-keys --indent 2 \
    --arg capturedAt "2026-09-30" --arg role "owner" --arg version "$runtime_version" \
    --arg image "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8" \
    --argjson createStatus "$create_status" --argjson updateStatus "$update_status" \
    --argjson deleteStatus "$delete_status" --argjson oneStatus "$deleted_one_status" \
    --argjson projectOneStatus "$project_one_status" '
    {
        capturedAt:$capturedAt,role:$role,version:$version,image:$image,sanitized:true,deployed:false,
        collisionKey:"application+username",credentialsVerifiedPrivately:true,
        createIdentity:{setDifference:true,parentVerified:true,createStatus:$createStatus},
        update:{status:$updateStatus,completeFieldsPersisted:true,parentVerified:true},
        deleteStatus:$deleteStatus,
        cleanupEvidence:{oneStatus:$oneStatus,applicationSecurityEmpty:true,projectOneStatus:$projectOneStatus}
    }
' >"$publish_directory/security-contract.metadata.json"

mkdir -p "$fixture_directory"
for fixture in "$publish_directory"/*.json; do
    cp "$fixture" "$fixture_directory/$(basename "$fixture")"
done

"$script_directory/check-fixtures.sh"
capture_succeeded=true
echo "Captured the sanitized Security contract without deploying the application."
