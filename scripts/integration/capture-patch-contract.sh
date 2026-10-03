#!/usr/bin/env bash
#
# Records the live contract of the Dokploy patch endpoints (a file edit applied to the
# repository of an application or a Compose stack) as sanitised fixtures for the selected
# Dokploy version. Things the capture proves, because the engine and the simulator depend on
# them:
#   - create answers the whole row and keeps `content` as sent, for both parents;
#   - `patch.byEntityId` is read per parent with its id and type, and is empty for the other type;
#   - update is a patch (omitted fields keep their value) and answers the whole row, but it
#     stores `content` with a trailing newline added when it has none;
#   - a second patch of the same file path for one parent is an internal error (HTTP 500);
#   - a `delete` patch still needs `content` (HTTP 400 without it);
#   - delete answers the removed row.
# The sanitiser redacts `content`, so the newline behaviour is recorded in the metadata. The
# script deletes everything it creates.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

capture_begin patch

capture_cleanup() {
    capture_remove_project
}

require_runtime_version
capture_project_setup patch

list_patches() {
    local id="$1" type="$2" destination="$3"
    api_request GET "patch.byEntityId?id=$(urlencode "$id")&type=$type" "$destination"
}

request_body "$workspace/create.request.json" --arg application "$capture_application_id" '
    {applicationId:$application,filePath:"README.md",content:"hello",type:"create",enabled:true}'
create_status="$(api_request POST patch.create "$workspace/patch-create.json" "$workspace/create.request.json")"
require_status "$create_status" 200 patch.create
patch_id="$(jq -er '.patchId' "$workspace/patch-create.json")"
jq -e --arg application "$capture_application_id" '.filePath == "README.md" and .content == "hello"
    and .type == "create" and .enabled == true and .applicationId == $application and .composeId == null
    and has("createdAt") and has("updatedAt")' "$workspace/patch-create.json" >/dev/null

request_body "$workspace/create-compose.request.json" --arg compose "$capture_compose_id" '
    {composeId:$compose,filePath:"config/app.env",content:"KEY=1",type:"update"}'
compose_status="$(api_request POST patch.create "$workspace/patch-create.compose.json" "$workspace/create-compose.request.json")"
require_status "$compose_status" 200 "patch.create (compose)"
compose_patch_id="$(jq -er '.patchId' "$workspace/patch-create.compose.json")"
jq -e --arg compose "$capture_compose_id" '.composeId == $compose and .applicationId == null and .enabled == true' \
    "$workspace/patch-create.compose.json" >/dev/null

list_patches "$capture_application_id" application "$workspace/patch-list.created.json" >/dev/null
jq -e --arg id "$patch_id" 'length == 1 and .[0].patchId == $id' "$workspace/patch-list.created.json" >/dev/null
list_patches "$capture_compose_id" compose "$workspace/patch-list.compose.json" >/dev/null
jq -e --arg id "$compose_patch_id" 'length == 1 and .[0].patchId == $id' "$workspace/patch-list.compose.json" >/dev/null
list_patches "$capture_application_id" compose "$workspace/patch-list.wrong-type.json" >/dev/null
jq -e 'length == 0' "$workspace/patch-list.wrong-type.json" >/dev/null
one_status="$(api_request GET "patch.one?patchId=$(urlencode "$patch_id")" "$workspace/patch-one.created.json")"
require_status "$one_status" 200 "patch.one after create"

# Update is a patch, and it stores the content with a trailing newline.
request_body "$workspace/content.request.json" --arg id "$patch_id" '{patchId:$id,content:"changed"}'
update_status="$(api_request POST patch.update "$workspace/patch-update.json" "$workspace/content.request.json")"
require_status "$update_status" 200 "patch.update (content)"
jq -e '.content == "changed\n" and .filePath == "README.md" and .type == "create" and .enabled == true' \
    "$workspace/patch-update.json" >/dev/null
