#!/usr/bin/env bash
# Records the web-server singleton reads and reversible settings updates, restoring every
# changed value on exit. Traefik file writes and SSH-key replacement are intentionally not
# performed by a contract capture because their responses are already described by OpenAPI.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

capture_begin web-server
restore_ready=false

capture_cleanup() {
    if [[ "$restore_ready" != true ]]; then
        return
    fi
    api_request POST settings.updateBuildsConcurrency "$workspace/restore-builds.json" \
        "$workspace/restore-builds.request.json" >/dev/null
    api_request POST settings.updateDockerCleanup "$workspace/restore-docker.json" \
        "$workspace/restore-docker.request.json" >/dev/null
    api_request POST settings.updateLogCleanup "$workspace/restore-logs.json" \
        "$workspace/restore-logs.request.json" >/dev/null
    api_request POST settings.toggleRequests "$workspace/restore-requests.json" \
        "$workspace/restore-requests.request.json" >/dev/null
}

require_runtime_version

base_status="$(api_request GET settings.getWebServerSettings "$workspace/web-server.before.json")"
require_status "$base_status" 200 settings.getWebServerSettings
jq -e '
    .id and (.serverIp | type == "string")
    and (.buildsConcurrency | type == "number")
    and (.remoteServersOnly | type == "boolean")
    and (.enforceSSO | type == "boolean")
    and (.enableDockerCleanup | type == "boolean")
    and (.logCleanupCron | type == "string")
' "$workspace/web-server.before.json" >/dev/null

requests_status="$(api_request GET settings.haveActivateRequests "$workspace/web-server-requests.before.json")"
require_status "$requests_status" 200 settings.haveActivateRequests
jq -e 'type == "boolean"' "$workspace/web-server-requests.before.json" >/dev/null

declare -a supplemental=(
    settings.haveTraefikDashboardPortEnabled
    settings.getTraefikPorts
    settings.readTraefikEnv
    settings.readTraefikConfig
    settings.readWebServerTraefikConfig
    settings.readMiddlewareTraefikConfig
)
declare -a supplemental_statuses=()
for operation in "${supplemental[@]}"; do
    name="${operation#settings.}"
    status="$(api_request GET "$operation" "$workspace/web-server-$name.json")"
    if [[ "$status" != 200 && "$status" != 500 ]]; then
        echo "$operation returned HTTP $status; expected HTTP 200 or the missing-file HTTP 500." >&2
        exit 1
    fi
    supplemental_statuses+=("$status")
done

builds="$(jq -r '.buildsConcurrency | if . == 1 then 2 else 1 end' "$workspace/web-server.before.json")"
remote="$(jq -r '.remoteServersOnly' "$workspace/web-server.before.json")"
sso="$(jq -r '.enforceSSO' "$workspace/web-server.before.json")"
docker_cleanup="$(jq -r '.enableDockerCleanup | not' "$workspace/web-server.before.json")"
request_logging="$(jq -r 'not' "$workspace/web-server-requests.before.json")"

jq '{buildsConcurrency}' "$workspace/web-server.before.json" >"$workspace/restore-builds.request.json"
jq '{enableDockerCleanup}' "$workspace/web-server.before.json" >"$workspace/restore-docker.request.json"
jq '{cronExpression:.logCleanupCron}' "$workspace/web-server.before.json" >"$workspace/restore-logs.request.json"
jq '{enable:.}' "$workspace/web-server-requests.before.json" >"$workspace/restore-requests.request.json"
restore_ready=true

request_body "$workspace/update-builds.request.json" --argjson value "$builds" '{buildsConcurrency:$value}'
request_body "$workspace/update-remote.request.json" --argjson value "$remote" '{remoteServersOnly:$value}'
request_body "$workspace/update-sso.request.json" --argjson value "$sso" '{enforceSSO:$value}'
request_body "$workspace/update-docker.request.json" --argjson value "$docker_cleanup" '{enableDockerCleanup:$value}'
request_body "$workspace/update-logs.request.json" '{cronExpression:"17 3 * * *"}'
request_body "$workspace/update-requests.request.json" --argjson value "$request_logging" '{enable:$value}'

enterprise_remote_status="$(api_request POST settings.updateRemoteServersOnly \
    "$workspace/web-server-update-remote.json" "$workspace/update-remote.request.json")"
