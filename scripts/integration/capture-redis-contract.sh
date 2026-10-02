#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
fixture_directory="$repository_root/fixtures/api/live/$dokploy_version"
sanitizer="$script_directory/sanitize-fixture.jq"

if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before capturing the Redis contract." >&2
    exit 1
fi

umask 077
mkdir -p "$state_directory"
chmod 700 "$state_directory"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/redis-contract-$run_id"
mkdir "$workspace"
chmod 700 "$workspace"

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

password_file="$workspace/database-password"
openssl rand -hex 24 | tr -d '\n' >"$password_file"
chmod 600 "$password_file"

redis_id=""
redis_name=""
ownership_description=""
project_id=""
environment_id=""
mutation_attempted=false
cleanup_confirmed=false
capture_succeeded=false
publication_started=false
publication_complete=false
published_fixture_backup="$workspace/published-fixtures.backup"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

api_request() {
    local method="$1"
    local endpoint="$2"
    local destination="$3"
    local body_file="${4:-}"
    local curl_arguments=(
        --silent
        --show-error
        --output "$destination"
        --write-out '%{http_code}'
        --header "@$auth_header_file"
        --request "$method"
    )

    if [[ -n "$body_file" ]]; then
        curl_arguments+=(
            --header 'Content-Type: application/json'
            --data-binary "@$body_file"
        )
    fi

    curl "${curl_arguments[@]}" "$base_url/api/$endpoint"
}

record_status() {
    local status="$1"
    local destination="$2"

    printf '%s\n' "$status" >"$destination"
    chmod 600 "$destination"
}

require_status() {
    local actual="$1"
    local expected="$2"
    local operation="$3"

    if [[ "$actual" != "$expected" ]]; then
        echo "$operation returned HTTP $actual; expected HTTP $expected." >&2
        return 1
    fi
}

