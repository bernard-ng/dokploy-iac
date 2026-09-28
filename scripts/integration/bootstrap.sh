#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

prepare_runtime

api_key_file="$state_directory/api-key"

if [[ -s "$api_key_file" ]]; then
    api_key="$(<"$api_key_file")"

    if curl --fail --silent --show-error \
        --header "x-api-key: $api_key" \
        "$base_url/api/project.all" >/dev/null
    then
        echo "Dokploy integration API key is ready."
        exit 0
    fi
fi

admin_password_file="$state_directory/admin-password"
cookie_file="$state_directory/cookies.txt"

create_secret_file "$admin_password_file"

signup_body="$(
    jq -n \
        --rawfile password "$admin_password_file" \
        '{
            email: "integration@dokploy.test",
            password: ($password | rtrimstr("\n")),
            name: "Integration",
            lastName: "Test"
        }'
)"

if ! curl --fail --silent --show-error \
    --cookie-jar "$cookie_file" \
    --header 'Content-Type: application/json' \
    --header "Origin: $base_url" \
    --data "$signup_body" \
    "$base_url/api/auth/sign-up/email" \
    >"$state_directory/signup.json"
then
    echo "Could not create the disposable Dokploy administrator." >&2
    echo "If this instance was initialized outside these scripts, run scripts/integration/reset.sh first." >&2
    exit 1
fi

curl --fail --silent --show-error \
    --cookie "$cookie_file" \
    --header "Origin: $base_url" \
    "$base_url/api/organization.all" \
    >"$state_directory/organizations.json"

organization_id="$(jq -er '.[0].id' "$state_directory/organizations.json")"

api_key_body="$(
    jq -n \
        --arg organizationId "$organization_id" \
        '{
            name: "dokploy-iac-integration",
            metadata: {organizationId: $organizationId},
            rateLimitEnabled: false
        }'
)"

curl --fail --silent --show-error \
    --cookie "$cookie_file" \
    --header 'Content-Type: application/json' \
    --header "Origin: $base_url" \
    --data "$api_key_body" \
    "$base_url/api/user.createApiKey" \
    >"$state_directory/api-key-response.json"

jq -er '.key' "$state_directory/api-key-response.json" >"$api_key_file"
chmod 600 "$api_key_file"

echo "Dokploy integration administrator and API key are ready."
