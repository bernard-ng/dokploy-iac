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
    echo "Run scripts/integration/up.sh before capturing the Mount contract." >&2
    exit 1
fi

umask 077
mkdir -p "$state_directory"
chmod 700 "$state_directory"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/mount-contract-$run_id"
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
application_id=""
mount_id=""
volume_name="mount-contract-$run_id"
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

list_mounts() {
    local destination="$1"

    api_request \
        GET \
        "mounts.listByServiceId?serviceType=application&serviceId=$(urlencode "$application_id")" \
        "$destination"
}

recover_mount_id() {
    local recovery_status
    local match_count

    if [[ -n "$mount_id" || -z "$application_id" ]]; then
        return
    fi

    recovery_status="$(list_mounts "$workspace/mount-list.recovery.json")" || recovery_status=""
    if [[ "$recovery_status" != "200" ]]; then
        return
    fi

    match_count="$(jq \
        --arg applicationId "$application_id" \
        --arg volumeName "$volume_name" '
        [.[] | select(
            .serviceType == "application"
            and .applicationId == $applicationId
            and .type == "volume"
            and .volumeName == $volumeName
            and (.mountPath == "/data" or .mountPath == "/updated")
        )] | length
    ' "$workspace/mount-list.recovery.json")" || match_count=""
    if [[ "$match_count" == "1" ]]; then
        mount_id="$(jq -er \
            --arg applicationId "$application_id" \
            --arg volumeName "$volume_name" '
            .[] | select(
                .serviceType == "application"
                and .applicationId == $applicationId
                and .type == "volume"
                and .volumeName == $volumeName
                and (.mountPath == "/data" or .mountPath == "/updated")
            ) | .mountId
        ' "$workspace/mount-list.recovery.json")" || mount_id=""
    elif [[ -n "$match_count" && "$match_count" != "0" ]]; then
        echo "Cleanup found multiple owned Mount candidates; refusing ambiguous removal." >&2
    fi
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/mount-contract-*) ;;
        *)
            echo "Refusing to delete an unexpected Mount capture workspace." >&2
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
        recover_mount_id

        if [[ -n "$mount_id" ]]; then
            jq -n --arg mountId "$mount_id" '{mountId:$mountId}' \
                >"$workspace/mount-remove.cleanup.request.json"
            cleanup_status="$(api_request \
                POST \
                "mounts.remove" \
                "$workspace/mount-remove.cleanup.json" \
                "$workspace/mount-remove.cleanup.request.json")"
            if [[ "$cleanup_status" != "200" ]]; then
                echo "Mount cleanup returned HTTP $cleanup_status; inspect the private workspace." >&2
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
        echo "Mount capture evidence remains in $workspace" >&2
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

project_name="mount-sdk-contract-$run_id"
jq -n \
    --arg name "$project_name" \
    '{name:$name,description:"Disposable Mount SDK contract"}' \
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
    '{name:"Mount Contract Application",environmentId:$environmentId}' \
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

preflight_status="$(list_mounts "$workspace/mount-list.preflight.json")"
require_status "$preflight_status" "200" "mounts.listByServiceId before create"
if ! jq -e '. == []' "$workspace/mount-list.preflight.json" >/dev/null; then
    echo "The disposable application had Mounts before capture." >&2
    exit 1
fi

jq -n \
    --arg serviceId "$application_id" \
    --arg volumeName "$volume_name" '
    {
        type:"volume",
        volumeName:$volumeName,
        mountPath:"/data",
        serviceType:"application",
        serviceId:$serviceId
    }
' >"$workspace/mount-create.request.json"
create_status="$(api_request \
    POST \
    "mounts.create" \
    "$workspace/mount-create.json" \
    "$workspace/mount-create.request.json")"
require_status "$create_status" "200" "mounts.create"
mount_id="$(jq -er \
    --arg applicationId "$application_id" \
    --arg volumeName "$volume_name" '
    select(
        .mountId != null
        and .serviceType == "application"
        and .applicationId == $applicationId
        and .type == "volume"
        and .volumeName == $volumeName
        and .mountPath == "/data"
    ) | .mountId
' "$workspace/mount-create.json")"
encoded_mount_id="$(urlencode "$mount_id")"

one_status="$(api_request \
    GET \
    "mounts.one?mountId=$encoded_mount_id" \
    "$workspace/mount-one.created.json")"
