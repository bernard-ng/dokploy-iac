#!/usr/bin/env bash

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

api_key_file="$state_directory/api-key"
if [[ ! -s "$api_key_file" ]]; then
    echo "integration API key is missing; run scripts/integration/up.sh first" >&2
    exit 1
fi

workspace="$(mktemp -d "$state_directory/phase6-apply.XXXXXX")"
project_name="phase6-apply-$(date -u +%Y%m%d%H%M%S)-$$"
config_file="$workspace/dokploy.yaml"
project_output="$workspace/projects.json"
postgres_password="$(openssl rand -hex 24)"
redis_password="$(openssl rand -hex 24)"

cleanup() {
    local project_id=""
    local state_file="$workspace/.dokploy/state.json"

    if [[ -s "$state_file" ]]; then
        project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$state_file")"
    fi
    if [[ -z "$project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            "$repository_root/target/debug/dokploy" project all >"$project_output" 2>/dev/null || true
        project_id="$(jq -r --arg name "$project_name" '[.[] | select(.name == $name)] | if length == 1 then .[0].projectId else empty end' "$project_output" 2>/dev/null || true)"
    fi
    if [[ -n "$project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            "$repository_root/target/debug/dokploy" project remove \
            --body-project-id "$project_id" >/dev/null 2>&1 || true
    fi

    case "$workspace" in
        "$state_directory"/phase6-apply.*)
            rm -rf -- "$workspace"
            ;;
        *)
            echo "refusing to remove an unexpected integration workspace" >&2
            ;;
    esac
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
    --auto-approve
"$repository_root/target/debug/dokploy" plan \
    --file "$config_file" \
    --detailed-exitcode
