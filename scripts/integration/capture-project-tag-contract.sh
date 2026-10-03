#!/usr/bin/env bash
#
# Records the live contract of the tag relation of a project (`tag.assignToProject`,
# `tag.removeFromProject`, and the `projectTags` of the project reads) as sanitised fixtures for
# the selected Dokploy version. Things the capture proves, because the engine and the simulator
# depend on them:
#   - an assigned tag is read as `projectTags: [{ id, projectId, tagId, tag: { ... } }]` on
#     `project.one` and `project.all`; assign answers the link row;
#   - assigning a tag twice is a 400, removing one that is not assigned is a success;
#   - `project.create` and `project.update` ignore a `tagIds` field;
#   - removing a tag removes its links.
# The script deletes everything it creates.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

capture_begin project-tag

tag_ids=()

capture_cleanup() {
    capture_remove_project
    local id
    for id in "${tag_ids[@]:-}"; do
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

capture_project_setup project-tag

request_body "$workspace/tag-a.request.json" '{name:"project-tag-contract-a",color:"#e11d48"}'
api_request POST tag.create "$workspace/tag-a.json" "$workspace/tag-a.request.json" >/dev/null
tag_a="$(jq -er '.tagId' "$workspace/tag-a.json")"
tag_ids+=("$tag_a")
request_body "$workspace/tag-b.request.json" '{name:"project-tag-contract-b"}'
api_request POST tag.create "$workspace/tag-b.json" "$workspace/tag-b.request.json" >/dev/null
tag_b="$(jq -er '.tagId' "$workspace/tag-b.json")"
tag_ids+=("$tag_b")

api_request GET "project.one?projectId=$(urlencode "$capture_project_id")" "$workspace/project-one.empty.json" >/dev/null
jq -e '.projectTags == []' "$workspace/project-one.empty.json" >/dev/null

request_body "$workspace/assign-a.request.json" --arg project "$capture_project_id" --arg tag "$tag_a" '{projectId:$project,tagId:$tag}'
assign_status="$(api_request POST tag.assignToProject "$workspace/project-tag-assign.json" "$workspace/assign-a.request.json")"
require_status "$assign_status" 200 tag.assignToProject
jq -e --arg project "$capture_project_id" --arg tag "$tag_a" '.projectId == $project and .tagId == $tag and has("id")' \
    "$workspace/project-tag-assign.json" >/dev/null
request_body "$workspace/assign-b.request.json" --arg project "$capture_project_id" --arg tag "$tag_b" '{projectId:$project,tagId:$tag}'
api_request POST tag.assignToProject "$workspace/project-tag-assign.b.json" "$workspace/assign-b.request.json" >/dev/null

api_request GET "project.one?projectId=$(urlencode "$capture_project_id")" "$workspace/project-one.tagged.json" >/dev/null
jq -e --arg a "$tag_a" --arg b "$tag_b" '
    (.projectTags | length) == 2
    and all(.projectTags[]; has("id") and has("projectId") and has("tagId") and (.tag.tagId == .tagId))
    and ([.projectTags[].tagId] | sort) == ([$a, $b] | sort)
    and ([.projectTags[] | select(.tagId == $a) | .tag.color] == ["#e11d48"])
    and ([.projectTags[] | select(.tagId == $b) | .tag.color] == [null])' "$workspace/project-one.tagged.json" >/dev/null
api_request GET project.all "$workspace/project-all.tagged.json" >/dev/null
jq -e --arg project "$capture_project_id" '[.[] | select(.projectId == $project) | .projectTags | length] == [2]' \
    "$workspace/project-all.tagged.json" >/dev/null

duplicate_status="$(api_request POST tag.assignToProject "$workspace/project-tag-assign.duplicate.json" "$workspace/assign-a.request.json")"
require_status "$duplicate_status" 400 "tag.assignToProject (already assigned)"

# project.update ignores a tagIds field.
request_body "$workspace/update.request.json" --arg project "$capture_project_id" '{projectId:$project,name:"project-tag-contract-project",tagIds:[]}'
update_status="$(api_request POST project.update "$workspace/project-update.json" "$workspace/update.request.json")"
require_status "$update_status" 200 "project.update (tagIds)"
api_request GET "project.one?projectId=$(urlencode "$capture_project_id")" "$workspace/project-one.after-update.json" >/dev/null
jq -e '.projectTags | length == 2' "$workspace/project-one.after-update.json" >/dev/null

request_body "$workspace/remove-a.request.json" --arg project "$capture_project_id" --arg tag "$tag_a" '{projectId:$project,tagId:$tag}'
remove_status="$(api_request POST tag.removeFromProject "$workspace/project-tag-remove.json" "$workspace/remove-a.request.json")"
require_status "$remove_status" 200 tag.removeFromProject
jq -e '.success == true' "$workspace/project-tag-remove.json" >/dev/null
api_request GET "project.one?projectId=$(urlencode "$capture_project_id")" "$workspace/project-one.untagged.json" >/dev/null
jq -e --arg b "$tag_b" '[.projectTags[].tagId] == [$b]' "$workspace/project-one.untagged.json" >/dev/null
remove_again_status="$(api_request POST tag.removeFromProject "$workspace/project-tag-remove.again.json" "$workspace/remove-a.request.json")"
require_status "$remove_again_status" 200 "tag.removeFromProject (not assigned)"

# Removing a tag removes its links.
request_body "$workspace/remove-tag.request.json" --arg tag "$tag_b" '{tagId:$tag}'
api_request POST tag.remove "$workspace/tag-remove.json" "$workspace/remove-tag.request.json" >/dev/null
api_request GET "project.one?projectId=$(urlencode "$capture_project_id")" "$workspace/project-one.tag-removed.json" >/dev/null
jq -e '.projectTags == []' "$workspace/project-one.tag-removed.json" >/dev/null

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson assignStatus "$assign_status" --argjson duplicateStatus "$duplicate_status" \
    --argjson removeStatus "$remove_status" --argjson removeAgainStatus "$remove_again_status" '
    {
        capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
        assignReturns:"link-row", projectTagsElement:"{id,projectId,tagId,tag}", readOn:["project.one","project.all"],
        assigningTwiceStatus:$duplicateStatus, removingUnassignedStatus:$removeAgainStatus,
        projectUpdateIgnoresTagIds:true, removingTagRemovesLinks:true,
        assignStatus:$assignStatus, removeStatus:$removeStatus
    }' >"$publish_directory/project-tag-contract.metadata.json"

capture_publish project-one.empty project-tag-assign project-one.tagged project-all.tagged \
    project-tag-assign.duplicate project-one.after-update project-tag-remove project-one.untagged \
    project-tag-remove.again project-one.tag-removed
capture_succeeded=true

echo "Captured the sanitised project tag contract."
