#!/usr/bin/env bash
#
# Records the live contract of the Dokploy network endpoints (a settings kind: a Docker
# network Dokploy manages) as sanitised fixtures for the selected Dokploy version. Things the
# capture proves, because the engine and the simulator depend on them:
#   - create returns the whole object and makes the Docker network; an unset `ipam` comes back
#     as `{}`, not null, and `dockerId` stays null;
#   - a second network of the same name is refused (HTTP 400, Docker's conflict);
#   - there is no update: remove returns the removed object and removes the Docker network.
# Overlay networks need Swarm, which the integration instance does not run, so they are not
# captured. The script deletes everything it creates.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

capture_begin network

plain="iac-contract-net"
configured="iac-contract-net-configured"
created_ids=()

capture_cleanup() {
    local id
    for id in "${created_ids[@]:-}"; do
        [[ -n "$id" ]] || continue
        request_body "$workspace/cleanup.request.json" --arg id "$id" '{networkId:$id}'
        api_request POST network.remove "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
    done
}

docker_network_exists() {
    docker network inspect "$1" >/dev/null 2>&1
}

require_runtime_version

status="$(api_request GET network.all "$workspace/network-all.preflight.json")"
require_status "$status" 200 "network.all before create"
if [[ "$(jq 'length' "$workspace/network-all.preflight.json")" != 0 ]]; then
    echo "The instance already has networks; run scripts/integration/reset.sh and up.sh first." >&2
    exit 1
fi

request_body "$workspace/create.request.json" --arg name "$plain" '
    {name:$name,driver:"bridge",internal:false,attachable:false,enableIPv4:true,enableIPv6:false}'
create_status="$(api_request POST network.create "$workspace/network-create.json" "$workspace/create.request.json")"
require_status "$create_status" 200 network.create
network_id="$(jq -er '.networkId' "$workspace/network-create.json")"
created_ids+=("$network_id")
jq -e --arg name "$plain" '.name == $name and .driver == "bridge" and .internal == false and .mtu == null
    and .ipam == {} and .dockerId == null and .serverId == null
    and has("createdAt") and has("organizationId")' "$workspace/network-create.json" >/dev/null
docker_network_exists "$plain"

all_status="$(api_request GET network.all "$workspace/network-all.created.json")"
require_status "$all_status" 200 "network.all after create"
jq -e --arg id "$network_id" 'length == 1 and .[0].networkId == $id' "$workspace/network-all.created.json" >/dev/null
one_status="$(api_request GET "network.one?networkId=$(urlencode "$network_id")" "$workspace/network-one.created.json")"
require_status "$one_status" 200 "network.one after create"
jq -e --arg id "$network_id" '.networkId == $id and .ipam == {}' "$workspace/network-one.created.json" >/dev/null

# The same name twice is refused, and nothing new is listed.
duplicate_status="$(api_request POST network.create "$workspace/network-duplicate.json" "$workspace/create.request.json")"
require_status "$duplicate_status" 400 "network.create (duplicate name)"
api_request GET network.all "$workspace/network-all.duplicate.json" >/dev/null
jq -e 'length == 1' "$workspace/network-all.duplicate.json" >/dev/null

# Every create-only field, with an IPAM block, comes back as written.
request_body "$workspace/configured.request.json" --arg name "$configured" '
    {name:$name,driver:"bridge",internal:true,attachable:true,enableIPv4:true,enableIPv6:false,mtu:1400,
     ipam:{driver:"default",config:[{subnet:"172.29.77.0/24",gateway:"172.29.77.1"}]}}'
configured_status="$(api_request POST network.create "$workspace/network-create.configured.json" "$workspace/configured.request.json")"
require_status "$configured_status" 200 "network.create (configured)"
configured_id="$(jq -er '.networkId' "$workspace/network-create.configured.json")"
created_ids+=("$configured_id")
jq -e '.internal == true and .attachable == true and .mtu == 1400
    and .ipam == {driver:"default",config:[{subnet:"172.29.77.0/24",gateway:"172.29.77.1"}]}' \
    "$workspace/network-create.configured.json" >/dev/null
api_request GET "network.one?networkId=$(urlencode "$configured_id")" "$workspace/network-one.configured.json" >/dev/null
jq -e '.ipam.config[0].subnet == "172.29.77.0/24" and .mtu == 1400' "$workspace/network-one.configured.json" >/dev/null

request_body "$workspace/remove.request.json" --arg id "$network_id" '{networkId:$id}'
remove_status="$(api_request POST network.remove "$workspace/network-remove.json" "$workspace/remove.request.json")"
require_status "$remove_status" 200 network.remove
jq -e --arg id "$network_id" '.networkId == $id' "$workspace/network-remove.json" >/dev/null
removes_docker_network=true
if docker_network_exists "$plain"; then removes_docker_network=false; fi
removed_one_status="$(api_request GET "network.one?networkId=$(urlencode "$network_id")" "$workspace/network-one.removed.json")"
api_request GET network.all "$workspace/network-all.removed.json" >/dev/null
jq -e --arg id "$configured_id" 'length == 1 and .[0].networkId == $id' "$workspace/network-all.removed.json" >/dev/null
created_ids=("$configured_id")

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson createStatus "$create_status" --argjson duplicateStatus "$duplicate_status" \
    --argjson removeStatus "$remove_status" --argjson removedOneStatus "$removed_one_status" \
    --argjson removesDockerNetwork "$removes_docker_network" '
    {
        capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
        createReturns:"object", removeReturns:"object", hasUpdate:false, unsetIpamReads:"{}",
        dockerIdAfterCreate:null, createMakesDockerNetwork:true, removesDockerNetwork:$removesDockerNetwork,
        duplicateNamesAllowed:false, duplicateStatus:$duplicateStatus,
        createStatus:$createStatus, removeStatus:$removeStatus, removedOneStatus:$removedOneStatus
    }' >"$publish_directory/network-contract.metadata.json"

capture_publish network-create network-all.created network-one.created network-all.duplicate \
    network-create.configured network-one.configured network-remove network-one.removed network-all.removed
capture_succeeded=true

echo "Captured the sanitised network contract."
