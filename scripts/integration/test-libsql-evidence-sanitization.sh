#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=libsql-one-evidence.sh
source "$script_directory/libsql-one-evidence.sh"

umask 077
test_root="$(mktemp -d "${TMPDIR:-/tmp}/dokploy-libsql-evidence-test.XXXXXX")"
retained_workspace="$test_root/retained"
ephemeral_directory="$test_root/ephemeral"
canary='libsql-password-canary-must-not-remain'
mkdir -p "$retained_workspace" "$ephemeral_directory"
chmod 700 "$test_root" "$retained_workspace" "$ephemeral_directory"

discard_test_root() {
    rm -rf -- "$test_root"
}
trap discard_test_root EXIT

api_get() {
    local _endpoint="$1"
    local destination="$2"

    if [[ "$response_mode" == "projection" ]]; then
        printf '{"databasePassword":"%s",' "$canary" >"$destination"
    else
        jq -n --arg canary "$canary" --arg mode "$response_mode" '{
            libsqlId: "libsql-test",
            sqldNode: "primary",
            applicationStatus: (if $mode == "success" then "idle" else null end),
            databasePassword: $canary
        }' >"$destination"
    fi
    printf '200'
}

run_failure_case() (
    local label="$1"
    local response_mode="$2"
    local retained_case="$retained_workspace/$label"

    export TMPDIR="$ephemeral_directory"
    mkdir "$retained_case"
    chmod 700 "$retained_case"

    cleanup_failure_case() {
        local status=$?

        trap - EXIT
        discard_pending_libsql_raw_response
        printf 'cleanup ran\n' >"$retained_case/cleanup-ran"
        exit "$status"
    }
    trap cleanup_failure_case EXIT

    capture_libsql_one_evidence \
        'libsql.one?libsqlId=libsql-test' \
        "$retained_case/direct-proof.json"
)

assert_failure_is_scrubbed() {
    local label="$1"
    local response_mode="$2"
    local retained_case="$retained_workspace/$label"

    if run_failure_case "$label" "$response_mode"; then
        echo "invalid LibSQL evidence unexpectedly passed $label" >&2
        exit 1
    fi
    if [[ ! -s "$retained_case/cleanup-ran" ]]; then
        echo "cleanup did not run after LibSQL evidence $label failed" >&2
        exit 1
    fi
    if [[ -e "$retained_case/direct-proof.json" ]]; then
        echo "invalid LibSQL evidence was published after $label failed" >&2
        exit 1
    fi
    if grep -R -F -q -- "$canary" "$retained_case"; then
        echo "the LibSQL password canary entered retained evidence after $label failed" >&2
        exit 1
    fi
    if grep -R -F -q -- 'databasePassword' "$retained_case"; then
        echo "a raw LibSQL password field entered retained evidence after $label failed" >&2
        exit 1
    fi
}

assert_failure_is_scrubbed projection projection
assert_failure_is_scrubbed validation validation

success_directory="$retained_workspace/success"
mkdir "$success_directory"
chmod 700 "$success_directory"
response_mode=success
TMPDIR="$ephemeral_directory" capture_libsql_one_evidence \
    'libsql.one?libsqlId=libsql-test' \
    "$success_directory/direct-proof.json"
if ! jq -e '
    (keys | sort) == ["applicationStatus", "libsqlId", "sqldNode"]
    and .libsqlId == "libsql-test"
    and .sqldNode == "primary"
    and .applicationStatus == "idle"
' "$success_directory/direct-proof.json" >/dev/null; then
    echo "valid LibSQL evidence did not retain the exact allowlist" >&2
    exit 1
fi
if grep -R -F -q -- "$canary" "$success_directory"; then
    echo "the LibSQL password canary entered valid retained evidence" >&2
    exit 1
fi
if grep -R -F -q -- 'databasePassword' "$success_directory"; then
    echo "a raw LibSQL password field entered valid retained evidence" >&2
    exit 1
fi

if find "$ephemeral_directory" -type f -print -quit | grep -q .; then
    echo "a raw LibSQL response survived cleanup" >&2
    exit 1
fi

echo "LibSQL evidence validation failures retain no raw credentials."
