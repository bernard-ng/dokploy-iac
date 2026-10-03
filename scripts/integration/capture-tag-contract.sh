#!/usr/bin/env bash
#
# Records the live contract of the Dokploy tag endpoints (a settings kind: organisation-wide,
# no project needed) as sanitised fixtures for the selected Dokploy version. A tag's create
# and update return the whole object, remove returns {"success": true}, a duplicate name is
# rejected, and update is a patch. The script deletes everything it creates.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

capture_begin tag

tag_name="tag-contract"
renamed="tag-contract-renamed"
created_ids=()

capture_cleanup() {
    local id
    for id in "${created_ids[@]:-}"; do
        [[ -n "$id" ]] || continue
        request_body "$workspace/cleanup.request.json" --arg id "$id" '{tagId:$id}'
        api_request POST tag.remove "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
    done
}

require_runtime_version

status="$(api_request GET tag.all "$workspace/tag-all.preflight.json")"
require_status "$status" 200 "tag.all before create"
if [[ "$(jq 'length' "$workspace/tag-all.preflight.json")" != 0 ]]; then
    echo "The instance already has tags; run scripts/integration/reset.sh and up.sh first." >&2
    exit 1
fi

request_body "$workspace/create.request.json" --arg name "$tag_name" '{name:$name,color:"#e11d48"}'
create_status="$(api_request POST tag.create "$workspace/tag-create.json" "$workspace/create.request.json")"
require_status "$create_status" 200 tag.create
tag_id="$(jq -er '.tagId' "$workspace/tag-create.json")"
created_ids+=("$tag_id")
jq -e --arg name "$tag_name" '.name == $name and .color == "#e11d48" and has("createdAt") and has("organizationId")' \
    "$workspace/tag-create.json" >/dev/null

all_status="$(api_request GET tag.all "$workspace/tag-all.created.json")"
require_status "$all_status" 200 "tag.all after create"
jq -e --arg id "$tag_id" 'length == 1 and .[0].tagId == $id' "$workspace/tag-all.created.json" >/dev/null

one_status="$(api_request GET "tag.one?tagId=$(urlencode "$tag_id")" "$workspace/tag-one.created.json")"
require_status "$one_status" 200 "tag.one after create"
jq -e --arg id "$tag_id" '.tagId == $id and .color == "#e11d48"' "$workspace/tag-one.created.json" >/dev/null

# A second tag with the same name is refused (the table is unique on name per organisation).
duplicate_status="$(api_request POST tag.create "$workspace/duplicate.json" "$workspace/create.request.json")"
if [[ "$duplicate_status" == 200 ]]; then
    created_ids+=("$(jq -er '.tagId' "$workspace/duplicate.json")")
fi

# Update is a patch: only the color is sent, the name stays.
request_body "$workspace/patch.request.json" --arg id "$tag_id" '{tagId:$id,color:"#16a34a"}'
patch_status="$(api_request POST tag.update "$workspace/tag-update.json" "$workspace/patch.request.json")"
require_status "$patch_status" 200 "tag.update (patch)"
jq -e --arg name "$tag_name" '.name == $name and .color == "#16a34a"' "$workspace/tag-update.json" >/dev/null
api_request GET "tag.one?tagId=$(urlencode "$tag_id")" "$workspace/tag-one.updated.json" >/dev/null
jq -e --arg name "$tag_name" '.name == $name and .color == "#16a34a"' "$workspace/tag-one.updated.json" >/dev/null

# An explicit null clears the color; a rename keeps it cleared.
request_body "$workspace/clear.request.json" --arg id "$tag_id" --arg name "$renamed" '{tagId:$id,name:$name,color:null}'
clear_status="$(api_request POST tag.update "$workspace/tag-update.cleared.json" "$workspace/clear.request.json")"
require_status "$clear_status" 200 "tag.update (clear)"
api_request GET "tag.one?tagId=$(urlencode "$tag_id")" "$workspace/tag-one.cleared.json" >/dev/null
jq -e --arg name "$renamed" '.name == $name and .color == null' "$workspace/tag-one.cleared.json" >/dev/null

request_body "$workspace/remove.request.json" --arg id "$tag_id" '{tagId:$id}'
remove_status="$(api_request POST tag.remove "$workspace/tag-remove.json" "$workspace/remove.request.json")"
require_status "$remove_status" 200 tag.remove
jq -e '.success == true' "$workspace/tag-remove.json" >/dev/null
for id in "${created_ids[@]}"; do
    [[ "$id" == "$tag_id" ]] && continue
    request_body "$workspace/cleanup.request.json" --arg id "$id" '{tagId:$id}'
    api_request POST tag.remove "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
done
created_ids=()

removed_one_status="$(api_request GET "tag.one?tagId=$(urlencode "$tag_id")" "$workspace/tag-one.removed.json")"
require_status "$removed_one_status" 404 "tag.one after remove"
removed_all_status="$(api_request GET tag.all "$workspace/tag-all.removed.json")"
require_status "$removed_all_status" 200 "tag.all after remove"
jq -e 'length == 0' "$workspace/tag-all.removed.json" >/dev/null

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson createStatus "$create_status" --argjson duplicateStatus "$duplicate_status" \
    --argjson removeStatus "$remove_status" --argjson removedOneStatus "$removed_one_status" '
    {
        capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
        createReturns:"object", updateReturns:"object", removeReturns:"success",
        updateIsPatch:true, nullClearsColor:true, uniqueNamePerOrganization:true,
        createStatus:$createStatus, duplicateStatus:$duplicateStatus,
        removeStatus:$removeStatus, removedOneStatus:$removedOneStatus
    }' >"$publish_directory/tag-contract.metadata.json"

capture_publish tag-create tag-all.created tag-one.created tag-update tag-one.updated \
    tag-update.cleared tag-one.cleared tag-remove tag-one.removed tag-all.removed
capture_succeeded=true

echo "Captured the sanitised tag contract."
