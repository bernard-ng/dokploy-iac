#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=schedule-evidence.sh
source "$script_directory/schedule-evidence.sh"

umask 077
test_root="$(mktemp -d "${TMPDIR:-/tmp}/dokploy-schedule-evidence-test.XXXXXX")"
workspace="$test_root/workspace"
private_directory="$workspace/private"
api_canary='schedule-api-key-canary-must-not-remain'
command_canary='schedule-command-canary-must-not-remain'
script_canary='schedule-script-canary-must-not-remain'
fingerprint_canary='schedule-fingerprint-canary-must-not-remain'
trap 'find "$test_root" -depth -delete' EXIT INT TERM
mkdir -p "$private_directory"
chmod 700 "$test_root" "$workspace" "$private_directory"

printf 'x-api-key: %s\n' "$api_canary" >"$private_directory/api-header"
jq -n --arg command "$command_canary" --arg script "$script_canary" \
    '{command:$command,script:$script}' \
    >"$private_directory/schedule-one.raw.json"
printf '%s\n' "$command_canary" >"$private_directory/command"
printf '%s\n' "$script_canary" >"$private_directory/script"
jq -n '{scheduleId:"schedule-id",name:"nightly",enabled:false}' \
    >"$workspace/schedule-one.safe.json"

scrub_schedule_private_evidence "$private_directory" "$workspace"
assert_schedule_retained_evidence_secret_free \
    "$workspace" \
    "$api_canary" \
    "$command_canary" \
    "$script_canary" \
    "$fingerprint_canary"

for canary in "$command_canary" "$script_canary"; do
    printf '%s\n' "$canary" >"$workspace/leaked-evidence"
    if assert_schedule_retained_evidence_secret_free \
        "$workspace" \
        "$api_canary" \
        "$command_canary" \
        "$script_canary" \
        "$fingerprint_canary" \
        2>/dev/null; then
        echo "the Schedule evidence guard accepted an executable-text canary" >&2
        exit 1
    fi
done

if scrub_schedule_private_evidence "$workspace/elsewhere" "$workspace" 2>/dev/null; then
    echo "the Schedule scrubber accepted a directory outside the private evidence path" >&2
    exit 1
fi

echo "Schedule private evidence scrubbing excludes API keys and executable-text canaries."
