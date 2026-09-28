#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
raw_directory="$state_directory/fixtures"
fixture_directory="$repository_root/fixtures/api/live/v0.30.6"
sanitizer="$script_directory/sanitize-fixture.jq"

if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before capturing fixtures." >&2
    exit 1
fi

api_key="$(<"$api_key_file")"
mkdir -p "$raw_directory" "$fixture_directory"

api_get() {
    local endpoint="$1"
    local destination="$2"

    curl --fail --silent --show-error \
        --header "x-api-key: $api_key" \
        "$base_url/api/$endpoint" \
        >"$destination"
}

api_post() {
    local endpoint="$1"
    local body="$2"
    local destination="$3"

    curl --fail --silent --show-error \
        --header "x-api-key: $api_key" \
        --header 'Content-Type: application/json' \
        --data "$body" \
        "$base_url/api/$endpoint" \
        >"$destination"
}

api_get "settings.getDokployVersion" "$raw_directory/version.json"

runtime_version="$(jq -er '.' "$raw_directory/version.json")"

if [[ "$runtime_version" != "v0.30.6" ]]; then
    echo "Expected Dokploy v0.30.6, received $runtime_version." >&2
    exit 1
fi

api_get "project.all" "$raw_directory/project-all.empty.json"

if [[ "$(jq 'length' "$raw_directory/project-all.empty.json")" != "0" ]]; then
    echo "Fixture capture requires an empty instance. Run scripts/integration/reset.sh first." >&2
    exit 1
fi

api_post \
    "project.create" \
    '{"name":"IaC Contract Test","description":"Disposable integration fixture"}' \
    "$raw_directory/project-create.json"

project_id="$(jq -er '.project.projectId' "$raw_directory/project-create.json")"
environment_id="$(jq -er '.environment.environmentId' "$raw_directory/project-create.json")"

api_post \
    "application.create" \
    "$(jq -n --arg environmentId "$environment_id" '{name:"API",environmentId:$environmentId,sourceType:"github"}')" \
    "$raw_directory/application-create.json"

application_id="$(jq -er '.applicationId' "$raw_directory/application-create.json")"
database_password="$(openssl rand -hex 18)"

api_post \
    "postgres.create" \
    "$(jq -n --arg environmentId "$environment_id" --arg password "$database_password" '{name:"Main database",databaseName:"app",databaseUser:"app",databasePassword:$password,environmentId:$environmentId,dockerImage:"postgres:18"}')" \
    "$raw_directory/postgres-create.json"

postgres_id="$(jq -er '.postgresId' "$raw_directory/postgres-create.json")"

api_get "project.one?projectId=$project_id" "$raw_directory/project-one.json"
api_get "project.all" "$raw_directory/project-all.populated.json"
api_get "application.one?applicationId=$application_id" "$raw_directory/application-one.json"
api_get "postgres.one?postgresId=$postgres_id" "$raw_directory/postgres-one.json"

for fixture_name in \
    project-all.empty \
    project-create \
    project-one \
    project-all.populated \
    application-create \
    application-one \
    postgres-create \
    postgres-one
do
    jq --sort-keys --indent 2 \
        --from-file "$sanitizer" \
        "$raw_directory/$fixture_name.json" \
        >"$fixture_directory/$fixture_name.owner.json"
done

jq -n --sort-keys --indent 2 \
    --arg capturedAt "2026-09-28" \
    --arg role "owner" \
    --arg version "$runtime_version" \
    --arg image "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8" \
    '{capturedAt:$capturedAt,role:$role,version:$version,image:$image,sanitized:true}' \
    >"$fixture_directory/metadata.json"

"$script_directory/check-fixtures.sh"

echo "Captured sanitized Dokploy v0.30.6 fixtures in $fixture_directory"