recover_redis_id() {
    local recovery_status
    local match_count
    local recovery_attempt

    if [[ -n "$redis_id" || -z "$project_id" || -z "$redis_name" ]]; then
        return
    fi

    for recovery_attempt in 1 2 3 4 5; do
        recovery_status="$(api_request \
            GET \
            "project.one?projectId=$(urlencode "$project_id")" \
            "$workspace/project-one.recovery.json")" || recovery_status=""
        record_status "$recovery_status" "$workspace/project-one.recovery.status"

        if [[ "$recovery_status" == "200" ]]; then
            match_count="$(jq \
                --arg name "$redis_name" \
                --arg description "$ownership_description" \
                '[.environments[]?.redis[]? | select(.name == $name and .description == $description)] | length' \
                "$workspace/project-one.recovery.json")" || match_count=""

            if [[ "$match_count" == "1" ]]; then
                redis_id="$(jq -er \
                    --arg name "$redis_name" \
                    --arg description "$ownership_description" \
                    '.environments[]?.redis[]? | select(.name == $name and .description == $description) | .redisId' \
                    "$workspace/project-one.recovery.json")" || redis_id=""
                return
            fi

            if [[ -n "$match_count" && "$match_count" != "0" ]]; then
                echo "Cleanup found multiple Redis records with the ownership marker; refusing ambiguous deletion." >&2
                return
            fi
        fi

        if [[ "$recovery_attempt" != "5" ]]; then
            sleep 1
        fi
    done
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/redis-contract-*) ;;
        *)
            echo "Refusing to delete an unexpected capture workspace." >&2
            return 1
            ;;
    esac

    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?"
    local cleanup_status
    local cleanup_lookup_status
    local cleanup_body="$workspace/redis-remove.cleanup.request.json"

    trap - EXIT INT TERM
    set +e

    if [[ "$publication_started" == true && "$publication_complete" == false ]]; then
        if [[ -d "$published_fixture_backup" ]]; then
            if [[ -d "$fixture_directory" ]]; then
                mv "$fixture_directory" "$workspace/failed-publication"
            fi

            if ! mv "$published_fixture_backup" "$fixture_directory"; then
                echo "Could not restore the previous tracked fixture directory." >&2
                exit_code=1
            fi
        else
            echo "Fixture publication was interrupted without a recoverable backup." >&2
            exit_code=1
        fi
    fi

    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        recover_redis_id

        if [[ -n "$redis_id" ]]; then
            cleanup_lookup_status="$(api_request \
                GET \
                "redis.one?redisId=$(urlencode "$redis_id")" \
                "$workspace/redis-one.cleanup-before.json")"
            record_status "$cleanup_lookup_status" "$workspace/redis-one.cleanup-before.status"

            if [[ "$cleanup_lookup_status" == "200" ]]; then
                jq -n --arg redisId "$redis_id" '{redisId:$redisId}' >"$cleanup_body"
                cleanup_status="$(api_request \
                    POST \
                    "redis.remove" \
                    "$workspace/redis-remove.cleanup.json" \
                    "$cleanup_body")"
                record_status "$cleanup_status" "$workspace/redis-remove.cleanup.status"

                if [[ "$cleanup_status" != "200" ]]; then
                    echo "Redis cleanup returned HTTP $cleanup_status; inspect the private capture workspace." >&2
                fi
            elif [[ "$cleanup_lookup_status" != "404" ]]; then
                echo "Redis cleanup lookup returned HTTP $cleanup_lookup_status; inspect the private capture workspace." >&2
            fi

            cleanup_lookup_status="$(api_request \
                GET \
                "redis.one?redisId=$(urlencode "$redis_id")" \
                "$workspace/redis-one.cleanup-after.json")"
            record_status "$cleanup_lookup_status" "$workspace/redis-one.cleanup-after.status"

            if [[ "$cleanup_lookup_status" == "404" ]]; then
                cleanup_confirmed=true
            fi
        else
            echo "Redis cleanup could not recover an unambiguous owned record; inspect the private capture workspace." >&2
        fi
    fi

    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        exit_code=1
    fi

    if [[ "$capture_succeeded" == true && "$exit_code" == "0" ]]; then
        if ! discard_private_workspace; then
            echo "Could not discard the verified private capture workspace." >&2
            exit_code=1
        fi
    else
        chmod -R go-rwx "$workspace"
    fi

    exit "$exit_code"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

version_status="$(api_request GET "settings.getDokployVersion" "$workspace/version.json")"
record_status "$version_status" "$workspace/version.status"
require_status "$version_status" "200" "settings.getDokployVersion"

runtime_version="$(jq -er '.' "$workspace/version.json")"

if [[ "$runtime_version" != "$dokploy_version" ]]; then
    echo "Expected Dokploy $dokploy_version, received $runtime_version." >&2
    exit 1
fi

expected_runtime_image="$dokploy_image"
dokploy_container_id="$(compose ps --quiet dokploy)"

if [[ -z "$dokploy_container_id" ]]; then
    echo "The local Dokploy integration container is not running." >&2
    exit 1
fi

runtime_image="$(docker inspect --format '{{.Config.Image}}' "$dokploy_container_id")"

if [[ "$runtime_image" != "$expected_runtime_image" ]]; then
    echo "The running Dokploy container does not use the pinned integration image." >&2
    exit 1
fi

projects_status="$(api_request GET "project.all" "$workspace/project-all.json")"
record_status "$projects_status" "$workspace/project-all.status"
require_status "$projects_status" "200" "project.all"

project_count="$(jq '[.[] | select(.name == "IaC Contract Test")] | length' "$workspace/project-all.json")"

if [[ "$project_count" != "1" ]]; then
    echo "Redis contract capture requires exactly one IaC Contract Test project." >&2
    exit 1
fi

project_id="$(jq -er '.[] | select(.name == "IaC Contract Test") | .projectId' "$workspace/project-all.json")"
environment_count="$(jq \
    '[.[] | select(.name == "IaC Contract Test") | .environments[]? | select(.name == "production")] | length' \
    "$workspace/project-all.json")"

if [[ "$environment_count" != "1" ]]; then
    echo "Redis contract capture requires exactly one production environment." >&2
    exit 1
