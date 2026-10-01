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
workspace="$(mktemp -d "$state_directory/phase8-libsql-apply.XXXXXX")"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
project_name="phase8-libsql-$run_id"
config_file="$workspace/dokploy.yaml"
api_header_file="$workspace/api-header"
project_output="$workspace/cleanup-projects.json"
libsql_password="$(openssl rand -hex 24)"
rotated_libsql_password="$(openssl rand -hex 24)"
fingerprint_key="$(uuidgen | tr '[:upper:]' '[:lower:]'):$(openssl rand -hex 32)"
project_id=""
environment_id=""
original_libsql_id=""
replacement_libsql_id=""

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

cleanup() {
    local primary_status=$?
    local cleanup_status=0
    local lookup_status=0
    local one_status=""
    local cleanup_project_id="$project_id"
    local state_file="$workspace/.dokploy/state.json"

    trap - EXIT
    set +e
    unset DOKPLOY_FINGERPRINT_KEY PHASE8_LIBSQL_PASSWORD
    if [[ -s "$state_file" ]]; then
        if [[ -z "$cleanup_project_id" ]]; then
            cleanup_project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$state_file")"
        fi
        if [[ -z "$replacement_libsql_id" ]]; then
            replacement_libsql_id="$(jq -r '.resources["libsql.main"].remoteId // empty' "$state_file")"
        fi
    fi
    if [[ -z "$cleanup_project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            "$repository_root/target/debug/dokploy" project all \
            >"$workspace/cleanup-projects-before.stdout" \
            2>"$workspace/cleanup-projects-before.stderr"
        lookup_status=$?
        if [[ "$lookup_status" -eq 0 ]]; then
            cleanup_project_id="$(jq -r --arg name "$project_name" '[.[] | select(.name == $name)] | if length == 1 then .[0].projectId else empty end' "$workspace/cleanup-projects-before.stdout")"
        else
            cleanup_status=1
        fi
    fi
    if [[ -n "$cleanup_project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            "$repository_root/target/debug/dokploy" project remove \
            --body-project-id "$cleanup_project_id" \
            >"$workspace/cleanup-project-remove.stdout" \
            2>"$workspace/cleanup-project-remove.stderr"
        if [[ "$?" -ne 0 ]]; then
            cleanup_status=1
        fi
    fi

    DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
        "$repository_root/target/debug/dokploy" project all \
        >"$project_output" \
        2>"$workspace/cleanup-projects.stderr"
    if [[ "$?" -ne 0 ]] || ! jq -e --arg name "$project_name" '[.[] | select(.name == $name)] | length == 0' "$project_output" >/dev/null; then
        echo "LibSQL cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    for libsql_id in "$original_libsql_id" "$replacement_libsql_id"; do
        if [[ -n "$libsql_id" ]]; then
            one_status="$(api_get "libsql.one?libsqlId=$(urlencode "$libsql_id")" "$workspace/cleanup-libsql-$libsql_id.json")"
            if [[ "$?" -ne 0 || "$one_status" != "404" ]]; then
                echo "LibSQL cleanup could not prove database absence" >&2
                cleanup_status=1
            fi
        fi
    done

    for secret in "$libsql_password" "$rotated_libsql_password" "$fingerprint_key"; do
        if grep -R -F -q -- "$secret" "$workspace"; then
            echo "a LibSQL secret appeared in an integration artifact" >&2
            cleanup_status=1
        fi
    done

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-libsql-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "LibSQL integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

write_libsql_config() {
    local description="$1"
    local username="$2"
    local node="$3"

    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 LibSQL executor validation
environments:
  production:
    description: Managed by the Phase 8 LibSQL integration check
    libsql:
      main:
        description: $description
        username: $username
        password:
          env: PHASE8_LIBSQL_PASSWORD
        node:
          type: $node
EOF
    if [[ "$node" == "replica" ]]; then
        printf '          primary_url: http://primary.internal:8080\n' >>"$config_file"
    fi
    chmod 600 "$config_file"
}

require_noop_plan() {
    local label="$1"
    local status=0

    set +e
    "$repository_root/target/debug/dokploy" plan \
        --file "$config_file" \
        --json \
        --detailed-exitcode \
        >"$workspace/$label.stdout" \
        2>"$workspace/$label.stderr"
    status=$?
    set -e
    if [[ "$status" -ne 0 ]]; then
        if [[ -s "$workspace/.dokploy/state.json" ]]; then
            project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$workspace/.dokploy/state.json")"
            environment_id="$(jq -r '.resources["environment.production"].remoteId // empty' "$workspace/.dokploy/state.json")"
        fi
        if [[ -n "$project_id" ]]; then
            api_get "project.one?projectId=$(urlencode "$project_id")" "$workspace/$label.project-one.json" \
                >"$workspace/$label.project-one.status"
        fi
        if [[ -n "$environment_id" ]]; then
            api_get "environment.one?environmentId=$(urlencode "$environment_id")" "$workspace/$label.environment-one.json" \
                >"$workspace/$label.environment-one.status"
        fi
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            "$repository_root/target/debug/dokploy" project all \
            >"$workspace/$label.project-all.json" \
            2>"$workspace/$label.project-all.stderr"
    fi

    return "$status"
}

run_apply() {
    local label="$1"

    "$repository_root/target/debug/dokploy" apply \
        --file "$config_file" \
        --auto-approve \
        >"$workspace/$label.stdout" \
        2>"$workspace/$label.stderr"
}

assert_direct_node() {
    local libsql_id="$1"
    local expected_node="$2"
    local label="$3"
    local response_file="$workspace/$label.json"
    local sanitized_file=""
    local status=""

    status="$(api_get "libsql.one?libsqlId=$(urlencode "$libsql_id")" "$response_file")"
    if [[ "$status" != "200" ]]; then
        echo "libsql.one returned HTTP $status; expected 200" >&2
        exit 1
    fi
    sanitized_file="$(mktemp "$workspace/$label.sanitized.XXXXXX")"
    if ! jq '{libsqlId, sqldNode, applicationStatus}' "$response_file" >"$sanitized_file"; then
        rm -f -- "$sanitized_file"
        echo "libsql.one returned an invalid response" >&2
        exit 1
    fi
    chmod 600 "$sanitized_file"
    if ! jq -e '
        (keys | sort) == ["applicationStatus", "libsqlId", "sqldNode"]
        and (.libsqlId | type) == "string"
        and (.sqldNode | type) == "string"
        and (.applicationStatus | type) == "string"
    ' "$sanitized_file" >/dev/null; then
        rm -f -- "$sanitized_file"
        echo "libsql.one returned an invalid response" >&2
        exit 1
    fi
    mv -- "$sanitized_file" "$response_file"
    if ! jq -e --arg id "$libsql_id" --arg node "$expected_node" '
        .libsqlId == $id and .sqldNode == $node and .applicationStatus == "idle"
    ' "$response_file" >/dev/null; then
        echo "LibSQL node topology or undeployed status did not match" >&2
        exit 1
    fi
}

assert_secret_free_artifacts() {
    local secret

    for secret in "$libsql_password" "$rotated_libsql_password" "$fingerprint_key"; do
        if grep -R -F -q -- "$secret" "$workspace"; then
            echo "a LibSQL secret appeared in a configuration, plan, diagnostic, state, or journal artifact" >&2
            exit 1
        fi
    done
}

cargo build --manifest-path "$repository_root/Cargo.toml" --locked -p dokploy-cli

export DOKPLOY_URL="$base_url"
export DOKPLOY_API_KEY="$(<"$api_key_file")"
export DOKPLOY_FINGERPRINT_KEY="$fingerprint_key"
export PHASE8_LIBSQL_PASSWORD="$libsql_password"

write_libsql_config edge phase8 primary
run_apply create
require_noop_plan after-create

project_id="$(jq -er --arg address "project.$project_name" '.resources[$address].remoteId' "$workspace/.dokploy/state.json")"
environment_id="$(jq -er '.resources["environment.production"].remoteId' "$workspace/.dokploy/state.json")"
original_libsql_id="$(jq -er '.resources["libsql.main"].remoteId' "$workspace/.dokploy/state.json")"
assert_direct_node "$original_libsql_id" primary primary-created

write_libsql_config edge_next phase8_next primary
run_apply metadata-update
require_noop_plan after-metadata-update

export PHASE8_LIBSQL_PASSWORD="$rotated_libsql_password"
run_apply password-rotation
require_noop_plan after-password-rotation

write_libsql_config edge_next phase8_next replica
run_apply node-replacement
require_noop_plan after-node-replacement

replacement_libsql_id="$(jq -er '.resources["libsql.main"].remoteId' "$workspace/.dokploy/state.json")"
if [[ "$replacement_libsql_id" == "$original_libsql_id" ]]; then
    echo "LibSQL node replacement retained the old physical identity" >&2
    exit 1
fi
old_status="$(api_get "libsql.one?libsqlId=$(urlencode "$original_libsql_id")" "$workspace/original-libsql.removed.json")"
if [[ "$old_status" != "404" ]]; then
    echo "the replaced LibSQL identity remains readable" >&2
    exit 1
fi
assert_direct_node "$replacement_libsql_id" replica replica-created

cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 LibSQL executor validation
environments:
  production:
    description: Managed by the Phase 8 LibSQL integration check
    libsql: {}
removed:
  - from: libsql.main
    destroy: true
EOF
chmod 600 "$config_file"
run_apply delete

one_status="$(api_get "libsql.one?libsqlId=$(urlencode "$replacement_libsql_id")" "$workspace/replacement-libsql.removed.json")"
if [[ "$one_status" != "404" ]]; then
    echo "libsql.one returned HTTP $one_status after declarative deletion; expected 404" >&2
    exit 1
fi
topology_status="$(api_get "project.one?projectId=$(urlencode "$project_id")" "$workspace/project-one.after-delete.json")"
if [[ "$topology_status" != "200" ]]; then
    echo "project.one returned HTTP $topology_status after declarative deletion; expected 200" >&2
    exit 1
fi
if ! jq -e --arg environment_id "$environment_id" --arg libsql_id "$replacement_libsql_id" '
    [.environments[] | select(.environmentId == $environment_id) | .libsql[]? | select(.libsqlId == $libsql_id)] | length == 0
' "$workspace/project-one.after-delete.json" >/dev/null; then
    echo "the deleted LibSQL identity remains visible in project topology" >&2
    exit 1
fi
if jq -e '.resources["libsql.main"]' "$workspace/.dokploy/state.json" >/dev/null; then
    echo "the deleted LibSQL identity remains in durable state" >&2
    exit 1
fi

require_noop_plan after-delete
assert_secret_free_artifacts

echo "LibSQL declarative create, metadata update, password rotation, node replacement, no-op convergence, and deletion cleanup passed."
