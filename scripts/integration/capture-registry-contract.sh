#!/usr/bin/env bash
#
# Records the live contract of the Dokploy registry endpoints (a settings kind) as
# sanitised fixtures for the selected Dokploy version.
#
# Dokploy runs `docker login` against the registry URL on create, and on an update that
# changes the URL or the credentials, so the capture starts a throwaway registry on
# 127.0.0.1:5000 for it to log in to. Things the capture proves, because the engine and the
# simulator depend on them:
#   - create returns the whole object, and so does `registry.all`, password included
#     (the fixtures redact it; `registry.one` omits it);
#   - update returns `true` and is a patch: omitted fields keep their value;
#   - a rejected login on update still persists the change (HTTP 400, URL changed);
#   - two registries may share a name;
#   - remove returns the removed object.
# The script deletes everything it creates.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

registry_image="registry@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373"
registry_port="${DOKPLOY_CAPTURE_REGISTRY_PORT:-5000}"
registry_host="localhost:$registry_port"
registry_container="dokploy-iac-capture-registry"

capture_begin registry

name="registry-contract"
renamed="registry-contract-renamed"
created_ids=()

capture_cleanup() {
    local id
    for id in "${created_ids[@]:-}"; do
        [[ -n "$id" ]] || continue
        request_body "$workspace/cleanup.request.json" --arg id "$id" '{registryId:$id}'
        api_request POST registry.remove "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
    done
    docker rm --force "$registry_container" >/dev/null 2>&1
    return 0
}

docker rm --force "$registry_container" >/dev/null 2>&1 || true
docker run --detach --rm --name "$registry_container" \
    --publish "127.0.0.1:$registry_port:5000" "$registry_image" >/dev/null
for _ in $(seq 1 30); do
    if curl --silent --fail "http://127.0.0.1:$registry_port/v2/" >/dev/null; then break; fi
    sleep 1
done

require_runtime_version

status="$(api_request GET registry.all "$workspace/registry-all.preflight.json")"
require_status "$status" 200 "registry.all before create"
if [[ "$(jq 'length' "$workspace/registry-all.preflight.json")" != 0 ]]; then
    echo "The instance already has registries; run scripts/integration/reset.sh and up.sh first." >&2
    exit 1
fi

request_body "$workspace/create.request.json" --arg name "$name" --arg url "$registry_host" '
    {registryName:$name,username:"owner",password:"capture-password-1",registryUrl:$url,
     registryType:"cloud",imagePrefix:null}'
create_status="$(api_request POST registry.create "$workspace/registry-create.json" "$workspace/create.request.json")"
require_status "$create_status" 200 registry.create
registry_id="$(jq -er '.registryId' "$workspace/registry-create.json")"
created_ids+=("$registry_id")
jq -e --arg name "$name" '.registryName == $name and .username == "owner" and .imagePrefix == null
    and has("password") and has("createdAt") and has("organizationId")' \
    "$workspace/registry-create.json" >/dev/null

all_status="$(api_request GET registry.all "$workspace/registry-all.created.json")"
require_status "$all_status" 200 "registry.all after create"
jq -e --arg id "$registry_id" 'length == 1 and .[0].registryId == $id' "$workspace/registry-all.created.json" >/dev/null

one_status="$(api_request GET "registry.one?registryId=$(urlencode "$registry_id")" "$workspace/registry-one.created.json")"
require_status "$one_status" 200 "registry.one after create"
jq -e --arg id "$registry_id" '.registryId == $id and (has("password") | not)' \
    "$workspace/registry-one.created.json" >/dev/null

# Update is a patch: only the prefix is sent, everything else stays.
request_body "$workspace/patch.request.json" --arg id "$registry_id" '{registryId:$id,imagePrefix:"team"}'
patch_status="$(api_request POST registry.update "$workspace/registry-update.json" "$workspace/patch.request.json")"
require_status "$patch_status" 200 "registry.update (patch)"
jq -e '. == true' "$workspace/registry-update.json" >/dev/null
api_request GET "registry.one?registryId=$(urlencode "$registry_id")" "$workspace/registry-one.updated.json" >/dev/null
jq -e --arg name "$name" '.registryName == $name and .imagePrefix == "team" and .username == "owner"' \
    "$workspace/registry-one.updated.json" >/dev/null

