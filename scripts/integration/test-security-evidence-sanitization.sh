#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=security-evidence.sh
source "$script_directory/security-evidence.sh"

umask 077
test_root="$(mktemp -d "${TMPDIR:-/tmp}/dokploy-security-evidence-test.XXXXXX")"
workspace="$test_root/workspace"
private_directory="$workspace/private"
api_canary='security-api-key-canary-must-not-remain'
password_canary='security-password-canary-must-not-remain'
rotated_canary='security-rotated-canary-must-not-remain'
trap 'find "$test_root" -depth -delete' EXIT INT TERM
mkdir -p "$private_directory"
chmod 700 "$test_root" "$workspace" "$private_directory"

printf 'x-api-key: %s\n' "$api_canary" >"$private_directory/api-header"
printf '%s' "$password_canary" >"$private_directory/admin-password"
jq -n --arg password "$password_canary" \
    '{securityId:"security-id",username:"admin",password:$password}' \
    >"$private_directory/security-one.raw.json"
jq -n '{securityId:"security-id",applicationId:"application-id",username:"admin",passwordPresent:true}' \
    >"$workspace/security-one.safe.json"

scrub_security_private_evidence "$private_directory" "$workspace"
assert_security_retained_evidence_secret_free \
    "$workspace" \
    "$api_canary" \
    "$password_canary" \
    "$rotated_canary"

for leaked in "$api_canary" "$password_canary" "$rotated_canary"; do
    printf '%s\n' "$leaked" >"$workspace/leaked-evidence"
    if assert_security_retained_evidence_secret_free \
        "$workspace" \
        "$api_canary" \
        "$password_canary" \
        "$rotated_canary" \
        2>/dev/null; then
        echo "the Security evidence guard accepted a seeded canary" >&2
        exit 1
    fi
    rm -f "$workspace/leaked-evidence"
done

if scrub_security_private_evidence "$workspace/elsewhere" "$workspace" 2>/dev/null; then
    echo "the Security scrubber accepted an unexpected directory" >&2
    exit 1
fi

echo "Security private evidence scrubbing excludes API keys and password canaries."
