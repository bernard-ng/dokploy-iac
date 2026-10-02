#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=compose-evidence.sh
source "$script_directory/compose-evidence.sh"

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
workspace="$(mktemp -d "$state_directory/phase8-compose-apply.XXXXXX")"
private_directory="$workspace/private"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
project_name="phase8-compose-$run_id"
compose_name="main"
config_file="$workspace/dokploy.yaml"
api_header_file="$private_directory/api-header"
project_output="$workspace/cleanup-projects.json"
initial_document_canary="compose-document-initial-$run_id-$(openssl rand -hex 12)"
updated_document_canary="compose-document-updated-$run_id-$(openssl rand -hex 12)"
fingerprint_key="$(uuidgen | tr '[:upper:]' '[:lower:]'):$(openssl rand -hex 32)"
project_id=""
environment_id=""
compose_id=""
pending_raw_response=""

mkdir -p "$private_directory"
chmod 700 "$private_directory"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$api_header_file"
chmod 600 "$api_header_file"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

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

discard_pending_raw_response() {
    local raw_response="$pending_raw_response"

    if [[ -z "$raw_response" ]]; then
        return 0
    fi

    rm -f -- "$raw_response"
    if [[ -e "$raw_response" ]]; then
        echo "could not discard a private Compose response" >&2
        return 1
    fi

    pending_raw_response=""
}

prepare_private_response() {
    if ! discard_pending_raw_response; then
        return 1
    fi
    if ! pending_raw_response="$(mktemp "$private_directory/response.XXXXXX")"; then
        pending_raw_response=""
        return 1
    fi
    chmod 600 "$pending_raw_response"
}

capture_compose_one_evidence() {
    local expected_document="$1"
    local destination="$2"
    local candidate=""
    local status=""

    prepare_private_response
    status="$(api_get "compose.one?composeId=$(urlencode "$compose_id")" "$pending_raw_response")"
    if [[ "$status" != "200" ]]; then
        discard_pending_raw_response
        echo "compose.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e \
        --arg id "$compose_id" \
        --arg environment_id "$environment_id" \
        --arg name "$compose_name" \
        --arg document "$expected_document" '
        .composeId == $id
        and .environmentId == $environment_id
        and .name == $name
        and .sourceType == "raw"
        and .composeType == "docker-compose"
        and .composeStatus == "idle"
        and .serverId == null
        and .composeFile == $document
        and (.deployments | type) == "array"
        and (.deployments | length) == 0
    ' "$pending_raw_response" >/dev/null; then
        discard_pending_raw_response
        echo "compose.one did not prove the exact undeployed Compose state" >&2
        return 1
    fi

    candidate="$(mktemp "$destination.candidate.XXXXXX")"
    chmod 600 "$candidate"
    if ! jq '{
        composeId,
        environmentId,
        name,
        description,
        sourceType,
        composeType,
        composeStatus,
        serverId,
        deploymentCount: (.deployments | length)
    }' "$pending_raw_response" >"$candidate"; then
        rm -f -- "$candidate"
        discard_pending_raw_response
        return 1
    fi
    if ! jq -e '
        (keys | sort) == [
            "composeId",
            "composeStatus",
            "composeType",
            "deploymentCount",
            "description",
            "environmentId",
            "name",
            "serverId",
            "sourceType"
        ]
        and (.composeId | type) == "string"
        and (.environmentId | type) == "string"
        and (.name | type) == "string"
        and (.sourceType | type) == "string"
        and (.composeType | type) == "string"
        and (.composeStatus | type) == "string"
        and .serverId == null
        and .deploymentCount == 0
    ' "$candidate" >/dev/null; then
        rm -f -- "$candidate"
        discard_pending_raw_response
        echo "the Compose evidence projection was invalid" >&2
        return 1
    fi
    discard_pending_raw_response
    mv -- "$candidate" "$destination"
}

capture_compose_one_absence() {
    local destination="$1"
    local candidate=""
    local status=""

    prepare_private_response
    status="$(api_get "compose.one?composeId=$(urlencode "$compose_id")" "$pending_raw_response")"
    discard_pending_raw_response

    candidate="$(mktemp "$destination.candidate.XXXXXX")"
    chmod 600 "$candidate"
    printf '%s\n' "$status" >"$candidate"
    mv -- "$candidate" "$destination"

    [[ "$status" == "404" ]]
}

