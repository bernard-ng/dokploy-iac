#!/usr/bin/env bash
#
# Runs the engine against the local Dokploy for the settings kinds (tag, registry): the live
# half of the contract the simulator copies (ADR 0015). Needs Docker and a fresh instance
# (run reset.sh and up.sh first). Starts a throwaway registry on 127.0.0.1:5000 because
# Dokploy runs `docker login` when it creates a registry.

set -euo pipefail

if [[ "$-" == *x* ]]; then set +x; fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before the live engine test." >&2
    exit 1
fi

registry_image="registry@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373"
registry_port="${DOKPLOY_CAPTURE_REGISTRY_PORT:-5000}"
registry_container="dokploy-iac-live-registry"
cleanup() { docker rm --force "$registry_container" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker rm --force "$registry_container" >/dev/null 2>&1 || true
docker run --detach --rm --name "$registry_container" \
    --publish "127.0.0.1:$registry_port:5000" "$registry_image" >/dev/null
for _ in $(seq 1 30); do
    if curl --silent --fail "http://127.0.0.1:$registry_port/v2/" >/dev/null; then break; fi
    sleep 1
done

# Settings state is per instance, so each test run needs an instance without tags or registries.
export DOKPLOY_URL="$base_url"
DOKPLOY_API_KEY="$(tr -d '\r\n' <"$api_key_file")"
export DOKPLOY_API_KEY
export DOKPLOY_ENGINE_LIVE_TEST=1
export LIVE_REGISTRY_HOST="localhost:$registry_port"
cd "$repository_root"
cargo test -p dokploy-engine --test live --locked -- --ignored --test-threads=1
