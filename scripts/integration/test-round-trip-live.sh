#!/usr/bin/env bash
#
# Proves ADR 0011 against a disposable live Dokploy: seed, import, rebuild on a fresh instance,
# import again, and compare every workspace file except instance-bound state.

set -euo pipefail

if [[ "$-" == *x* ]]; then set +x; fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

ensure_swarm_manager

round_trip_directory="$(mktemp -d "${TMPDIR:-/tmp}/dokploy-round-trip.XXXXXX")"
service_snapshot="$round_trip_directory/services-before"
docker service ls --quiet >"$service_snapshot"

cleanup() {
    local service_id

    while IFS= read -r service_id; do
        if [[ -n "$service_id" ]] && ! grep --fixed-strings --line-regexp --quiet "$service_id" "$service_snapshot"; then
            docker service rm "$service_id" >/dev/null 2>&1 || true
        fi
    done < <(docker service ls --quiet 2>/dev/null || true)
    "$script_directory/reset.sh" >/dev/null 2>&1 || true
    if [[ "${DOKPLOY_KEEP_ROUND_TRIP:-0}" == "1" ]]; then
        echo "Kept round-trip workspace at $round_trip_directory"
    else
        find "$round_trip_directory" -depth -delete
    fi
}
trap cleanup EXIT

copy_workspace() {
    local source_directory="$1"
    local destination_directory="$2"

    mkdir -p "$destination_directory"
    cp -R "$source_directory/." "$destination_directory/"
}

start_clean_instance() {
    "$script_directory/reset.sh"
    "$script_directory/up.sh"
    DOKPLOY_API_KEY="$(tr -d '\r\n' <"$state_directory/api-key")"
    export DOKPLOY_API_KEY
}

hash_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

workspace_digest_manifest() {
    local workspace="$1"
    local output="$2"
    local relative_file

    (
        cd "$workspace"
        find . -type f ! -path './.dokploy/state/*' -print | LC_ALL=C sort | while IFS= read -r relative_file; do
            printf '%s  %s\n' "$(hash_file "$relative_file")" "$relative_file"
        done
    ) >"$output"
}

cli="$repository_root/target/debug/dokploy"
seed="$round_trip_directory/seed"
first="$round_trip_directory/first"
rebuild="$round_trip_directory/rebuild"
second="$round_trip_directory/second"

copy_workspace "$repository_root/fixtures/round-trip/workspace" "$seed"

export DOKPLOY_URL="$base_url"
export DOKPLOY_FINGERPRINT_KEY="${DOKPLOY_FINGERPRINT_KEY:-0199a0c8-2351-7c31-8899-2c8f81983ea5:$(openssl rand -hex 32)}"
export ROUNDTRIP_API_TOKEN="roundtrip-secret-canary"

cd "$repository_root"
cargo build --package dokploy-cli --locked

echo "Round trip: seeding Dokploy $dokploy_version"
start_clean_instance
(
    cd "$seed"
    "$cli" apply --auto-approve
    "$cli" deploy --wait --auto-approve
)
"$cli" import all --directory "$first"

echo "Round trip: rebuilding on a fresh Dokploy $dokploy_version"
copy_workspace "$first" "$rebuild"
find "$rebuild/.dokploy/state" -depth -delete
start_clean_instance
(
    cd "$rebuild"
    "$cli" apply --auto-approve --allow-disruptive
    "$cli" deploy --wait --auto-approve
)
"$cli" import all --directory "$second"

workspace_digest_manifest "$first" "$round_trip_directory/first.sha256"
workspace_digest_manifest "$second" "$round_trip_directory/second.sha256"
if ! cmp -s "$round_trip_directory/first.sha256" "$round_trip_directory/second.sha256"; then
    echo "Round-trip workspaces differ (paths and content digests):" >&2
    diff -u "$round_trip_directory/first.sha256" "$round_trip_directory/second.sha256" >&2 || true
    exit 1
fi

echo "Live round trip passed on Dokploy $dokploy_version."
