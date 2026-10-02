#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

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
workspace="$(mktemp -d "$state_directory/phase8-port-apply.XXXXXX")"
private_directory="$workspace/private"
import_directory="$workspace/import"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
project_name="phase8-port-$run_id"
config_file="$workspace/dokploy.yaml"
import_config_file="$import_directory/dokploy.yaml"
api_header_file="$private_directory/api-header"
project_output="$workspace/cleanup-projects.json"
project_id=""
environment_id=""
api_application_id=""
worker_application_id=""
port_id=""
adopted_port_id=""
declare -a observed_port_ids=()

mkdir -p "$private_directory" "$import_directory"
chmod 700 "$private_directory" "$import_directory"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$api_header_file"
chmod 600 "$api_header_file"

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

project_state_value() {
    local state_file="$1"
    local address="$2"

    jq -er --arg address "$address" '.resources[$address].remoteId' "$state_file"
}

capture_port_one_evidence() {
    local expected_port_id="$1"
    local expected_application_id="$2"
    local expected_published="$3"
    local expected_target="$4"
    local expected_mode="$5"
    local expected_protocol="$6"
    local destination="$7"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "port.one?portId=$(urlencode "$expected_port_id")" "$raw_response")"
    if [[ "$status" != "200" ]]; then
        rm -f -- "$raw_response"
        echo "port.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e \
        --arg id "$expected_port_id" \
        --arg application_id "$expected_application_id" \
        --argjson published "$expected_published" \
        --argjson target "$expected_target" \
        --arg mode "$expected_mode" \
        --arg protocol "$expected_protocol" '
        .portId == $id
        and .applicationId == $application_id
        and .publishedPort == $published
        and .targetPort == $target
        and .publishMode == $mode
        and .protocol == $protocol
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "port.one did not prove the exact declarative Port state" >&2
        return 1
    fi
    jq '{portId, applicationId, publishedPort, targetPort, publishMode, protocol}' \
        "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

capture_port_one_absence() {
    local absent_port_id="$1"
    local destination="$2"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "port.one?portId=$(urlencode "$absent_port_id")" "$raw_response")"
    rm -f -- "$raw_response"
    printf '%s\n' "$status" >"$destination"

    # Dokploy v0.30.6 reports a missing Port as HTTP 400 rather than 404.
    [[ "$status" == "400" ]]
}

capture_application_ports() {
    local application_id="$1"
    local destination="$2"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "application.one?applicationId=$(urlencode "$application_id")" "$raw_response")"
    if [[ "$status" != "200" ]]; then
        rm -f -- "$raw_response"
        echo "application.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e --arg id "$application_id" '
        .applicationId == $id
        and .applicationStatus == "idle"
        and (.deployments | type) == "array"
        and (.deployments | length) == 0
        and (.ports | type) == "array"
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "application.one did not prove an idle, undeployed application" >&2
        return 1
    fi
    jq '{
        applicationId,
        applicationStatus,
        deploymentCount: (.deployments | length),
        ports: [.ports[] | {portId, applicationId, publishedPort, targetPort, publishMode, protocol}]
    }' "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

assert_application_ports() {
    local evidence_file="$1"
    local expected_count="$2"
    local expected_port_id="${3:-}"

    if ! jq -e \
        --argjson count "$expected_count" \
        --arg id "$expected_port_id" '
        .deploymentCount == 0
        and (.ports | length) == $count
        and ($id == "" or .ports[0].portId == $id)
    ' "$evidence_file" >/dev/null; then
        echo "application.one did not contain the expected authoritative Port collection" >&2
        return 1
    fi
}

cli() {
    "$repository_root/target/debug/dokploy" "$@"
}

