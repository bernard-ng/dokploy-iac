#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"

if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before the live SDK tests." >&2
    exit 1
fi

api_key="$(<"$api_key_file")"

if ! curl --fail --silent --show-error \
    --header "x-api-key: $api_key" \
    "$base_url/api/settings.getDokployVersion" \
    | jq --exit-status '. == "v0.30.6"' \
        >/dev/null
then
    echo "The live SDK tests require the pinned local Dokploy v0.30.6 instance." >&2
    exit 1
fi

if ! curl --fail --silent --show-error \
    --header "x-api-key: $api_key" \
    "$base_url/api/project.all" \
    | jq --exit-status \
        'length > 0
        and any(.[]?.environments[]?.applications[]?; .applicationId != null)
        and any(.[]?.environments[]?.postgres[]?; .postgresId != null)' \
        >/dev/null
then
    echo "The live SDK tests require the populated application and Postgres fixtures created by scripts/integration/capture-fixtures.sh." >&2
    exit 1
fi

cd "$repository_root"

DOKPLOY_SDK_LIVE_TEST=1 \
DOKPLOY_URL="$base_url" \
DOKPLOY_API_KEY="$api_key" \
    cargo test --package dokploy-sdk --test live -- --ignored --test-threads=1