# A full update changes several fields and the credentials.
request_body "$workspace/full.request.json" --arg id "$registry_id" --arg name "$renamed" --arg url "$registry_host" '
    {registryId:$id,registryName:$name,username:"operator",password:"capture-password-2",
     registryUrl:$url,imagePrefix:null}'
full_status="$(api_request POST registry.update "$workspace/registry-update.full.json" "$workspace/full.request.json")"
require_status "$full_status" 200 "registry.update (full)"
api_request GET "registry.one?registryId=$(urlencode "$registry_id")" "$workspace/registry-one.full-updated.json" >/dev/null
jq -e --arg name "$renamed" '.registryName == $name and .username == "operator" and .imagePrefix == null' \
    "$workspace/registry-one.full-updated.json" >/dev/null

# A URL the daemon cannot log in to is rejected (400), and the change is persisted anyway.
request_body "$workspace/login.request.json" --arg id "$registry_id" '{registryId:$id,registryUrl:"unreachable.invalid"}'
login_status="$(api_request POST registry.update "$workspace/login-failure.json" "$workspace/login.request.json")"
require_status "$login_status" 400 "registry.update (unreachable registry)"
api_request GET "registry.one?registryId=$(urlencode "$registry_id")" "$workspace/registry-one.login-failed.json" >/dev/null
login_persisted=false
if jq -e '.registryUrl == "unreachable.invalid"' "$workspace/registry-one.login-failed.json" >/dev/null; then
    login_persisted=true
fi
request_body "$workspace/restore.request.json" --arg id "$registry_id" --arg url "$registry_host" '{registryId:$id,registryUrl:$url}'
api_request POST registry.update "$workspace/restore.json" "$workspace/restore.request.json" >/dev/null

# Registries are not unique by name.
duplicate_request_ok=false
request_body "$workspace/duplicate.request.json" --arg name "$renamed" --arg url "$registry_host" '
    {registryName:$name,username:"operator",password:"capture-password-3",registryUrl:$url,
     registryType:"cloud",imagePrefix:null}'
duplicate_status="$(api_request POST registry.create "$workspace/duplicate.json" "$workspace/duplicate.request.json")"
if [[ "$duplicate_status" == 200 ]]; then
    duplicate_request_ok=true
    created_ids+=("$(jq -er '.registryId' "$workspace/duplicate.json")")
fi

request_body "$workspace/remove.request.json" --arg id "$registry_id" '{registryId:$id}'
remove_status="$(api_request POST registry.remove "$workspace/registry-remove.json" "$workspace/remove.request.json")"
require_status "$remove_status" 200 registry.remove
for id in "${created_ids[@]}"; do
    [[ "$id" == "$registry_id" ]] && continue
    request_body "$workspace/cleanup.request.json" --arg id "$id" '{registryId:$id}'
    api_request POST registry.remove "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
done
created_ids=()

removed_one_status="$(api_request GET "registry.one?registryId=$(urlencode "$registry_id")" "$workspace/registry-one.removed.json")"
require_status "$removed_one_status" 404 "registry.one after remove"
removed_all_status="$(api_request GET registry.all "$workspace/registry-all.removed.json")"
require_status "$removed_all_status" 200 "registry.all after remove"
jq -e 'length == 0' "$workspace/registry-all.removed.json" >/dev/null

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson createStatus "$create_status" --argjson loginStatus "$login_status" \
    --argjson loginPersisted "$login_persisted" --argjson duplicateAllowed "$duplicate_request_ok" \
    --argjson removeStatus "$remove_status" --argjson removedOneStatus "$removed_one_status" '
    {
        capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
        createReturns:"object", updateReturns:"true", updateIsPatch:true,
        listingIncludesCredential:true, detailOmitsCredential:true,
        loginOnCreate:true, rejectedLoginStatus:$loginStatus, rejectedLoginPersistsChange:$loginPersisted,
        duplicateNamesAllowed:$duplicateAllowed,
        createStatus:$createStatus, removeStatus:$removeStatus, removedOneStatus:$removedOneStatus
    }' >"$publish_directory/registry-contract.metadata.json"

capture_publish registry-create registry-all.created registry-one.created registry-update \
    registry-one.updated registry-update.full registry-one.full-updated registry-one.login-failed \
    registry-remove registry-one.removed registry-all.removed
capture_succeeded=true

echo "Captured the sanitised registry contract."
