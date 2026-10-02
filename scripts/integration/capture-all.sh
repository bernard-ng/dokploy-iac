#!/usr/bin/env bash
#
# Runs live contract captures against the running integration instance (up.sh) and
# leaves the sanitised fixtures for the selected Dokploy version under
# fixtures/api/live/<version>/ in this checkout. It never commits anything; the
# capture workflow uploads the directory as an artifact for review (ADR 0015).
#
#   capture-all.sh              every capture
#   capture-all.sh redirect port   only these
#
# `fixtures` is the base project/application/Postgres capture. It needs an empty
# instance, runs first, and leaves its "IaC Contract Test" project behind because the
# `domain` and `redis` captures reuse it; run reset.sh and up.sh before capturing again. Every other name is capture-<name>-contract.sh.
# The tag capture records response shapes without publishing fixtures, so `all`
# leaves it out. A capture that fails does not stop the others; the exit status
# says whether all succeeded.

set -euo pipefail

if [[ "$-" == *x* ]]; then set +x; fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

if [[ ! -s "$state_directory/api-key" ]]; then
    echo "Run scripts/integration/up.sh before capturing." >&2
    exit 1
fi

requested=("$@")
selected_all=false
if [[ ${#requested[@]} -eq 0 || ( ${#requested[@]} -eq 1 && "${requested[0]}" == all ) ]]; then
    selected_all=true
    requested=(fixtures)
    for script in "$script_directory"/capture-*-contract.sh; do
        name="$(basename "$script" -contract.sh)"
        name="${name#capture-}"
        if [[ "$name" != tag ]]; then
            requested+=("$name")
        fi
    done
fi

script_for() {
    local name="$1"
    if [[ "$name" == fixtures ]]; then
        printf '%s\n' "$script_directory/capture-fixtures.sh"
    elif [[ "$name" =~ ^[a-z][a-z0-9-]*$ ]]; then
        printf '%s\n' "$script_directory/capture-$name-contract.sh"
    fi
}

for name in "${requested[@]}"; do
    script="$(script_for "$name")"
    if [[ -z "$script" || ! -x "$script" ]]; then
        echo "Unknown capture '$name'." >&2
        exit 1
    fi
done

# The capture scripts publish by replacing this directory, so a first capture of a
# version needs it to exist.
mkdir -p "$dokploy_fixture_directory"
chmod 755 "$dokploy_fixture_directory"

# A subset (or a version being captured for the first time) cannot be complete.
export DOKPLOY_FIXTURE_CHECK=partial

# Some capture scripts publish straight into the fixture directory, so a failed one can
# leave files behind. Each capture runs against a snapshot that is restored on failure.
snapshot="$(mktemp -d)"
trap 'rm -rf "$snapshot"' EXIT

failed=()
for name in "${requested[@]}"; do
    echo "::group::capture $name (Dokploy $dokploy_version)"
    cp -a "$dokploy_fixture_directory/." "$snapshot/"
    if ! "$(script_for "$name")"; then
        failed+=("$name")
        echo "capture $name FAILED; restoring the fixtures from before it ran" >&2
        rm -rf "$dokploy_fixture_directory"
        mkdir -p "$dokploy_fixture_directory"
        cp -a "$snapshot/." "$dokploy_fixture_directory/"
    fi
    rm -rf "${snapshot:?}"/* "${snapshot:?}"/.[!.]* 2>/dev/null || true
    echo "::endgroup::"
done

if ((${#failed[@]} > 0)); then
    echo "Failed captures: ${failed[*]}" >&2
    exit 1
fi

if [[ "$selected_all" == true ]]; then
    DOKPLOY_FIXTURE_CHECK=full "$script_directory/check-fixtures.sh"
fi

echo "Captured ${requested[*]} for Dokploy $dokploy_version in $dokploy_fixture_directory"