appends_newline=true
request_body "$workspace/newline.request.json" --arg id "$patch_id" '{patchId:$id,content:"kept\n"}'
api_request POST patch.update "$workspace/patch-update.newline.json" "$workspace/newline.request.json" >/dev/null
newline_kept="$(jq -r '.content == "kept\n"' "$workspace/patch-update.newline.json")"

request_body "$workspace/full.request.json" --arg id "$patch_id" '
    {patchId:$id,filePath:"docs/guide.md",type:"update",enabled:false}'
full_status="$(api_request POST patch.update "$workspace/patch-update.full.json" "$workspace/full.request.json")"
require_status "$full_status" 200 "patch.update (path, type, enabled)"
jq -e '.filePath == "docs/guide.md" and .type == "update" and .enabled == false' "$workspace/patch-update.full.json" >/dev/null
api_request GET "patch.one?patchId=$(urlencode "$patch_id")" "$workspace/patch-one.updated.json" >/dev/null
jq -e '.filePath == "docs/guide.md" and .enabled == false' "$workspace/patch-one.updated.json" >/dev/null

# One patch per file path and parent: the second create is an internal error, not a 4xx.
request_body "$workspace/duplicate.request.json" --arg application "$capture_application_id" '
    {applicationId:$application,filePath:"docs/guide.md",content:"again",type:"create"}'
duplicate_status="$(api_request POST patch.create "$workspace/patch-create.duplicate.json" "$workspace/duplicate.request.json")"
require_status "$duplicate_status" 500 "patch.create (duplicate file path)"
list_patches "$capture_application_id" application "$workspace/patch-list.duplicate.json" >/dev/null
jq -e 'length == 1' "$workspace/patch-list.duplicate.json" >/dev/null

# A delete patch has no content to write, but the field is still required.
request_body "$workspace/delete-patch.request.json" --arg application "$capture_application_id" \
    '{applicationId:$application,filePath:"obsolete.txt",type:"delete"}'
delete_patch_status="$(api_request POST patch.create "$workspace/patch-create.delete-patch.json" "$workspace/delete-patch.request.json")"
require_status "$delete_patch_status" 400 "patch.create (delete without content)"

request_body "$workspace/remove.request.json" --arg id "$patch_id" '{patchId:$id}'
remove_status="$(api_request POST patch.delete "$workspace/patch-remove.json" "$workspace/remove.request.json")"
require_status "$remove_status" 200 patch.delete
jq -e --arg id "$patch_id" '.patchId == $id' "$workspace/patch-remove.json" >/dev/null
removed_one_status="$(api_request GET "patch.one?patchId=$(urlencode "$patch_id")" "$workspace/patch-one.removed.json")"
require_status "$removed_one_status" 404 "patch.one after delete"
list_patches "$capture_application_id" application "$workspace/patch-list.removed.json" >/dev/null
jq -e 'length == 0' "$workspace/patch-list.removed.json" >/dev/null

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson createStatus "$create_status" --argjson duplicateStatus "$duplicate_status" \
    --argjson deletePatchStatus "$delete_patch_status" --argjson removeStatus "$remove_status" \
    --argjson removedOneStatus "$removed_one_status" --argjson appendsNewline "$appends_newline" \
    --argjson newlineKept "$newline_kept" '
    {
        capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
        createReturns:"object", createKeepsContent:true, updateReturns:"object", updateIsPatch:true,
        updateAppendsTrailingNewline:$appendsNewline, updateKeepsExistingTrailingNewline:$newlineKept,
        duplicateFilePathStatus:$duplicateStatus, deletePatchNeedsContentStatus:$deletePatchStatus,
        removeReturns:"object", contentRedacted:true,
        createStatus:$createStatus, removeStatus:$removeStatus, removedOneStatus:$removedOneStatus
    }' >"$publish_directory/patch-contract.metadata.json"

capture_publish patch-create patch-create.compose patch-list.created patch-list.compose \
    patch-list.wrong-type patch-one.created patch-update patch-update.full patch-one.updated \
    patch-create.duplicate patch-list.duplicate patch-create.delete-patch patch-remove \
    patch-one.removed patch-list.removed
capture_succeeded=true

echo "Captured the sanitised patch contract."
