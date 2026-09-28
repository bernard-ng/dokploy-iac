#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
fixture_file="$state_directory/fixtures/postgres-one.json"

if [[ ! -s "$api_key_file" || ! -s "$fixture_file" ]]; then
    echo "Live integration state is missing; run scripts/integration/up.sh and scripts/integration/capture-fixtures.sh first." >&2
    exit 1
fi

postgres_id="$(jq -er '.postgresId' "$fixture_file")"
output_file="$(mktemp "$runtime_directory/cli-redaction.XXXXXX.json")"
trap 'rm -f "$output_file"' EXIT
chmod 600 "$output_file"

DOKPLOY_URL="$base_url" \
    DOKPLOY_API_KEY="$(<"$api_key_file")" \
    cargo run --quiet --manifest-path "$repository_root/Cargo.toml" \
        --package dokploy-cli \
        --bin dokploy \
        -- postgres one --query-postgres-id "$postgres_id" \
        >"$output_file"

if ! jq -e --slurpfile raw "$fixture_file" '
    .postgresId == $raw[0].postgresId
    and .environmentId == $raw[0].environmentId
    and .databasePassword == "[REDACTED]"
    and ([.. | strings | select(. == $raw[0].databasePassword)] | length == 0)
' "$output_file" >/dev/null
then
    echo "The live CLI response did not satisfy the redaction contract." >&2
    exit 1
fi

echo "Live CLI response redaction passed."
