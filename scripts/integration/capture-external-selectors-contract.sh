#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
fixture_directory="$repository_root/fixtures/api/live"
if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before capturing external selectors." >&2
    exit 1
fi

umask 077
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/external-selector-contract-$run_id"
raw_directory="$workspace/private"
candidate_fixture_root="$workspace/candidate/api/live"
candidate_versioned_fixture_directory="$candidate_fixture_root/$dokploy_version"
published_fixture_backup="$workspace/published-fixtures.backup"
mkdir -p "$raw_directory" "$candidate_versioned_fixture_directory"
chmod 700 "$workspace" "$raw_directory"

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

capture_succeeded=false
publication_started=false
publication_complete=false

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/external-selector-contract-*) ;;
        *) echo "Refusing to delete an unexpected selector capture workspace." >&2; return 1 ;;
    esac
    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?"
    local restore_ready=true
    trap - EXIT INT TERM
    set +e
    if [[ "$publication_started" == true && "$publication_complete" == false ]]; then
        if [[ -d "$published_fixture_backup" ]]; then
            if [[ -e "$fixture_directory" ]] \
                && ! mv "$fixture_directory" "$workspace/failed-publication"
            then
                echo "Could not quarantine partially published fixtures." >&2
                restore_ready=false
                exit_code=1
            fi
            if [[ "$restore_ready" == true && ! -e "$fixture_directory" ]]; then
                mv "$published_fixture_backup" "$fixture_directory" || exit_code=1
            else
                echo "Could not restore the previous fixture directory." >&2
                exit_code=1
            fi
        else
            echo "Fixture publication lacks a recoverable backup." >&2
            exit_code=1
        fi
    fi
    if [[ "$capture_succeeded" == true ]]; then
        discard_private_workspace || exit_code=1
    else
        echo "External-selector capture evidence remains in $workspace" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

api_get() {
    local endpoint="$1" destination="$2"
    curl --silent --show-error --output "$destination" --write-out '%{http_code}' \
        --header "@$auth_header_file" "$base_url/api/$endpoint"
}

response_code="$(api_get settings.getDokployVersion "$raw_directory/version.json")"
if [[ "$response_code" != 200 ]] \
    || ! jq -e --arg version "$dokploy_version" '. == $version' "$raw_directory/version.json" >/dev/null
then
    echo "External-selector capture requires Dokploy $dokploy_version." >&2
    exit 1
fi

for endpoint in server.all registry.all destination.all; do
    response_code="$(api_get "$endpoint" "$raw_directory/$endpoint.json")"
    if [[ "$response_code" != 200 ]]; then
        echo "$endpoint returned HTTP $response_code; expected HTTP 200." >&2
        exit 1
    fi
done

if ! jq -e '
    type == "array" and length <= 10000
    and all(.[]; (.serverId | type) == "string" and .serverId != ""
        and (.name | type) == "string" and .name != ""
        and (.serverType | type) == "string" and .serverType != "")
    and ([.[].serverId] | length == (unique | length))
' "$raw_directory/server.all.json" >/dev/null; then
    echo "server.all did not match the bounded minimal selector contract." >&2
    exit 1
fi

if ! jq -e '
    type == "array" and length <= 10000
    and all(.[]; (.registryId | type) == "string" and .registryId != ""
        and (.registryName | type) == "string" and .registryName != "")
    and ([.[].registryId] | length == (unique | length))
' "$raw_directory/registry.all.json" >/dev/null; then
    echo "registry.all did not match the bounded minimal selector contract." >&2
    exit 1
fi

if ! jq -e '
    type == "array" and length <= 10000
    and all(.[]; (.destinationId | type) == "string" and .destinationId != ""
        and (.name | type) == "string" and .name != "")
    and ([.[].destinationId] | length == (unique | length))
' "$raw_directory/destination.all.json" >/dev/null; then
    echo "destination.all did not match the bounded minimal selector contract." >&2
    exit 1
fi

if [[ -d "$fixture_directory" ]]; then
    cp -a "$fixture_directory/." "$candidate_fixture_root/"
fi

jq '
    reduce .[] as $item ({names:{},next:1,items:[]};
        ($item.name) as $name
        | if .names[$name] == null then
            .names[$name] = ("server-name-" + (.next | tostring)) | .next += 1
          else . end
        | .items += [{
            serverId:("server-selector-" + ((.items | length) + 1 | tostring)),
            name:.names[$name],serverType:$item.serverType
        }]
    ) | .items
' "$raw_directory/server.all.json" \
    >"$candidate_versioned_fixture_directory/server-all.selectors.json"

jq '
    reduce .[] as $item ({names:{},next:1,items:[]};
        ($item.registryName) as $name
        | if .names[$name] == null then
            .names[$name] = ("registry-name-" + (.next | tostring)) | .next += 1
          else . end
        | .items += [{
            registryId:("registry-selector-" + ((.items | length) + 1 | tostring)),
            registryName:.names[$name]
        }]
    ) | .items
' "$raw_directory/registry.all.json" \
    >"$candidate_versioned_fixture_directory/registry-all.selectors.json"

jq '
    reduce .[] as $item ({names:{},next:1,items:[]};
        ($item.name) as $name
        | if .names[$name] == null then
            .names[$name] = ("destination-name-" + (.next | tostring)) | .next += 1
          else . end
        | .items += [{
            destinationId:("destination-selector-" + ((.items | length) + 1 | tostring)),
            name:.names[$name]
        }]
    ) | .items
' "$raw_directory/destination.all.json" \
    >"$candidate_versioned_fixture_directory/destination-all.selectors.json"

jq -n \
    --argjson servers "$(jq 'length' "$raw_directory/server.all.json")" \
    --argjson registries "$(jq 'length' "$raw_directory/registry.all.json")" \
    --argjson destinations "$(jq 'length' "$raw_directory/destination.all.json")" \
    --arg version "$dokploy_version" '
    {
        version:$version,sanitized:true,mutations:0,
        endpoints:["server.all","registry.all","destination.all"],
        boundedItemLimit:10000,
        duplicateIdsRejected:true,
        duplicateNamesPreserved:true,
        sensitiveFieldsExcluded:true,
        observedCounts:{servers:$servers,registries:$registries,destinations:$destinations}
    }
' >"$candidate_versioned_fixture_directory/external-selector-contract.metadata.json"

find "$candidate_fixture_root" -type d -exec chmod 0755 {} +
find "$candidate_fixture_root" -type f -exec chmod 0644 {} +

DOKPLOY_FIXTURE_DIRECTORY="$candidate_fixture_root" \
    "$script_directory/check-fixtures.sh"

publication_started=true
mv "$fixture_directory" "$published_fixture_backup"
if ! mv "$candidate_fixture_root" "$fixture_directory"; then
    echo "Could not publish validated external-selector fixtures." >&2
    exit 1
fi
publication_complete=true
find "$published_fixture_backup" -depth -delete
capture_succeeded=true

echo "Captured sanitized external-selector contracts without mutations."
