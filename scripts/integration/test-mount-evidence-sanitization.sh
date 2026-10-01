#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=mount-evidence.sh
source "$script_directory/mount-evidence.sh"

umask 077
test_root="$(mktemp -d "${TMPDIR:-/tmp}/dokploy-mount-evidence-test.XXXXXX")"
workspace="$test_root/workspace"
private_directory="$workspace/private"
api_canary='mount-api-key-canary-must-not-remain'
content_canary='mount-content-canary-must-not-remain'
fingerprint_canary='mount-fingerprint-canary-must-not-remain'
trap 'find "$test_root" -depth -delete' EXIT INT TERM
mkdir -p "$private_directory"
chmod 700 "$test_root" "$workspace" "$private_directory"

printf 'x-api-key: %s\n' "$api_canary" >"$private_directory/api-header"
jq -n --arg content "$content_canary" '{content:$content}' \
    >"$private_directory/mounts-one.raw.json"
printf '%s\n' "$content_canary" >"$private_directory/mount-content"
jq -n '{mountId:"mount-id",mountPath:"/data",type:"volume"}' \
    >"$workspace/mounts-one.safe.json"

scrub_mount_private_evidence "$private_directory" "$workspace"
assert_mount_retained_evidence_secret_free \
    "$workspace" \
    "$api_canary" \
    "$content_canary" \
    "$fingerprint_canary"

printf '%s\n' "$content_canary" >"$workspace/leaked-evidence"
if assert_mount_retained_evidence_secret_free \
    "$workspace" \
    "$api_canary" \
    "$content_canary" \
    "$fingerprint_canary" \
    2>/dev/null; then
    echo "the Mount evidence guard accepted a content canary" >&2
    exit 1
fi

echo "Mount private evidence scrubbing excludes API keys and file content canaries."