fi

environment_id="$(jq -er \
    '.[] | select(.name == "IaC Contract Test") | .environments[] | select(.name == "production") | .environmentId' \
    "$workspace/project-all.json")"

environment_id_encoded="$(urlencode "$environment_id")"
search_endpoint="redis.search?environmentId=$environment_id_encoded&limit=100&offset=0"
preflight_status="$(api_request GET "$search_endpoint" "$workspace/redis-search.preflight.json")"
record_status "$preflight_status" "$workspace/redis-search.preflight.status"
require_status "$preflight_status" "200" "redis.search preflight"

if ! jq -e '.items == [] and .total == 0' "$workspace/redis-search.preflight.json" >/dev/null; then
    echo "Redis contract capture requires an environment with no Redis records." >&2
    exit 1
fi

resource_suffix="$(date -u +%s)-$(openssl rand -hex 5)"
redis_name="IaC Redis Contract $resource_suffix"
app_name="iac-redis-contract-$resource_suffix"
ownership_description="dokploy-iac disposable Redis contract $resource_suffix"
create_body="$workspace/redis-create.request.json"

jq -n \
    --arg name "$redis_name" \
    --arg appName "$app_name" \
    --rawfile databasePassword "$password_file" \
    --arg environmentId "$environment_id" \
    --arg description "$ownership_description" \
    '{
        name:$name,
        appName:$appName,
        databasePassword:$databasePassword,
        dockerImage:"redis:8",
        environmentId:$environmentId,
        description:$description
    }' \
    >"$create_body"

mutation_attempted=true
create_status="$(api_request POST "redis.create" "$workspace/redis-create.json" "$create_body")"
record_status "$create_status" "$workspace/redis-create.status"
require_status "$create_status" "200" "redis.create"

redis_id="$(jq -er '.redisId' "$workspace/redis-create.json")"
redis_id_encoded="$(urlencode "$redis_id")"

one_status="$(api_request GET "redis.one?redisId=$redis_id_encoded" "$workspace/redis-one.json")"
record_status "$one_status" "$workspace/redis-one.status"
require_status "$one_status" "200" "redis.one"

if ! jq -e '
    .applicationStatus == "idle"
    and .serverId == null
    and .server == null
' "$workspace/redis-one.json" >/dev/null; then
    echo "The captured Redis record is not in the expected undeployed state." >&2
    exit 1
fi

search_status="$(api_request GET "$search_endpoint" "$workspace/redis-search.populated.json")"
record_status "$search_status" "$workspace/redis-search.populated.status"
require_status "$search_status" "200" "redis.search populated"

if ! jq -e \
    --arg redisId "$redis_id" \
    '.total == 1 and (.items | length) == 1 and .items[0].redisId == $redisId' \
    "$workspace/redis-search.populated.json" \
    >/dev/null
then
    echo "The populated Redis search response did not contain exactly the owned record." >&2
    exit 1
fi

populated_project_status="$(api_request \
    GET \
    "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.redis-populated.json")"
record_status "$populated_project_status" "$workspace/project-one.redis-populated.status"
require_status "$populated_project_status" "200" "project.one with Redis present"

if ! jq -e \
    --arg environmentId "$environment_id" \
    --arg redisId "$redis_id" \
    --arg name "$redis_name" \
    --arg description "$ownership_description" \
    '[.environments[]? | select(.environmentId == $environmentId) | .redis[]?
        | select(
            .redisId == $redisId
            and .name == $name
            and .description == $description
        )]
    | length == 1' \
    "$workspace/project-one.redis-populated.json" \
    >/dev/null
then
    echo "project.one did not expose one uniquely recoverable owned Redis record." >&2
    exit 1
fi

remove_body="$workspace/redis-remove.request.json"
jq -n --arg redisId "$redis_id" '{redisId:$redisId}' >"$remove_body"

remove_status="$(api_request POST "redis.remove" "$workspace/redis-remove.json" "$remove_body")"
record_status "$remove_status" "$workspace/redis-remove.status"
require_status "$remove_status" "200" "redis.remove"

