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
workspace="$(mktemp -d "$state_directory/phase8-mongo-apply.XXXXXX")"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
project_name="phase8-mongo-$run_id"
config_file="$workspace/dokploy.yaml"
api_header_file="$workspace/api-header"
search_output="$workspace/mongo-search.removed.json"
one_output="$workspace/mongo-one.removed.json"
project_output="$workspace/cleanup-projects.json"
mongo_password="$(openssl rand -hex 24)"
rotated_mongo_password="$(openssl rand -hex 24)"
mongo_id=""
environment_id=""

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
    local project_id=""
    local state_file="$workspace/.dokploy/state.json"

    trap - EXIT
    set +e
    unset PHASE8_MONGO_PASSWORD
    if [[ -s "$state_file" ]]; then
        project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$state_file")"
        if [[ -z "$mongo_id" ]]; then
            mongo_id="$(jq -r '.resources["mongo.main"].remoteId // empty' "$state_file")"
        fi
    fi
    if [[ -z "$project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            "$repository_root/target/debug/dokploy" project all \
            >"$workspace/cleanup-projects-before.stdout" \
            2>"$workspace/cleanup-projects-before.stderr"
        lookup_status=$?
        if [[ "$lookup_status" -eq 0 ]]; then
            project_id="$(jq -r --arg name "$project_name" '[.[] | select(.name == $name)] | if length == 1 then .[0].projectId else empty end' "$workspace/cleanup-projects-before.stdout")"
        else
            cleanup_status=1
        fi
    fi
    if [[ -n "$project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            "$repository_root/target/debug/dokploy" project remove \
            --body-project-id "$project_id" \
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
        echo "MongoDB cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    if [[ -n "$mongo_id" ]]; then
        one_status="$(api_get "mongo.one?mongoId=$(urlencode "$mongo_id")" "$workspace/cleanup-mongo-one.json")"
        if [[ "$?" -ne 0 || "$one_status" != "404" ]]; then
            echo "MongoDB cleanup could not prove database absence" >&2
            cleanup_status=1
        fi
    fi

    for secret in "$mongo_password" "$rotated_mongo_password"; do
        if grep -R -F -q -- "$secret" "$workspace"; then
            echo "a MongoDB secret appeared in an integration artifact" >&2
            cleanup_status=1
        fi
    done

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-mongo-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "MongoDB integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

write_mongo_config() {
    local username="$1"
    local replica_sets="$2"

    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 MongoDB executor validation
environments:
  production:
    description: Managed by the Phase 8 MongoDB integration check
    mongo:
      main:
        username: $username
        password:
          env: PHASE8_MONGO_PASSWORD
        replica_sets: $replica_sets
EOF
    chmod 600 "$config_file"
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

run_apply() {
    local label="$1"

    "$repository_root/target/debug/dokploy" apply \
        --file "$config_file" \
        --auto-approve \
        >"$workspace/$label.stdout" \
        2>"$workspace/$label.stderr"
}

reject_password_rotation_plan() {
    local status=0

    export PHASE8_MONGO_PASSWORD="$rotated_mongo_password"
    set +e
    "$repository_root/target/debug/dokploy" plan \
        --file "$config_file" \
        --json \
        --detailed-exitcode \
        >"$workspace/password-rotation.stdout" \
        2>"$workspace/password-rotation.stderr"
    status=$?
    set -e
    export PHASE8_MONGO_PASSWORD="$mongo_password"

    if [[ "$status" -ne 1 ]]; then
        echo "MongoDB password rotation plan exited $status; expected a blocked exit status of 1" >&2
        exit 1
    fi
    if ! grep -q 'DOKPLAN017' \
        "$workspace/password-rotation.stdout" \
        "$workspace/password-rotation.stderr"; then
        echo "MongoDB password rotation did not report the unsupported-mutation diagnostic" >&2
        exit 1
    fi
    if grep -Eq 'mongo\.(update|changePassword|remove|create)' \
        "$workspace/password-rotation.stdout" \
        "$workspace/password-rotation.stderr"; then
        echo "the read-only rejected MongoDB password plan referenced a mutation endpoint" >&2
        exit 1
    fi
}

assert_secret_free_artifacts() {
    local secret

    for secret in "$mongo_password" "$rotated_mongo_password"; do
        if grep -R -F -q -- "$secret" "$workspace"; then
            echo "a MongoDB secret appeared in a configuration, plan, diagnostic, state, or journal artifact" >&2
            exit 1
        fi
    done
}

cargo build --manifest-path "$repository_root/Cargo.toml" --locked -p dokploy-cli

export DOKPLOY_URL="$base_url"
export DOKPLOY_API_KEY="$(<"$api_key_file")"
export PHASE8_MONGO_PASSWORD="$mongo_password"

write_mongo_config phase8 false
run_apply create
require_noop_plan after-create

mongo_id="$(jq -er '.resources["mongo.main"].remoteId' "$workspace/.dokploy/state.json")"
environment_id="$(jq -er '.resources["environment.production"].remoteId' "$workspace/.dokploy/state.json")"

reject_password_rotation_plan
require_noop_plan after-password-restore

write_mongo_config phase8_next true
run_apply metadata-update
require_noop_plan after-metadata-update

cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 MongoDB executor validation
environments:
  production:
    description: Managed by the Phase 8 MongoDB integration check
    mongo: {}
removed:
  - from: mongo.main
    destroy: true
EOF
chmod 600 "$config_file"
run_apply delete

encoded_mongo_id="$(urlencode "$mongo_id")"
one_status="$(api_get "mongo.one?mongoId=$encoded_mongo_id" "$one_output")"
if [[ "$one_status" != "404" ]]; then
    echo "mongo.one returned HTTP $one_status after declarative deletion; expected 404" >&2
    exit 1
fi

encoded_environment_id="$(urlencode "$environment_id")"
search_status="$(api_get "mongo.search?environmentId=$encoded_environment_id&limit=100&offset=0" "$search_output")"
if [[ "$search_status" != "200" ]]; then
    echo "mongo.search returned HTTP $search_status after declarative deletion; expected 200" >&2
    exit 1
fi
if ! jq -e --arg mongo_id "$mongo_id" '[.items[]? | select(.mongoId == $mongo_id)] | length == 0' "$search_output" >/dev/null; then
    echo "the deleted MongoDB identity remains visible in its environment" >&2
    exit 1
fi
if jq -e '.resources["mongo.main"]' "$workspace/.dokploy/state.json" >/dev/null; then
    echo "the deleted MongoDB identity remains in durable state" >&2
    exit 1
fi

require_noop_plan after-delete
assert_secret_free_artifacts

echo "MongoDB declarative create, metadata update, no-op convergence, blocked password rotation, and deletion cleanup passed."
