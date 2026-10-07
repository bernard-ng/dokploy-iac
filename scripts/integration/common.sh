#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=version.sh
source "$(dirname "${BASH_SOURCE[0]}")/version.sh"

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

ensure_swarm_manager() {
    local swarm_state
    local control_available

    swarm_state="$(docker info --format '{{.Swarm.LocalNodeState}}')"
    control_available="$(docker info --format '{{.Swarm.ControlAvailable}}')"
    if [[ "$swarm_state" == "inactive" ]]; then
        docker swarm init --advertise-addr 127.0.0.1 >/dev/null
        echo "Initialized a single-node Docker Swarm for deployment tests."
        return
    fi
    if [[ "$swarm_state" != "active" || "$control_available" != "true" ]]; then
        echo "The integration host must be a Docker Swarm manager." >&2
        exit 1
    fi
}
