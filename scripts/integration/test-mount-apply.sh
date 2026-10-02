#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=mount-evidence.sh
source "$script_directory/mount-evidence.sh"

api_key_file="$state_directory/api-key"
expected_image='dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8'
if [[ ! -s "$api_key_file" ]]; then
    echo "integration API key is missing; run scripts/integration/up.sh first" >&2
    exit 1
fi

container_id="$(compose ps --quiet dokploy)"
if [[ -z "$container_id" ]]; then
    echo "the local Dokploy integration container is not running" >&2
    exit 1
fi
actual_image="$(docker inspect --format '{{.Config.Image}}' "$container_id")"
if [[ "$actual_image" != "$expected_image" ]]; then
    echo "the running Dokploy image does not match the pinned v0.30.6 digest" >&2
    exit 1
fi

umask 077
workspace="$(mktemp -d "$state_directory/phase8-mount-apply.XXXXXX")"
private_directory="$workspace/private"
import_directory="$workspace/import"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
project_name="phase8-mount-$run_id"
config_file="$workspace/dokploy.yaml"
import_config_file="$import_directory/dokploy.yaml"
api_header_file="$private_directory/api-header"
content_file="$private_directory/mount-content"
project_output="$workspace/cleanup-projects.json"
state_file="$workspace/.dokploy/state.json"
# The file content and API key are generated per run and exist only in the
# private directory; they are compared with the retained evidence at the end.
content_canary="mount-content-$(openssl rand -hex 16)"
rotated_canary="mount-rotated-$(openssl rand -hex 16)"
api_key_value="$(tr -d '\r\n' <"$api_key_file")"
project_id=""
api_application_id=""
worker_application_id=""
compose_id=""
declare -a observed_mount_ids=()

mkdir -p "$private_directory" "$import_directory"
chmod 700 "$private_directory" "$import_directory"
{
    printf 'x-api-key: %s\n' "$api_key_value"
} >"$api_header_file"
chmod 600 "$api_header_file"
printf '%s' "$content_canary" >"$content_file"
chmod 600 "$content_file"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

# Raw responses stay in the private directory; only allowlisted projections
# are written next to the retained evidence.
api_get() {
    local endpoint="$1"
    local destination="$2"

    curl \
        --silent \
        --show-error \
        --output "$destination" \
        --write-out '%{http_code}' \
        --header "@$api_header_file" \
        "$base_url/api/$endpoint"
}

state_value() {
    jq -er --arg address "$1" '.resources[$address].remoteId' "$state_file"
}

# Proves the exact Mount record and, for file Mounts, the exact remote content
# without ever retaining the content bytes.
capture_mount_one_evidence() {
    local mount_id="$1"
    local service_type="$2"
    local service_id="$3"
    local mount_type="$4"
    local mount_path="$5"
    local source_key="$6"
    local source_value="$7"
    local destination="$8"
    local expected_content_file="${9:-}"
    local id_field="${service_type}Id"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "mounts.one?mountId=$(urlencode "$mount_id")" "$raw_response")"
    if [[ "$status" != "200" ]]; then
        rm -f -- "$raw_response"
        echo "mounts.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e \
        --arg id "$mount_id" \
        --arg service_type "$service_type" \
        --arg id_field "$id_field" \
        --arg service_id "$service_id" \
        --arg type "$mount_type" \
        --arg path "$mount_path" \
        --arg source_key "$source_key" \
        --arg source_value "$source_value" '
        .mountId == $id
        and .serviceType == $service_type
        and .[$id_field] == $service_id
        and .type == $type
        and .mountPath == $path
        and .[$source_key] == $source_value
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "mounts.one did not prove the exact declarative Mount state" >&2
        return 1
    fi
    if [[ -n "$expected_content_file" ]]; then
        if ! jq -e --rawfile content "$expected_content_file" '.content == $content' \
            "$raw_response" >/dev/null; then
            rm -f -- "$raw_response"
            echo "mounts.one did not prove the exact remote file content" >&2
            return 1
        fi
    fi
    jq '{mountId, serviceType, type, mountPath, hostPath, volumeName, filePath}' \
        "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

