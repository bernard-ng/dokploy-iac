#!/usr/bin/env bash
#
# Offline test of check-fixtures.sh modes (needs jq only). The capture workflow checks
# in `partial` mode while a version is being captured; that must tolerate kinds that are
# not captured yet but never weaken the content and leak checks.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "$script_directory/../.." && pwd)"
checker="$script_directory/check-fixtures.sh"

workspace="$(mktemp -d)"
trap 'rm -rf "$workspace"' EXIT

fresh_copy() {
    rm -rf "$workspace/live"
    mkdir -p "$workspace/live"
    cp -R "$repository_root/fixtures/api/live/v0.30.6" "$workspace/live/v0.30.6"
    find "$workspace/live" -type d -exec chmod 755 {} +
    find "$workspace/live" -type f -exec chmod 644 {} +
}

check() {
    local mode="$1"
    DOKPLOY_FIXTURE_DIRECTORY="$workspace/live" DOKPLOY_FIXTURE_CHECK="$mode" \
        "$checker" >/dev/null 2>"$workspace/stderr"
}

expect_pass() {
    if ! check "$1"; then
        echo "FAIL: $2 should pass in $1 mode" >&2
        cat "$workspace/stderr" >&2
        exit 1
    fi
}

expect_fail() {
    if check "$1"; then
        echo "FAIL: $2 should fail in $1 mode" >&2
        exit 1
    fi
}

fresh_copy
expect_pass full "the committed fixtures"
expect_pass partial "the committed fixtures"

fresh_copy
rm "$workspace"/live/v0.30.6/redirect-*.json
expect_fail full "a version without the Redirect fixtures"
expect_pass partial "a version without the Redirect fixtures"

fresh_copy
jq '.mounts[0].env = "unredacted-value"' "$workspace/live/v0.30.6/postgres-one.owner.json" \
    >"$workspace/changed.json" 2>/dev/null \
    || jq '.env = "unredacted-value"' "$workspace/live/v0.30.6/postgres-one.owner.json" \
        >"$workspace/changed.json"
cp "$workspace/changed.json" "$workspace/live/v0.30.6/postgres-one.owner.json"
expect_fail full "a fixture with an unredacted env value"
expect_fail partial "a fixture with an unredacted env value"

fresh_copy
chmod 600 "$workspace/live/v0.30.6/redirect-one.created.owner.json"
expect_fail partial "a fixture with the wrong file mode"

fresh_copy
if DOKPLOY_FIXTURE_DIRECTORY="$workspace/live" DOKPLOY_FIXTURE_CHECK=sloppy "$checker" >/dev/null 2>&1; then
    echo "FAIL: an unknown mode should be refused" >&2
    exit 1
fi

echo "check-fixtures.sh modes behave."