capture_compose_search_evidence() {
    local destination="$1"
    local candidate=""
    local status=""

    prepare_private_response
    status="$(api_get \
        "compose.search?environmentId=$(urlencode "$environment_id")&limit=100&offset=0" \
        "$pending_raw_response")"
    if [[ "$status" != "200" ]]; then
        discard_pending_raw_response
        echo "compose.search returned HTTP $status; expected 200" >&2
        return 1
    fi

    candidate="$(mktemp "$destination.candidate.XXXXXX")"
    chmod 600 "$candidate"
    if ! jq '{
        items: [
            .items[]? | {
                composeId,
                environmentId,
                name,
                description,
                sourceType,
                composeStatus
            }
        ],
        total
    }' "$pending_raw_response" >"$candidate"; then
        rm -f -- "$candidate"
        discard_pending_raw_response
        return 1
    fi
    if ! jq -e '
        (keys | sort) == ["items", "total"]
        and (.items | type) == "array"
        and (.total | type) == "number"
        and ([.items[] | select(
            (.composeId | type) != "string"
            or (.environmentId | type) != "string"
            or (.name | type) != "string"
            or (.sourceType | type) != "string"
            or (.composeStatus | type) != "string"
        )] | length) == 0
    ' "$candidate" >/dev/null; then
        rm -f -- "$candidate"
        discard_pending_raw_response
        echo "the Compose collection evidence projection was invalid" >&2
        return 1
    fi
    discard_pending_raw_response
    mv -- "$candidate" "$destination"
}