require_status "$enterprise_remote_status" 403 settings.updateRemoteServersOnly
enterprise_sso_status="$(api_request POST settings.updateEnforceSSO \
    "$workspace/web-server-update-sso.json" "$workspace/update-sso.request.json")"
require_status "$enterprise_sso_status" 403 settings.updateEnforceSSO

declare -a update_names=(builds docker logs requests)
declare -a update_operations=(
    settings.updateBuildsConcurrency
    settings.updateDockerCleanup
    settings.updateLogCleanup
    settings.toggleRequests
)
declare -a update_statuses=()
for index in "${!update_names[@]}"; do
    name="${update_names[$index]}"
    operation="${update_operations[$index]}"
    status="$(api_request POST "$operation" "$workspace/web-server-update-$name.json" \
        "$workspace/update-$name.request.json")"
    require_status "$status" 200 "$operation"
    update_statuses+=("$status")
done

updated_status="$(api_request GET settings.getWebServerSettings "$workspace/web-server.updated.json")"
require_status "$updated_status" 200 "settings.getWebServerSettings after updates"
updated_requests_status="$(api_request GET settings.haveActivateRequests "$workspace/web-server-requests.updated.json")"
require_status "$updated_requests_status" 200 "settings.haveActivateRequests after update"
jq -e --argjson builds "$builds" --argjson remote "$remote" --argjson sso "$sso" \
    --argjson docker "$docker_cleanup" '
    .buildsConcurrency == $builds and .remoteServersOnly == $remote
    and .enforceSSO == $sso and .enableDockerCleanup == $docker
    and .logCleanupCron == "17 3 * * *"
' "$workspace/web-server.updated.json" >/dev/null
jq -e --argjson expected "$request_logging" '. == $expected' \
    "$workspace/web-server-requests.updated.json" >/dev/null

fixtures='[
  "web-server.before.owner.json",
  "web-server-requests.before.owner.json",
  "web-server-haveTraefikDashboardPortEnabled.owner.json",
  "web-server-getTraefikPorts.owner.json",
  "web-server-readTraefikEnv.owner.json",
  "web-server-readTraefikConfig.owner.json",
  "web-server-readWebServerTraefikConfig.owner.json",
  "web-server-readMiddlewareTraefikConfig.owner.json",
  "web-server-update-builds.owner.json",
  "web-server-update-remote.owner.json",
  "web-server-update-sso.owner.json",
  "web-server-update-docker.owner.json",
  "web-server-update-logs.owner.json",
  "web-server-update-requests.owner.json",
  "web-server.updated.owner.json",
  "web-server-requests.updated.owner.json"
]'
jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson baseStatus "$base_status" --argjson requestsStatus "$requests_status" \
    --argjson updatedStatus "$updated_status" --argjson updatedRequestsStatus "$updated_requests_status" \
    --argjson supplementalStatuses "$(printf '%s\n' "${supplemental_statuses[@]}" | jq -s 'map(tonumber)')" \
    --argjson updateStatuses "$(printf '%s\n' "${update_statuses[@]}" | jq -s 'map(tonumber)')" \
    --argjson enterpriseStatuses "[$enterprise_remote_status,$enterprise_sso_status]" \
    --argjson fixtures "$fixtures" '
    {
      capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
      kind:"web_server", singleton:true, createOperation:false, removeOperation:false,
      baseStatus:$baseStatus, requestsStatus:$requestsStatus,
      supplementalStatuses:$supplementalStatuses, updateStatuses:$updateStatuses,
      enterpriseStatuses:$enterpriseStatuses,
      updatedStatus:$updatedStatus, updatedRequestsStatus:$updatedRequestsStatus,
      fixtures:$fixtures
    }' >"$publish_directory/web-server-contract.metadata.json"

CAPTURE_ID_KEYS="id:web-server-1" capture_publish \
    web-server.before web-server-requests.before \
    web-server-haveTraefikDashboardPortEnabled web-server-getTraefikPorts \
    web-server-readTraefikEnv web-server-readTraefikConfig \
    web-server-readWebServerTraefikConfig web-server-readMiddlewareTraefikConfig \
    web-server-update-builds web-server-update-remote web-server-update-sso \
    web-server-update-docker web-server-update-logs web-server-update-requests \
    web-server.updated web-server-requests.updated
capture_succeeded=true

echo "Captured the sanitised web-server contract."
