#!/usr/bin/env bash
# Records the active organization singleton and restores its original values on every exit.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

capture_begin organization
restore_request="$workspace/restore.request.json"
organization_id=""

capture_cleanup() {
    if [[ -n "$organization_id" && -s "$restore_request" ]]; then
        api_request POST organization.update "$workspace/restore.json" "$restore_request" >/dev/null
    fi
}

require_runtime_version

active_status="$(api_request GET organization.active "$workspace/organization-active.before.json")"
require_status "$active_status" 200 organization.active
organization_id="$(jq -er '.id' "$workspace/organization-active.before.json")"
jq -e '.name | type == "string"' "$workspace/organization-active.before.json" >/dev/null
jq '{organizationId:.id,name:.name,logo:.logo,defaultRole:.defaultRole}' \
    "$workspace/organization-active.before.json" >"$restore_request"

# The update is full: the identity and name are required even when only the logo changes.
request_body "$workspace/partial.request.json" --arg id "$organization_id" \
    '{organizationId:$id,logo:"https://example.invalid/organization-contract.svg"}'
partial_status="$(api_request POST organization.update "$workspace/organization-update.partial-rejected.json" "$workspace/partial.request.json")"
require_status "$partial_status" 400 "organization.update (partial)"

request_body "$workspace/update.request.json" --arg id "$organization_id" '
    {organizationId:$id,name:"Organization Contract",logo:"https://example.invalid/organization-contract.svg",defaultRole:null}'
update_status="$(api_request POST organization.update "$workspace/organization-update.json" "$workspace/update.request.json")"
require_status "$update_status" 200 organization.update

updated_status="$(api_request GET organization.active "$workspace/organization-active.updated.json")"
require_status "$updated_status" 200 "organization.active after update"
jq -e --arg id "$organization_id" '
    .id == $id and .name == "Organization Contract"
    and .logo == "https://example.invalid/organization-contract.svg" and .defaultRole == null
' "$workspace/organization-active.updated.json" >/dev/null

fixtures='[
  "organization-active.before.owner.json",
  "organization-update.partial-rejected.owner.json",
  "organization-update.owner.json",
  "organization-active.updated.owner.json"
]'
jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson activeStatus "$active_status" --argjson partialStatus "$partial_status" \
    --argjson updateStatus "$update_status" --argjson fixtures "$fixtures" '
    {
      capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
      kind:"organization", singleton:true, createOperation:false, removeOperation:false,
      activeStatus:$activeStatus, updateStatus:$updateStatus, updateRequiresIdAndName:true,
      partialStatus:$partialStatus, fixtures:$fixtures
    }' >"$publish_directory/organization-contract.metadata.json"

CAPTURE_ID_KEYS="id:organization-1,ownerId:user-1" \
    capture_publish organization-active.before organization-update.partial-rejected \
        organization-update organization-active.updated
capture_succeeded=true

echo "Captured the sanitised organization contract."
