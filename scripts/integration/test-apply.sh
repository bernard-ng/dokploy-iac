#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
if [[ ! -s "$api_key_file" ]]; then
    echo "integration API key is missing; run scripts/integration/up.sh first" >&2
    exit 1
fi

umask 077
workspace="$(mktemp -d "$state_directory/phase6-apply.XXXXXX")"
project_name="phase6-apply-$(date -u +%Y%m%d%H%M%S)-$$"
config_file="$workspace/dokploy.yaml"
api_header_file="$workspace/api-header"
project_output="$workspace/cleanup-projects.json"
postgres_password="$(openssl rand -hex 24)"
redis_password="$(openssl rand -hex 24)"
postgres_id=""
redis_id=""

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
    local resource_status=""
    local project_id=""
    local state_file="$workspace/.dokploy/state.json"

    trap - EXIT
    set +e
    unset PHASE6_POSTGRES_PASSWORD PHASE6_REDIS_PASSWORD
    if [[ -s "$state_file" ]]; then
        project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$state_file")"
        if [[ -z "$postgres_id" ]]; then
            postgres_id="$(jq -r '.resources["postgres.main"].remoteId // empty' "$state_file")"
        fi
        if [[ -z "$redis_id" ]]; then
            redis_id="$(jq -r '.resources["redis.cache"].remoteId // empty' "$state_file")"
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
        echo "combined apply cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    if [[ -n "$postgres_id" ]]; then
        resource_status="$(api_get "postgres.one?postgresId=$(urlencode "$postgres_id")" "$workspace/cleanup-postgres-one.json")"
        if [[ "$?" -ne 0 || "$resource_status" != "404" ]]; then
            echo "combined apply cleanup could not prove Postgres absence" >&2
            cleanup_status=1
        fi
    fi
    if [[ -n "$redis_id" ]]; then
        resource_status="$(api_get "redis.one?redisId=$(urlencode "$redis_id")" "$workspace/cleanup-redis-one.json")"
        if [[ "$?" -ne 0 || "$resource_status" != "404" ]]; then
            echo "combined apply cleanup could not prove Redis absence" >&2
            cleanup_status=1
        fi
    fi

    for secret in "$postgres_password" "$redis_password"; do
        if grep -R -F -q -- "$secret" "$workspace"; then
            echo "a database secret appeared in combined apply artifacts" >&2
            cleanup_status=1
        fi
    done

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase6-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "combined apply integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 6 executor validation
environments:
  production:
    description: Managed by the Phase 6 integration check
    applications:
      api: {}
    postgres:
      main:
        database: phase6
        username: phase6
        password:
          env: PHASE6_POSTGRES_PASSWORD
    redis:
      cache:
        password:
          env: PHASE6_REDIS_PASSWORD
    domains:
      public:
        host: $project_name.example.test
        application: application.api
EOF
chmod 600 "$config_file"

cargo build --manifest-path "$repository_root/Cargo.toml" --locked -p dokploy-cli

export DOKPLOY_URL="$base_url"
export DOKPLOY_API_KEY="$(<"$api_key_file")"
export PHASE6_POSTGRES_PASSWORD="$postgres_password"
export PHASE6_REDIS_PASSWORD="$redis_password"

"$repository_root/target/debug/dokploy" apply \
    --file "$config_file" \
    --parallelism 2 \
    --auto-approve \
    >"$workspace/apply.stdout" \
    2>"$workspace/apply.stderr"
postgres_id="$(jq -er '.resources["postgres.main"].remoteId' "$workspace/.dokploy/state.json")"
redis_id="$(jq -er '.resources["redis.cache"].remoteId' "$workspace/.dokploy/state.json")"
"$repository_root/target/debug/dokploy" plan \
    --file "$config_file" \
    --detailed-exitcode \
    >"$workspace/plan.stdout" \
    2>"$workspace/plan.stderr"

for secret in "$postgres_password" "$redis_password"; do
    if grep -R -F -q -- "$secret" "$workspace"; then
        echo "a database secret appeared in combined apply artifacts" >&2
        exit 1
    fi
done
