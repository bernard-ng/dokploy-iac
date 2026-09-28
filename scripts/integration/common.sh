#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
runtime_directory="$repository_root/.integration"
secret_directory="$runtime_directory/secrets"
state_directory="$runtime_directory/state"
compose_file="$repository_root/compose.integration.yaml"
integration_port="${DOKPLOY_INTEGRATION_PORT:-33000}"
base_url="http://127.0.0.1:$integration_port"

compose() {
    docker compose --file "$compose_file" "$@"
}

create_secret_file() {
    local destination="$1"

    if [[ -f "$destination" ]]; then
        return
    fi

    openssl rand -hex 32 >"$destination"
    chmod 600 "$destination"
}

prepare_runtime() {
    umask 077
    mkdir -p "$secret_directory" "$state_directory"
    create_secret_file "$secret_directory/postgres_password"
    create_secret_file "$secret_directory/auth_secret"
}
