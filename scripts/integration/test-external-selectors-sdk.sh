#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before the live external-selector SDK test." >&2
    exit 1
fi

umask 077
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/external-selector-sdk-test-$run_id"
mkdir -p "$workspace"
chmod 700 "$workspace"

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/external-selector-sdk-test-*) ;;
        *) echo "Refusing to delete an unexpected selector SDK workspace." >&2; return 1 ;;
    esac
    find "$workspace" -depth -delete
}

test_succeeded=false
cleanup() {
    local exit_code="$?"
    trap - EXIT INT TERM
    set +e
    if [[ "$test_succeeded" == true ]]; then
        discard_private_workspace || exit_code=1
    else
        echo "External-selector SDK test evidence remains in $workspace" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

response_code="$(curl --silent --show-error --output "$workspace/version.json" \
    --write-out '%{http_code}' --header "@$auth_header_file" \
    "$base_url/api/settings.getDokployVersion")"
if [[ "$response_code" != 200 ]] \
    || ! jq -e '. == "v0.30.6"' "$workspace/version.json" >/dev/null
then
    echo "The live external-selector SDK test requires Dokploy v0.30.6." >&2
    exit 1
fi

cd "$repository_root"
DOKPLOY_EXTERNAL_SELECTOR_LIVE_TEST=1 \
DOKPLOY_URL="$base_url" \
DOKPLOY_API_KEY="$(<"$api_key_file")" \
    cargo test --package dokploy-sdk --test live_external_selectors --locked -- \
        --ignored --exact live_external_selector_reads_are_bounded_typed_and_unique

test_succeeded=true
echo "Live external-selector SDK reads passed without mutations."