cleanup() {
    local primary_status=$?
    local cleanup_status=0
    local cleanup_project_id="$project_id"
    local state_file="$workspace/.dokploy/state.json"

    trap - EXIT
    set +e
    unset DOKPLOY_FINGERPRINT_KEY PHASE8_COMPOSE_DOCUMENT
    if ! discard_pending_raw_response; then
        cleanup_status=1
    fi
    if [[ -s "$state_file" ]]; then
        if [[ -z "$cleanup_project_id" ]]; then
            cleanup_project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$state_file")"
        fi
        if [[ -z "$compose_id" ]]; then
            compose_id="$(jq -r '.resources["compose.main"].remoteId // empty' "$state_file")"
        fi
    fi
    if [[ -z "$cleanup_project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            "$repository_root/target/debug/dokploy" api project all \
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
            "$repository_root/target/debug/dokploy" api project remove \
            --body-project-id "$cleanup_project_id" \
            >"$workspace/cleanup-project-remove.stdout" \
            2>"$workspace/cleanup-project-remove.stderr"
        if [[ "$?" -ne 0 ]]; then
            cleanup_status=1
        fi
    fi

    DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
        "$repository_root/target/debug/dokploy" api project all \
        >"$project_output" \
        2>"$workspace/cleanup-projects.stderr"
    if [[ "$?" -ne 0 ]] || ! jq -e --arg name "$project_name" '[.[] | select(.name == $name)] | length == 0' "$project_output" >/dev/null; then
        echo "Compose cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    if [[ -n "$compose_id" ]]; then
        if ! capture_compose_one_absence "$workspace/cleanup-compose.removed.status"; then
            echo "Compose cleanup could not prove resource absence" >&2
            cleanup_status=1
        fi
    fi

    if ! discard_pending_raw_response; then
        cleanup_status=1
    fi
    if ! scrub_compose_private_evidence "$private_directory" "$workspace"; then
        echo "Compose cleanup could not discard private authentication evidence" >&2
        cleanup_status=1
    fi

    if ! assert_compose_retained_evidence_secret_free \
        "$workspace" \
        "$initial_document_canary" \
        "$updated_document_canary" \
        "$fingerprint_key" \
        "$(tr -d '\r\n' <"$api_key_file")"; then
        cleanup_status=1
    fi

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-compose-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "Compose integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

write_compose_config() {
    local description="$1"

    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Compose executor validation
  environments:
    production:
      description: Managed by the Phase 8 Compose integration check
      compose:
        $compose_name:
          description: $description
          document:
            env: PHASE8_COMPOSE_DOCUMENT
EOF
    chmod 600 "$config_file"
}

write_removed_config() {
    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Compose executor validation
  environments:
    production:
      description: Managed by the Phase 8 Compose integration check
      compose: {}
removed:
  - from: compose.$compose_name
    destroy: true
EOF
    chmod 600 "$config_file"
}

run_apply() {
    local label="$1"

    "$repository_root/target/debug/dokploy" apply \
        --file "$config_file" \
        --auto-approve \
        >"$workspace/$label.stdout" \
        2>"$workspace/$label.stderr"
}

require_noop_plan() {
    local label="$1"

    "$repository_root/target/debug/dokploy" plan \
        --file "$config_file" \
        --json \
        --detailed-exitcode \
        >"$workspace/$label.stdout" \
        2>"$workspace/$label.stderr"
}

assert_one_scoped_compose() {
    local evidence_file="$1"
    local expected_description="$2"

    if ! jq -e \
        --arg id "$compose_id" \
        --arg environment_id "$environment_id" \
        --arg name "$compose_name" \
        --arg description "$expected_description" '
        .total == 1
        and (.items | length) == 1
        and .items[0].composeId == $id
        and .items[0].environmentId == $environment_id
        and .items[0].name == $name
        and .items[0].description == $description
        and .items[0].sourceType == "raw"
        and .items[0].composeStatus == "idle"
    ' "$evidence_file" >/dev/null; then
        echo "compose.search did not prove one exact undeployed scoped resource" >&2
        exit 1
    fi
}

assert_secret_free_artifacts() {
    local secret

    for secret in "$initial_document_canary" "$updated_document_canary" "$fingerprint_key"; do
        if grep -R --exclude-dir=private -F -q -- "$secret" "$workspace"; then
            echo "a Compose document or fingerprint secret appeared in an integration artifact" >&2
            exit 1
        fi
    done
    if grep -R --exclude-dir=private -F -q -- "$(tr -d '\r\n' <"$api_key_file")" "$workspace"; then
        echo "the integration API key appeared in a Compose integration artifact" >&2
        exit 1
    fi
}

cargo build --manifest-path "$repository_root/Cargo.toml" --locked -p dokploy-cli

export DOKPLOY_URL="$base_url"
export DOKPLOY_API_KEY="$(<"$api_key_file")"
export DOKPLOY_FINGERPRINT_KEY="$fingerprint_key"
export PHASE8_COMPOSE_DOCUMENT=$'services:\n  placeholder:\n    image: busybox:1.36.1\n    command: ["sh", "-c", "sleep infinity"]\n    environment:\n      PHASE8_CANARY: '"$initial_document_canary"$'\n'

write_compose_config initial
run_apply create
require_noop_plan after-create

project_id="$(jq -er --arg address "project.$project_name" '.resources[$address].remoteId' "$workspace/.dokploy/state.json")"
environment_id="$(jq -er '.resources["environment.production"].remoteId' "$workspace/.dokploy/state.json")"
compose_id="$(jq -er --arg address "compose.$compose_name" '.resources[$address].remoteId' "$workspace/.dokploy/state.json")"
capture_compose_one_evidence "$PHASE8_COMPOSE_DOCUMENT" "$workspace/compose-one.created.json"
capture_compose_search_evidence "$workspace/compose-search.created.json"
assert_one_scoped_compose "$workspace/compose-search.created.json" initial

export PHASE8_COMPOSE_DOCUMENT=$'services:\n  placeholder:\n    image: busybox:1.36.1\n    command: ["sh", "-c", "sleep 86400"]\n    environment:\n      PHASE8_CANARY: '"$updated_document_canary"$'\n'
write_compose_config updated
run_apply metadata-document-update
require_noop_plan after-update
capture_compose_one_evidence "$PHASE8_COMPOSE_DOCUMENT" "$workspace/compose-one.updated.json"
capture_compose_search_evidence "$workspace/compose-search.updated.json"
assert_one_scoped_compose "$workspace/compose-search.updated.json" updated

write_removed_config
run_apply delete-preserving-volumes

if ! capture_compose_one_absence "$workspace/compose-one.deleted.status"; then
    echo "compose.one did not prove absence after declarative deletion" >&2
    exit 1
fi
capture_compose_search_evidence "$workspace/compose-search.deleted.json"
if ! jq -e '.items == [] and .total == 0' "$workspace/compose-search.deleted.json" >/dev/null; then
    echo "compose.search still contains the deleted Compose record" >&2
    exit 1
fi
if jq -e --arg address "compose.$compose_name" '.resources[$address]' "$workspace/.dokploy/state.json" >/dev/null; then
    echo "the deleted Compose identity remains in durable state" >&2
    exit 1
fi

require_noop_plan after-delete
assert_secret_free_artifacts

echo "Compose declarative create, no-op convergence, metadata/document update, preserve-volume deletion, absence, and secret-scan checks passed."
