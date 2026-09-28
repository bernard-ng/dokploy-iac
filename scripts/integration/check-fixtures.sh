#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture_directory="$repository_root/fixtures/api/live"

if [[ ! -d "$fixture_directory" ]]; then
    echo "No live fixtures found." >&2
    exit 1
fi

unsafe_values="$(
    find "$fixture_directory" -type f -name '*.json' -print0 \
    | xargs -0 jq -r '
        paths(scalars) as $path
        | ($path[-1] | tostring) as $key
        | getpath($path) as $value
        | select(
            $key == "env"
            or $key == "previewEnv"
            or $key == "buildArgs"
            or $key == "previewBuildArgs"
            or $key == "buildSecrets"
            or $key == "previewBuildSecrets"
            or ($key | test("(?i)(password|secret|token|privatekey|accesskey)"))
        )
        | select($value != null and $value != "" and $value != "<redacted>")
        | $path | map(tostring) | join(".")
    ' 2>/dev/null || true
)"

if [[ -n "$unsafe_values" ]]; then
    echo "Live fixtures contain unredacted secret-like values:" >&2
    echo "$unsafe_values" >&2
    exit 1
fi

echo "Live fixture secret checks passed."
