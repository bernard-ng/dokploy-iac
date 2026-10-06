#!/usr/bin/env bash
# Records the read and update contract of a GitHub App installed outside the API.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

capture_begin github
provider_id="capture-github-provider-$(openssl rand -hex 6)"
github_id="capture-github-row-$(openssl rand -hex 6)"
provider_name="github-contract"

capture_cleanup() {
    docker compose --file "$compose_file" exec -T postgres \
        psql -q -v ON_ERROR_STOP=1 -U dokploy -d dokploy \
        -v provider_id="$provider_id" >/dev/null <<'SQL'
DELETE FROM git_provider WHERE "gitProviderId" = :'provider_id';
SQL
}

require_runtime_version
status="$(api_request GET gitProvider.getAll "$workspace/preflight.json")"
require_status "$status" 200 "gitProvider.getAll before seed"
if [[ "$(jq '[.[] | select(.providerType == "github")] | length' "$workspace/preflight.json")" != 0 ]]; then
    echo "The instance already has a GitHub provider; run scripts/integration/reset.sh and up.sh first." >&2
    exit 1
fi

docker compose --file "$compose_file" exec -T postgres \
    psql -q -v ON_ERROR_STOP=1 -U dokploy -d dokploy \
    -v provider_id="$provider_id" -v github_id="$github_id" -v provider_name="$provider_name" <<'SQL'
INSERT INTO git_provider
    ("gitProviderId", name, "providerType", "createdAt", "organizationId", "userId", "sharedWithOrganization")
SELECT :'provider_id', :'provider_name', 'github', '2026-10-06T00:00:00.000Z', id, owner_id, false
FROM organization
LIMIT 1;

INSERT INTO github
    ("githubId", "githubAppName", "githubAppId", "githubClientId", "githubClientSecret",
     "githubInstallationId", "githubPrivateKey", "githubWebhookSecret", "gitProviderId", "githubUrl")
VALUES
    (:'github_id', 'capture-app', 12345, 'capture-client', 'capture-client-secret',
     '67890', 'capture-private-key', 'capture-webhook-secret', :'provider_id', 'https://github.com');
SQL

api_request GET gitProvider.getAll "$workspace/github-all.created.json" >/dev/null
api_request GET github.githubProviders "$workspace/github-installed.created.json" >/dev/null
status="$(api_request GET "github.one?githubId=$(urlencode "$github_id")" "$workspace/github-one.created.json")"
require_status "$status" 200 "github.one after seed"

request_body "$workspace/update.request.json" \
    --arg githubId "$github_id" --arg gitProviderId "$provider_id" '
    {githubId:$githubId,gitProviderId:$gitProviderId,name:"github-contract-renamed",githubAppName:"capture-app-renamed"}'
update_status="$(api_request POST github.update "$workspace/github-update.json" "$workspace/update.request.json")"
require_status "$update_status" 200 github.update
api_request GET "github.one?githubId=$(urlencode "$github_id")" "$workspace/github-one.updated.json" >/dev/null

request_body "$workspace/share.request.json" --arg id "$provider_id" \
    '{gitProviderId:$id,sharedWithOrganization:true}'
share_status="$(api_request POST gitProvider.toggleShare "$workspace/github-share.json" "$workspace/share.request.json")"
require_status "$share_status" 200 gitProvider.toggleShare
api_request GET gitProvider.getAll "$workspace/github-all.shared.json" >/dev/null

fixtures='[
  "github-all.created.owner.json",
  "github-installed.created.owner.json",
  "github-one.created.owner.json",
  "github-update.owner.json",
  "github-one.updated.owner.json",
  "github-share.owner.json",
  "github-all.shared.owner.json"
]'
jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson updateStatus "$update_status" --argjson shareStatus "$share_status" \
    --argjson fixtures "$fixtures" '
    {
      capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
      kind:"github_provider", bootstrap:"database_seed_for_dashboard_installed_app",
      createOperation:false, removeOperation:false, updateStatus:$updateStatus, shareStatus:$shareStatus,
      detailExposesCredentials:true, fixtures:$fixtures
    }' >"$publish_directory/github-contract.metadata.json"

CAPTURE_ID_KEYS="gitProviderId:git-provider-1,githubId:github-1" \
    capture_publish \
        github-all.created github-installed.created github-one.created github-update \
        github-one.updated github-share github-all.shared
capture_succeeded=true
echo "Captured the sanitised GitHub provider contract."
