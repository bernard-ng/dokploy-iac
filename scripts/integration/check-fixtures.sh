#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture_directory="${DOKPLOY_FIXTURE_DIRECTORY:-$repository_root/fixtures/api/live}"
versioned_fixture_directory="$fixture_directory/v0.30.6"

if [[ ! -d "$fixture_directory" ]]; then
    echo "No live fixtures found." >&2
    exit 1
fi

required_redis_fixtures=(
    "redis-create.owner.json"
    "redis-one.owner.json"
    "redis-search.populated.owner.json"
    "project-one.redis-populated.owner.json"
    "redis-remove.owner.json"
    "redis-one.removed.owner.json"
    "redis-search.removed.owner.json"
    "project-one.redis-removed.owner.json"
    "redis-contract.metadata.json"
)

for fixture_name in "${required_redis_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing Redis contract fixture: $fixture_name" >&2
        exit 1
    fi
done

if ! find "$fixture_directory" -type f -name '*.json' -print0 \
    | xargs -0 jq --exit-status '.' >/dev/null
then
    echo "Live fixtures must all contain valid JSON." >&2
    exit 1
fi

if ! jq --exit-status '
    .redisId == "redis-1"
    and .environmentId == "environment-1"
    and .name == "Redis Contract Test"
    and .appName == "redis-contract-test"
    and .databasePassword == "<redacted>"
    and .applicationStatus == "idle"
    and .serverId == null
    and .server == null
    and (.mounts | length) == 1
    and .mounts[0].redisId == "redis-1"
    and .mounts[0].mountId == "mount-1"
    and .mounts[0].volumeName == "volume-1"
    and .mounts[0].applicationId == null
    and .mounts[0].postgresId == null
' "$versioned_fixture_directory/redis-one.owner.json" >/dev/null; then
    echo "Redis detail fixture does not preserve the sanitized live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    [.environments[]? | select(.environmentId == "environment-1") | .redis[]?
        | select(
            .redisId == "redis-1"
            and .name == "Redis Contract Test"
            and .description == "Disposable Redis contract capture"
        )]
    | length == 1
' "$versioned_fixture_directory/project-one.redis-populated.owner.json" >/dev/null; then
    echo "Redis populated project fixture does not prove unique recovery." >&2
    exit 1
fi

if ! jq --exit-status '
    .total == 1
    and (.items | length) == 1
    and .items[0].redisId == "redis-1"
    and .items[0].environmentId == "environment-1"
' "$versioned_fixture_directory/redis-search.populated.owner.json" >/dev/null; then
    echo "Redis populated search fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '.items == [] and .total == 0' \
    "$versioned_fixture_directory/redis-search.removed.owner.json" >/dev/null
then
    echo "Redis cleanup search fixture is not empty." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "redis.one"
' "$versioned_fixture_directory/redis-one.removed.owner.json" >/dev/null; then
    echo "Redis cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.searchEmpty == true
    and .cleanupEvidence.projectOneAbsent == true
' "$versioned_fixture_directory/redis-contract.metadata.json" >/dev/null; then
    echo "Redis contract metadata does not prove capture and cleanup." >&2
    exit 1
fi

if ! jq --exit-status \
    '[.environments[]?.redis[]?] | length == 0' \
    "$versioned_fixture_directory/project-one.redis-removed.owner.json" >/dev/null
then
    echo "Redis cleanup project fixture still contains a Redis record." >&2
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

runtime_secret_files=(
    "$repository_root/.integration/state/api-key"
    "$repository_root/.integration/secrets/auth_secret"
    "$repository_root/.integration/secrets/postgres_password"
)

while IFS= read -r -d '' generated_password_file; do
    runtime_secret_files+=("$generated_password_file")
done < <(find "$repository_root/.integration/state" \
    -mindepth 2 \
    -maxdepth 2 \
    -type f \
    -name database-password \
    -print0 2>/dev/null || true)

for secret_file in "${runtime_secret_files[@]}"; do
    if [[ -s "$secret_file" ]] && grep -R -F -q -f "$secret_file" "$fixture_directory"; then
        echo "Live fixtures contain a local integration secret." >&2
        exit 1
    fi
done

echo "Live fixture secret checks passed."
