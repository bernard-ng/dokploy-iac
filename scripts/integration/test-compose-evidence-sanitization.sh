#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=compose-evidence.sh
source "$script_directory/compose-evidence.sh"

umask 077
test_root="$(mktemp -d "${TMPDIR:-/tmp}/dokploy-compose-evidence-test.XXXXXX")"
workspace="$test_root/workspace"
private_directory="$workspace/private"
api_canary='compose-api-key-canary-must-not-remain'
document_canary='compose-document-canary-must-not-remain'
fingerprint_canary='compose-fingerprint-canary-must-not-remain'
trap 'find "$test_root" -depth -delete' EXIT INT TERM
mkdir -p "$private_directory"
chmod 700 "$test_root" "$workspace" "$private_directory"

printf 'x-api-key: %s\n' "$api_canary" >"$private_directory/api-header"
jq -n --arg document "$document_canary" '{composeFile:$document}' \
    >"$private_directory/compose-one.raw.json"
jq -n '{composeId:"compose-id",environmentId:"environment-id",composeStatus:"idle"}' \
    >"$workspace/compose-one.safe.json"

scrub_compose_private_evidence "$private_directory" "$workspace"
assert_compose_retained_evidence_secret_free \
    "$workspace" \
    "$api_canary" \
    "$document_canary" \
    "$fingerprint_canary"

printf '%s\n' "$document_canary" >"$workspace/leaked-evidence"
if assert_compose_retained_evidence_secret_free \
    "$workspace" \
    "$api_canary" \
    "$document_canary" \
    "$fingerprint_canary" \
    2>/dev/null; then
    echo "the Compose evidence guard accepted a document canary" >&2
    exit 1
fi

echo "Compose private evidence scrubbing excludes API keys and document canaries."
