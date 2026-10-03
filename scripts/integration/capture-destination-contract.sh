#!/usr/bin/env bash
#
# Records the live contract of the Dokploy destination endpoints (a settings kind: an S3
# backup target) as sanitised fixtures for the selected Dokploy version. Things the capture
# proves, because the engine and the simulator depend on them:
#   - create returns the whole object, secrets included, and so do `destination.all` and
#     `destination.one` (the fixtures redact them);
#   - update needs every field (a partial body is a 400) and returns the whole object;
#   - two destinations may share a name;
#   - remove returns the removed object.
# Dokploy never contacts the endpoint on create or update, so no S3 server is needed. The
# script deletes everything it creates.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

capture_begin destination

name="destination-contract"
renamed="destination-contract-renamed"
created_ids=()

capture_cleanup() {
    local id
    for id in "${created_ids[@]:-}"; do
        [[ -n "$id" ]] || continue
        request_body "$workspace/cleanup.request.json" --arg id "$id" '{destinationId:$id}'
        api_request POST destination.remove "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
    done
}

require_runtime_version

status="$(api_request GET destination.all "$workspace/destination-all.preflight.json")"
require_status "$status" 200 "destination.all before create"
if [[ "$(jq 'length' "$workspace/destination-all.preflight.json")" != 0 ]]; then
    echo "The instance already has destinations; run scripts/integration/reset.sh and up.sh first." >&2
    exit 1
fi

request_body "$workspace/create.request.json" --arg name "$name" '
    {name:$name,provider:"",accessKey:"capture-access-key",secretAccessKey:"capture-secret-key",
     bucket:"capture-bucket",region:"us-east-1",endpoint:"http://s3.invalid:9000"}'
create_status="$(api_request POST destination.create "$workspace/destination-create.json" "$workspace/create.request.json")"
require_status "$create_status" 200 destination.create
destination_id="$(jq -er '.destinationId' "$workspace/destination-create.json")"
created_ids+=("$destination_id")
jq -e --arg name "$name" '.name == $name and .bucket == "capture-bucket" and .additionalFlags == []
    and .accessKey == "capture-access-key" and .secretAccessKey == "capture-secret-key"
    and has("createdAt") and has("organizationId")' "$workspace/destination-create.json" >/dev/null

all_status="$(api_request GET destination.all "$workspace/destination-all.created.json")"
require_status "$all_status" 200 "destination.all after create"
jq -e --arg id "$destination_id" 'length == 1 and .[0].destinationId == $id
    and .[0].secretAccessKey == "capture-secret-key"' "$workspace/destination-all.created.json" >/dev/null

one_status="$(api_request GET "destination.one?destinationId=$(urlencode "$destination_id")" "$workspace/destination-one.created.json")"
require_status "$one_status" 200 "destination.one after create"
jq -e --arg id "$destination_id" '.destinationId == $id and .accessKey == "capture-access-key"' \
    "$workspace/destination-one.created.json" >/dev/null

# Update needs every field: a body with the id and the name alone is rejected.
request_body "$workspace/partial.request.json" --arg id "$destination_id" --arg name "$renamed" '{destinationId:$id,name:$name}'
partial_status="$(api_request POST destination.update "$workspace/destination-update.partial-rejected.json" "$workspace/partial.request.json")"
require_status "$partial_status" 400 "destination.update (partial)"

request_body "$workspace/full.request.json" --arg id "$destination_id" --arg name "$renamed" '
    {destinationId:$id,name:$name,provider:"",accessKey:"capture-access-key-2",
     secretAccessKey:"capture-secret-key-2",bucket:"capture-bucket",region:"eu-west-1",
     endpoint:"http://s3.invalid:9000",additionalFlags:["--s3-force-path-style"]}'
update_status="$(api_request POST destination.update "$workspace/destination-update.json" "$workspace/full.request.json")"
require_status "$update_status" 200 "destination.update (full)"
jq -e --arg name "$renamed" '.name == $name and .region == "eu-west-1"
    and .additionalFlags == ["--s3-force-path-style"]' "$workspace/destination-update.json" >/dev/null
api_request GET "destination.one?destinationId=$(urlencode "$destination_id")" "$workspace/destination-one.updated.json" >/dev/null
jq -e --arg name "$renamed" '.name == $name and .region == "eu-west-1"' "$workspace/destination-one.updated.json" >/dev/null

# Destinations are not unique by name.
duplicate_allowed=false
request_body "$workspace/duplicate.request.json" --arg name "$renamed" '
    {name:$name,provider:"",accessKey:"capture-access-key-3",secretAccessKey:"capture-secret-key-3",
     bucket:"capture-bucket",region:"eu-west-1",endpoint:"http://s3.invalid:9000"}'
duplicate_status="$(api_request POST destination.create "$workspace/duplicate.json" "$workspace/duplicate.request.json")"
if [[ "$duplicate_status" == 200 ]]; then
    duplicate_allowed=true
    created_ids+=("$(jq -er '.destinationId' "$workspace/duplicate.json")")
fi

request_body "$workspace/remove.request.json" --arg id "$destination_id" '{destinationId:$id}'
remove_status="$(api_request POST destination.remove "$workspace/destination-remove.json" "$workspace/remove.request.json")"
require_status "$remove_status" 200 destination.remove
jq -e --arg id "$destination_id" '.destinationId == $id' "$workspace/destination-remove.json" >/dev/null
for id in "${created_ids[@]}"; do
    [[ "$id" == "$destination_id" ]] && continue
    request_body "$workspace/cleanup.request.json" --arg id "$id" '{destinationId:$id}'
    api_request POST destination.remove "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
done
created_ids=()

removed_one_status="$(api_request GET "destination.one?destinationId=$(urlencode "$destination_id")" "$workspace/destination-one.removed.json")"
require_status "$removed_one_status" 404 "destination.one after remove"
removed_all_status="$(api_request GET destination.all "$workspace/destination-all.removed.json")"
require_status "$removed_all_status" 200 "destination.all after remove"
jq -e 'length == 0' "$workspace/destination-all.removed.json" >/dev/null

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson createStatus "$create_status" --argjson partialStatus "$partial_status" \
    --argjson duplicateAllowed "$duplicate_allowed" --argjson removeStatus "$remove_status" \
    --argjson removedOneStatus "$removed_one_status" '
    {
        capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
        createReturns:"object", updateReturns:"object", updateRequiresEveryField:true,
        listingIncludesCredentials:true, detailIncludesCredentials:true, removeReturns:"object",
        duplicateNamesAllowed:$duplicateAllowed, partialUpdateStatus:$partialStatus,
        createStatus:$createStatus, removeStatus:$removeStatus, removedOneStatus:$removedOneStatus
    }' >"$publish_directory/destination-contract.metadata.json"

capture_publish destination-create destination-all.created destination-one.created \
    destination-update.partial-rejected destination-update destination-one.updated \
    destination-remove destination-one.removed destination-all.removed
capture_succeeded=true

echo "Captured the sanitised destination contract."