cleanup() {
    local primary_status=$?
    local cleanup_status=0
    local cleanup_project_id="$project_id"
    local state_file="$workspace/.dokploy/state.json"
    local identity

    trap - EXIT
    set +e
    if [[ -z "$cleanup_project_id" && -s "$state_file" ]]; then
        cleanup_project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$state_file")"
    fi
    if [[ -z "$cleanup_project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
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
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            cli project remove \
            --body-project-id "$cleanup_project_id" \
            >"$workspace/cleanup-project-remove.stdout" \
            2>"$workspace/cleanup-project-remove.stderr"
        if [[ "$?" -ne 0 ]]; then
            cleanup_status=1
        fi
    fi

    DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
        cli project all \
        >"$project_output" \
        2>"$workspace/cleanup-projects.stderr"
    if [[ "$?" -ne 0 ]] || ! jq -e --arg name "$project_name" '[.[] | select(.name == $name)] | length == 0' "$project_output" >/dev/null; then
        echo "Port cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    for identity in "${observed_port_ids[@]}"; do
        if ! capture_port_one_absence "$identity" "$workspace/cleanup-port.$identity.status"; then
            echo "Port cleanup could not prove Port identity absence" >&2
            cleanup_status=1
        fi
    done

    if ! find "$private_directory" -depth -delete 2>/dev/null || [[ -e "$private_directory" ]]; then
        echo "Port cleanup could not discard private authentication evidence" >&2
        cleanup_status=1
    fi

    if grep -R -F -q -- "$(tr -d '\r\n' <"$api_key_file")" "$workspace"; then
        echo "the integration API key appeared in retained Port evidence" >&2
        cleanup_status=1
    fi

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-port-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "Port integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

write_config() {
    local owner="$1"
    local published="$2"
    local target="$3"
    local mode="$4"
    local protocol="$5"
    local api_ports='{}'
    local worker_ports='{}'

    if [[ "$owner" == "api" ]]; then
        api_ports="
        ports:
          http:
            published_port: $published
            target_port: $target
            publish_mode: $mode
            protocol: $protocol"
    else
        worker_ports="
        ports:
          http:
            published_port: $published
            target_port: $target
            publish_mode: $mode
            protocol: $protocol"
    fi

    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Port executor validation
  environments:
    production:
      description: Managed by the Phase 8 Port integration check
      applications:
        api: $api_ports
        worker: $worker_ports
EOF
    chmod 600 "$config_file"
}

write_removed_config() {
    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Port executor validation
  environments:
    production:
      description: Managed by the Phase 8 Port integration check
      applications:
        api: {}
        worker: {}
removed:
  - from: port.http
    destroy: true
EOF
    chmod 600 "$config_file"
}

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
export DOKPLOY_API_KEY="$(<"$api_key_file")"

# 1. Create through the declarative engine and converge immediately.
write_config api 18080 8080 ingress tcp
run_apply create
require_noop_plan after-create

state_file="$workspace/.dokploy/state.json"
project_id="$(project_state_value "$state_file" "project.$project_name")"
environment_id="$(project_state_value "$state_file" environment.production)"
api_application_id="$(project_state_value "$state_file" application.api)"
worker_application_id="$(project_state_value "$state_file" application.worker)"
port_id="$(project_state_value "$state_file" port.http)"
observed_port_ids+=("$port_id")
capture_port_one_evidence "$port_id" "$api_application_id" 18080 8080 ingress tcp "$workspace/port-one.created.json"
capture_application_ports "$api_application_id" "$workspace/application-api.created.json"
assert_application_ports "$workspace/application-api.created.json" 1 "$port_id"
capture_application_ports "$worker_application_id" "$workspace/application-worker.created.json"
assert_application_ports "$workspace/application-worker.created.json" 0

# 2. Replace every owned field in place; the physical identity must not change.
write_config api 18081 8081 host udp
run_apply update
require_noop_plan after-update
updated_port_id="$(project_state_value "$state_file" port.http)"
if [[ "$updated_port_id" != "$port_id" ]]; then
    echo "an in-place Port update changed the physical identity" >&2
    exit 1
fi
capture_port_one_evidence "$port_id" "$api_application_id" 18081 8081 host udp "$workspace/port-one.updated.json"
capture_application_ports "$api_application_id" "$workspace/application-api.updated.json"
assert_application_ports "$workspace/application-api.updated.json" 1 "$port_id"

# 3. Changing the containing application deletes before creating.
write_config worker 18081 8081 host udp
run_apply containment-replacement
require_noop_plan after-containment-replacement
replacement_port_id="$(project_state_value "$state_file" port.http)"
if [[ "$replacement_port_id" == "$port_id" ]]; then
    echo "a containment change did not replace the physical Port" >&2
    exit 1
fi
observed_port_ids+=("$replacement_port_id")
if ! capture_port_one_absence "$port_id" "$workspace/port-one.replaced.status"; then
    echo "port.one did not prove absence of the replaced Port identity" >&2
    exit 1
fi
capture_port_one_evidence "$replacement_port_id" "$worker_application_id" 18081 8081 host udp "$workspace/port-one.replacement.json"
capture_application_ports "$api_application_id" "$workspace/application-api.replaced.json"
assert_application_ports "$workspace/application-api.replaced.json" 0
capture_application_ports "$worker_application_id" "$workspace/application-worker.replaced.json"
assert_application_ports "$workspace/application-worker.replaced.json" 1 "$replacement_port_id"

# 4. Delete declaratively and prove authoritative absence.
write_removed_config
run_apply delete
if ! capture_port_one_absence "$replacement_port_id" "$workspace/port-one.deleted.status"; then
    echo "port.one did not prove absence after declarative deletion" >&2
    exit 1
fi
capture_application_ports "$worker_application_id" "$workspace/application-worker.deleted.json"
assert_application_ports "$workspace/application-worker.deleted.json" 0
if jq -e '.resources["port.http"]' "$state_file" >/dev/null; then
    echo "the deleted Port identity remains in durable state" >&2
    exit 1
fi
require_noop_plan after-delete

# 5. Adopt an out-of-band Port through protected import and converge.
cli port create \
    --body-application-id "$worker_application_id" \
    --body-published-port 18090 \
    --body-target-port 8090 \
    --body-publish-mode ingress \
    --body-protocol tcp \
    >"$workspace/out-of-band-create.stdout" \
    2>"$workspace/out-of-band-create.stderr"
adopted_port_id="$(jq -er '.portId' "$workspace/out-of-band-create.stdout")"
observed_port_ids+=("$adopted_port_id")
# Import adopts the whole project; the adopted Port is found by its
# remote identity because its address is derived from its natural key.
cli import project "$project_id" \
    --file "$import_config_file" \
    >"$workspace/import.stdout" \
    2>"$workspace/import.stderr"
require_noop_plan after-import "$import_config_file"
if ! jq -e --arg id "$adopted_port_id" '
    [.resources[] | select(.kind == "port" and .remoteId == $id)] as $adopted
    | ($adopted | length) == 1 and $adopted[0].protected == true
' "$import_directory/.dokploy/state.json" >/dev/null; then
    echo "the imported Port is not protected in durable state" >&2
    exit 1
fi
capture_port_one_evidence "$adopted_port_id" "$worker_application_id" 18090 8090 ingress tcp "$workspace/port-one.adopted.json"
capture_application_ports "$worker_application_id" "$workspace/application-worker.adopted.json"
assert_application_ports "$workspace/application-worker.adopted.json" 1 "$adopted_port_id"

echo "Port declarative create, no-op convergence, complete update, containment replacement, deletion, absence, protected import, and zero-deployment checks passed."