require_status "$one_status" "200" "mounts.one after create"
if ! jq -e \
    --arg id "$mount_id" \
    --arg applicationId "$application_id" \
    --arg volumeName "$volume_name" '
    .mountId == $id
    and .serviceType == "application"
    and .applicationId == $applicationId
    and .type == "volume"
    and .volumeName == $volumeName
    and .mountPath == "/data"
' "$workspace/mount-one.created.json" >/dev/null; then
    echo "mounts.one did not preserve the created Mount identity and target." >&2
    exit 1
fi

created_list_status="$(list_mounts "$workspace/mount-list.created.json")"
require_status "$created_list_status" "200" "mounts.listByServiceId after create"
if ! jq -e \
    --arg id "$mount_id" \
    --arg applicationId "$application_id" '
    length == 1
    and .[0].mountId == $id
    and .[0].serviceType == "application"
    and .[0].applicationId == $applicationId
' "$workspace/mount-list.created.json" >/dev/null; then
    echo "mounts.listByServiceId did not return one exact created identity." >&2
    exit 1
fi

jq -n \
    --arg mountId "$mount_id" \
    '{mountId:$mountId,mountPath:"/updated"}' \
    >"$workspace/mount-update.request.json"
update_status="$(api_request \
    POST \
    "mounts.update" \
    "$workspace/mount-update.json" \
    "$workspace/mount-update.request.json")"
require_status "$update_status" "200" "mounts.update"

updated_one_status="$(api_request \
    GET \
    "mounts.one?mountId=$encoded_mount_id" \
    "$workspace/mount-one.updated.json")"
require_status "$updated_one_status" "200" "mounts.one after update"
if ! jq -e --arg id "$mount_id" '
    .mountId == $id and .mountPath == "/updated"
' "$workspace/mount-one.updated.json" >/dev/null; then
    echo "Mount path update did not persist on the undeployed application." >&2
    exit 1
fi

jq -n --arg mountId "$mount_id" '{mountId:$mountId}' \
    >"$workspace/mount-remove.request.json"
remove_status="$(api_request \
    POST \
    "mounts.remove" \
    "$workspace/mount-remove.json" \
    "$workspace/mount-remove.request.json")"
require_status "$remove_status" "200" "mounts.remove"
mount_id=""

removed_one_status="$(api_request \
    GET \
    "mounts.one?mountId=$encoded_mount_id" \
    "$workspace/mount-one.removed.json")"
require_status "$removed_one_status" "404" "mounts.one after remove"

removed_list_status="$(list_mounts "$workspace/mount-list.removed.json")"
require_status "$removed_list_status" "200" "mounts.listByServiceId after remove"
if ! jq -e '. == []' "$workspace/mount-list.removed.json" >/dev/null; then
    echo "mounts.listByServiceId still contains the removed Mount." >&2
    exit 1
fi

application_one_status="$(api_request \
    GET \
    "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.mount-removed.json")"
require_status "$application_one_status" "200" "application.one after Mount remove"
if ! jq -e '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.mounts | length) == 0
' "$workspace/application-one.mount-removed.json" >/dev/null; then
    echo "The undeployed application retained Mount or deployment state." >&2
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
    "$workspace/project-one.mount-removed.json")"
require_status "$removed_project_status" "404" "project.one after cleanup"
project_id=""
cleanup_confirmed=true

declare -a fixture_sources=(
    "mount-create"
    "mount-one.created"
    "mount-list.created"
    "mount-update"
    "mount-one.updated"
    "mount-remove"
    "mount-one.removed"
    "mount-list.removed"
    "application-one.mount-removed"
    "project-one.mount-removed"
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
    --argjson removeStatus "$remove_status" \
    --argjson oneStatus "$removed_one_status" \
    --argjson projectOneStatus "$removed_project_status" '
    {
        capturedAt:$capturedAt,
        role:$role,
        version:$version,
        image:$image,
        sanitized:true,
        deployed:false,
        mountType:"volume",
        createIdentity:{direct:true,targetVerified:true,createStatus:$createStatus},
        update:{status:$updateStatus,mountPathPersisted:true},
        removeStatus:$removeStatus,
        cleanupEvidence:{
            oneStatus:$oneStatus,
            listEmpty:true,
            applicationMountsEmpty:true,
            projectOneStatus:$projectOneStatus
        }
    }
' >"$publish_directory/mount-contract.metadata.json"

mkdir -p "$fixture_directory"
for fixture in "$publish_directory"/*.json; do
    cp "$fixture" "$fixture_directory/$(basename "$fixture")"
done

"$script_directory/check-fixtures.sh"
capture_succeeded=true

echo "Captured the sanitized Mount contract without deploying the application."