removed_one_status="$(api_request GET "redis.one?redisId=$redis_id_encoded" "$workspace/redis-one.removed.json")"
record_status "$removed_one_status" "$workspace/redis-one.removed.status"
require_status "$removed_one_status" "404" "redis.one after removal"

removed_search_status="$(api_request GET "$search_endpoint" "$workspace/redis-search.removed.json")"
record_status "$removed_search_status" "$workspace/redis-search.removed.status"
require_status "$removed_search_status" "200" "redis.search after removal"

if ! jq -e '.items == [] and .total == 0' "$workspace/redis-search.removed.json" >/dev/null; then
    echo "Redis search was not empty after removal." >&2
    exit 1
fi

project_one_status="$(api_request \
    GET \
    "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.redis-removed.json")"
record_status "$project_one_status" "$workspace/project-one.redis-removed.status"
require_status "$project_one_status" "200" "project.one after Redis removal"

if ! jq -e \
    --arg redisId "$redis_id" \
    --arg name "$redis_name" \
    '[.environments[]?.redis[]? | select(.redisId == $redisId or .name == $name)] | length == 0' \
    "$workspace/project-one.redis-removed.json" \
    >/dev/null
then
    echo "project.one still contains the removed Redis record." >&2
    exit 1
fi

cleanup_confirmed=true

candidate_fixture_root="$workspace/candidate/api/live"
candidate_versioned_fixture_directory="$candidate_fixture_root/$dokploy_version"
mkdir -p "$candidate_versioned_fixture_directory"
cp -R "$fixture_directory/." "$candidate_versioned_fixture_directory/"

fixture_mappings=(
    "redis-create.owner.json:redis-create.json"
    "redis-one.owner.json:redis-one.json"
    "redis-search.populated.owner.json:redis-search.populated.json"
    "project-one.redis-populated.owner.json:project-one.redis-populated.json"
    "redis-remove.owner.json:redis-remove.json"
    "redis-one.removed.owner.json:redis-one.removed.json"
    "redis-search.removed.owner.json:redis-search.removed.json"
    "project-one.redis-removed.owner.json:project-one.redis-removed.json"
)

for fixture_mapping in "${fixture_mappings[@]}"; do
    fixture_name="${fixture_mapping%%:*}"
    fixture_source="${fixture_mapping#*:}"
    jq --sort-keys --indent 2 \
        --from-file "$sanitizer" \
        "$workspace/$fixture_source" \
        >"$candidate_versioned_fixture_directory/$fixture_name"
done

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" \
    --arg version "$runtime_version" \
    --arg image "$runtime_image" \
    '{
        capturedAt:$capturedAt,
        cleanupEvidence:{
            oneStatus:404,
            projectOneAbsent:true,
            searchEmpty:true
        },
        deployed:false,
        endpoints:["redis.create","redis.one","redis.search","redis.remove","project.one"],
        image:$image,
        role:"owner",
        sanitized:true,
        version:$version
    }' \
    >"$candidate_versioned_fixture_directory/redis-contract.metadata.json"

find "$candidate_fixture_root" -type d -exec chmod 755 {} +
find "$candidate_fixture_root" -type f -exec chmod 644 {} +

if grep -R -F -q -f "$api_key_file" "$candidate_fixture_root"; then
    echo "Sanitized Redis fixtures contain the local API key." >&2
    exit 1
fi

if grep -R -F -q -f "$password_file" "$candidate_fixture_root"; then
    echo "Sanitized Redis fixtures contain the generated database password." >&2
    exit 1
fi

DOKPLOY_FIXTURE_DIRECTORY="$candidate_fixture_root" \
    "$script_directory/check-fixtures.sh"

publication_started=true
mv "$fixture_directory" "$published_fixture_backup"
mv "$candidate_versioned_fixture_directory" "$fixture_directory"
publication_complete=true

capture_succeeded=true

echo "Captured and removed a disposable Redis contract without deployment."
echo "Verified private raw responses will now be discarded."