capture_mount_absence() {
    local absent_mount_id="$1"
    local destination="$2"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "mounts.one?mountId=$(urlencode "$absent_mount_id")" "$raw_response")"
    rm -f -- "$raw_response"
    printf '%s\n' "$status" >"$destination"

    [[ "$status" == "404" ]]
}

capture_mount_collection() {
    local service_type="$1"
    local service_id="$2"
    local destination="$3"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "mounts.listByServiceId?serviceId=$(urlencode "$service_id")&serviceType=$service_type" "$raw_response")"
    if [[ "$status" != "200" ]]; then
        rm -f -- "$raw_response"
        echo "mounts.listByServiceId returned HTTP $status; expected 200" >&2
        return 1
    fi
    jq '{mountIds: ([.[].mountId] | sort)}' "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

assert_mount_collection() {
    local evidence_file="$1"
    shift
    local expected
    expected="$(printf '%s\n' "$@" | jq -R . | jq -sc 'map(select(. != "")) | sort')"

    if ! jq -e --argjson expected "$expected" '.mountIds == $expected' "$evidence_file" >/dev/null; then
        echo "the authoritative Mount collection did not match the expected identities" >&2
        return 1
    fi
}

capture_zero_deployments() {
    local kind="$1"
    local id="$2"
    local destination="$3"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "$kind.one?${kind}Id=$(urlencode "$id")" "$raw_response")"
    if [[ "$status" != "200" ]] || ! jq -e '
        (.deployments | type) == "array" and (.deployments | length) == 0
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "$kind.one did not prove an undeployed service" >&2
        return 1
    fi
    jq '{deploymentCount: (.deployments | length)}' "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

cli() {
    "$repository_root/target/debug/dokploy" "$@"
}

cleanup() {
    local primary_status=$?
    local cleanup_status=0
    local cleanup_project_id="$project_id"
    local identity

    trap - EXIT
    set +e
    if [[ -z "$cleanup_project_id" && -s "$state_file" ]]; then
        cleanup_project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$state_file")"
    fi
    if [[ -z "$cleanup_project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$api_key_value" \
            cli project all \
            >"$workspace/cleanup-projects-before.stdout" \
            2>"$workspace/cleanup-projects-before.stderr"
        if [[ "$?" -eq 0 ]]; then
            cleanup_project_id="$(jq -r --arg name "$project_name" '[.[] | select(.name == $name)] | if length == 1 then .[0].projectId else empty end' "$workspace/cleanup-projects-before.stdout")"
        else
            cleanup_status=1
        fi
    fi
    if [[ -n "$cleanup_project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$api_key_value" \
            cli project remove \
            --body-project-id "$cleanup_project_id" \
            >"$workspace/cleanup-project-remove.stdout" \
            2>"$workspace/cleanup-project-remove.stderr"
        if [[ "$?" -ne 0 ]]; then
            cleanup_status=1
        fi
    fi

    DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$api_key_value" \
        cli project all \
        >"$project_output" \
        2>"$workspace/cleanup-projects.stderr"
    if [[ "$?" -ne 0 ]] || ! jq -e --arg name "$project_name" '[.[] | select(.name == $name)] | length == 0' "$project_output" >/dev/null; then
        echo "Mount cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    for identity in "${observed_mount_ids[@]}"; do
        if ! capture_mount_absence "$identity" "$workspace/cleanup-mount.$identity.status"; then
            echo "Mount cleanup could not prove Mount identity absence" >&2
            cleanup_status=1
        fi
    done

    if ! scrub_mount_private_evidence "$private_directory" "$workspace"; then
        echo "Mount cleanup could not discard private evidence" >&2
        cleanup_status=1
    fi
    if ! assert_mount_retained_evidence_secret_free \
        "$workspace" \
        "$api_key_value" \
        "$content_canary" \
        "$rotated_canary"; then
        cleanup_status=1
    fi

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-mount-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "Mount integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

# Configuration variables mutated between applies.
data_target="api"
data_path="/data"
data_source="{ type: volume, volume_name: data-$run_id }"
include_shared="true"
removed_block=""

write_config() {
    local shared=""

    if [[ "$include_shared" == "true" ]]; then
        shared="
      shared:
        target: compose.web
        mount_path: /shared
        source: { type: volume, volume_name: shared-$run_id }"
    fi

    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Mount executor validation
  environments:
    production:
      description: Managed by the Phase 8 Mount integration check
      applications:
        api: {}
        worker: {}
      compose:
        web:
          document:
            file: compose.yaml
      mounts:
        data:
          target: application.$data_target
          mount_path: $data_path
          source: $data_source$shared
        settings:
          target: application.api
          mount_path: /etc/settings.conf
          source:
            type: file
            file_path: settings.conf
            content:
              file: private/mount-content
$removed_block
EOF
    chmod 600 "$config_file"
}

printf 'services:\n  web:\n    image: busybox:1.36\n    command: ["sleep", "infinity"]\n' \
    >"$workspace/compose.yaml"

run_apply() {
    local label="$1"

    cli apply \
        --file "$config_file" \
        --auto-approve \
        >"$workspace/$label.stdout" \
        2>"$workspace/$label.stderr"
}

require_noop_plan() {
    local label="$1"
    local file="${2:-$config_file}"

    cli plan \
        --file "$file" \
        --json \
        --detailed-exitcode \
        >"$workspace/$label.stdout" \
        2>"$workspace/$label.stderr"
}

cargo build --manifest-path "$repository_root/Cargo.toml" --locked -p dokploy-cli

export DOKPLOY_URL="$base_url"
export DOKPLOY_API_KEY="$api_key_value"

# 1. Create undeployed against an application and a Compose target, including a
# file Mount whose content comes from a descriptor only; then converge.
write_config
run_apply create
require_noop_plan after-create

project_id="$(state_value "project.$project_name")"
api_application_id="$(state_value application.api)"
worker_application_id="$(state_value application.worker)"
compose_id="$(state_value compose.web)"
data_id="$(state_value mount.data)"
shared_id="$(state_value mount.shared)"
settings_id="$(state_value mount.settings)"
observed_mount_ids+=("$data_id" "$shared_id" "$settings_id")
capture_mount_one_evidence "$data_id" application "$api_application_id" volume /data volumeName "data-$run_id" "$workspace/mount-data.created.json"
capture_mount_one_evidence "$shared_id" compose "$compose_id" volume /shared volumeName "shared-$run_id" "$workspace/mount-shared.created.json"
capture_mount_one_evidence "$settings_id" application "$api_application_id" file /etc/settings.conf filePath settings.conf "$workspace/mount-settings.created.json" "$content_file"
capture_mount_collection application "$api_application_id" "$workspace/collection-api.created.json"
assert_mount_collection "$workspace/collection-api.created.json" "$data_id" "$settings_id"
capture_mount_collection compose "$compose_id" "$workspace/collection-compose.created.json"
assert_mount_collection "$workspace/collection-compose.created.json" "$shared_id"
if ! jq -e '.resources["mount.settings"].sensitiveInputs // {} | keys | length >= 0' "$state_file" >/dev/null; then
    echo "durable state is not readable" >&2
    exit 1
fi

# 2. Update the Mount path in place and rotate the file content; the physical
# identities must not change.
data_path="/data2"
printf '%s' "$rotated_canary" >"$content_file"
write_config
run_apply update
require_noop_plan after-update
if [[ "$(state_value mount.data)" != "$data_id" || "$(state_value mount.settings)" != "$settings_id" ]]; then
    echo "an in-place Mount update changed a physical identity" >&2
    exit 1
fi
capture_mount_one_evidence "$data_id" application "$api_application_id" volume /data2 volumeName "data-$run_id" "$workspace/mount-data.updated.json"
capture_mount_one_evidence "$settings_id" application "$api_application_id" file /etc/settings.conf filePath settings.conf "$workspace/mount-settings.updated.json" "$content_file"

# 3. Changing the target service replaces the Mount (delete before create).
data_target="worker"
write_config
run_apply target-replacement
require_noop_plan after-target-replacement
replacement_id="$(state_value mount.data)"
if [[ "$replacement_id" == "$data_id" ]]; then
    echo "a target change did not replace the physical Mount" >&2
    exit 1
fi
observed_mount_ids+=("$replacement_id")
if ! capture_mount_absence "$data_id" "$workspace/mount-data.replaced.status"; then
    echo "mounts.one did not prove absence of the replaced Mount identity" >&2
    exit 1
fi
capture_mount_one_evidence "$replacement_id" application "$worker_application_id" volume /data2 volumeName "data-$run_id" "$workspace/mount-data.replacement.json"
data_id="$replacement_id"

# 4. Changing the storage type is a replacement as well.
data_source="{ type: bind, host_path: /var/lib/phase8-mount-$run_id }"
write_config
run_apply type-replacement
require_noop_plan after-type-replacement
type_replacement_id="$(state_value mount.data)"
if [[ "$type_replacement_id" == "$data_id" ]]; then
    echo "a storage type change did not replace the physical Mount" >&2
    exit 1
fi
observed_mount_ids+=("$type_replacement_id")
if ! capture_mount_absence "$data_id" "$workspace/mount-data.type-replaced.status"; then
    echo "mounts.one did not prove absence of the type-replaced Mount identity" >&2
    exit 1
fi
capture_mount_one_evidence "$type_replacement_id" application "$worker_application_id" bind /data2 hostPath "/var/lib/phase8-mount-$run_id" "$workspace/mount-data.type-replacement.json"
data_id="$type_replacement_id"

# 5. Delete declaratively and prove authoritative absence.
include_shared="false"
removed_block='removed:
  - from: mount.shared
    destroy: true'
write_config
run_apply delete
if ! capture_mount_absence "$shared_id" "$workspace/mount-shared.deleted.status"; then
    echo "mounts.one did not prove absence after declarative deletion" >&2
    exit 1
fi
capture_mount_collection compose "$compose_id" "$workspace/collection-compose.deleted.json"
assert_mount_collection "$workspace/collection-compose.deleted.json"
if jq -e '.resources["mount.shared"]' "$state_file" >/dev/null; then
    echo "the deleted Mount identity remains in durable state" >&2
    exit 1
fi
require_noop_plan after-delete

# 6. Adopt an out-of-band Mount through protected import and converge.
cli mounts create \
    --body-type volume \
    --body-volume-name "adopted-$run_id" \
    --body-mount-path /adopted \
    --body-service-type application \
    --body-service-id "$worker_application_id" \
    >"$workspace/out-of-band-create.stdout" \
    2>"$workspace/out-of-band-create.stderr"
adopted_id="$(jq -er '.mountId' "$workspace/out-of-band-create.stdout")"
observed_mount_ids+=("$adopted_id")
# Import adopts the whole project; the adopted Mount is found by its remote
# identity because its address is derived from its mount path.
cli import project "$project_id" \
    --file "$import_config_file" \
    >"$workspace/import.stdout" \
    2>"$workspace/import.stderr"
require_noop_plan after-import "$import_config_file"
if ! jq -e --arg id "$adopted_id" '
    [.resources[] | select(.kind == "mount" and .remoteId == $id)] as $adopted
    | ($adopted | length) == 1 and $adopted[0].protected == true
' "$import_directory/.dokploy/state.json" >/dev/null; then
    echo "the imported Mount is not protected in durable state" >&2
    exit 1
fi
capture_mount_one_evidence "$adopted_id" application "$worker_application_id" volume /adopted volumeName "adopted-$run_id" "$workspace/mount-adopted.json"

# 7. No deployment was created for any target.
capture_zero_deployments application "$api_application_id" "$workspace/deployments-api.json"
capture_zero_deployments application "$worker_application_id" "$workspace/deployments-worker.json"
capture_zero_deployments compose "$compose_id" "$workspace/deployments-compose.json"

echo "Mount declarative create (application and Compose targets, file content), no-op convergence, in-place update and content rotation, target and type replacement, deletion, absence, protected import, and zero-deployment checks passed."
