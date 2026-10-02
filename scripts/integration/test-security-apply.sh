#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=security-evidence.sh
source "$script_directory/security-evidence.sh"

api_key_file="$state_directory/api-key"
expected_image='dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8'
if [[ ! -s "$api_key_file" ]]; then
    echo "integration API key is missing; run scripts/integration/up.sh first" >&2
    exit 1
fi

container_id="$(compose ps --quiet dokploy)"
if [[ -z "$container_id" ]]; then
    echo "the local Dokploy integration container is not running" >&2
    exit 1
fi
actual_image="$(docker inspect --format '{{.Config.Image}}' "$container_id")"
if [[ "$actual_image" != "$expected_image" ]]; then
    echo "the running Dokploy image does not match the pinned v0.30.6 digest" >&2
    exit 1
fi

umask 077
workspace="$(mktemp -d "$state_directory/phase8-security-apply.XXXXXX")"
private_directory="$workspace/private"
import_directory="$workspace/import"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
project_name="phase8-security-$run_id"
config_file="$workspace/dokploy.yaml"
import_config_file="$import_directory/dokploy.yaml"
api_header_file="$private_directory/api-header"
password_file="$private_directory/admin-password"
project_output="$workspace/cleanup-projects.json"
project_id=""
environment_id=""
api_application_id=""
worker_application_id=""
security_id=""
adopted_security_id=""
declare -a observed_security_ids=()

# Generated canaries are known only to this process, the private directory, and
# the disposable server. The final scan proves none of them survives.
first_password="security-first-$(openssl rand -hex 24)"
second_password="security-second-$(openssl rand -hex 24)"
third_password="security-third-$(openssl rand -hex 24)"
adopted_password="security-adopted-$(openssl rand -hex 24)"
api_key_canary="$(tr -d '\r\n' <"$api_key_file")"

mkdir -p "$private_directory" "$import_directory"
chmod 700 "$private_directory" "$import_directory"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$api_header_file"
chmod 600 "$api_header_file"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

set_password() {
    printf '%s' "$1" >"$password_file"
    chmod 600 "$password_file"
}

# Raw responses stay in the private directory; only allowlisted projections
# are written next to the retained evidence.
api_get() {
    local endpoint="$1"
    local destination="$2"

    curl \
        --silent \
        --show-error \
        --output "$destination" \
        --write-out '%{http_code}' \
        --header "@$api_header_file" \
        "$base_url/api/$endpoint"
}

api_post() {
    local endpoint="$1"
    local body_file="$2"
    local destination="$3"

    curl \
        --silent \
        --show-error \
        --output "$destination" \
        --write-out '%{http_code}' \
        --header "@$api_header_file" \
        --header 'Content-Type: application/json' \
        --request POST \
        --data-binary "@$body_file" \
        "$base_url/api/$endpoint"
}

project_state_value() {
    local state_file="$1"
    local address="$2"

    jq -er --arg address "$address" '.resources[$address].remoteId' "$state_file"
}

# The exact password is compared privately through a file; only a presence flag
# is retained.
capture_security_one_evidence() {
    local expected_security_id="$1"
    local expected_application_id="$2"
    local expected_username="$3"
    local expected_password_file="$4"
    local destination="$5"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "security.one?securityId=$(urlencode "$expected_security_id")" "$raw_response")"
    if [[ "$status" != "200" ]]; then
        rm -f -- "$raw_response"
        echo "security.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e \
        --arg id "$expected_security_id" \
        --arg application_id "$expected_application_id" \
        --arg username "$expected_username" \
        --rawfile password "$expected_password_file" '
        .securityId == $id
        and .applicationId == $application_id
        and .username == $username
        and .password == $password
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "security.one did not prove the exact declarative Security state" >&2
        return 1
    fi
    jq '{securityId, applicationId, username, passwordPresent: ((.password // "") | length > 0)}' \
        "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

capture_security_one_absence() {
    local absent_security_id="$1"
    local destination="$2"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "security.one?securityId=$(urlencode "$absent_security_id")" "$raw_response")"
    rm -f -- "$raw_response"
    printf '%s\n' "$status" >"$destination"

    # Dokploy v0.30.6 reports a missing Security entry as HTTP 404.
    [[ "$status" == "404" ]]
}

capture_application_security() {
    local application_id="$1"
    local destination="$2"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "application.one?applicationId=$(urlencode "$application_id")" "$raw_response")"
    if [[ "$status" != "200" ]]; then
        rm -f -- "$raw_response"
        echo "application.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e --arg id "$application_id" '
        .applicationId == $id
        and .applicationStatus == "idle"
        and (.deployments | type) == "array"
        and (.deployments | length) == 0
        and (.security | type) == "array"
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "application.one did not prove an idle, undeployed application" >&2
        return 1
    fi
    jq '{
        applicationId,
        applicationStatus,
        deploymentCount: (.deployments | length),
        security: [.security[] | {securityId, applicationId, username}]
    }' "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

assert_application_security() {
    local evidence_file="$1"
    local expected_count="$2"
    local expected_security_id="${3:-}"

    if ! jq -e \
        --argjson count "$expected_count" \
        --arg id "$expected_security_id" '
        .deploymentCount == 0
        and (.security | length) == $count
        and ($id == "" or .security[0].securityId == $id)
    ' "$evidence_file" >/dev/null; then
        echo "application.one did not contain the expected authoritative Security collection" >&2
        return 1
    fi
}

cli() {
    "$repository_root/target/debug/dokploy" "$@"
}

cleanup() {
    local primary_status=$?
    local cleanup_status=0
    local cleanup_project_id="$project_id"
    local state_file="$workspace/.dokploy/state.json"
    local identity

    trap - EXIT
    set +e
    if [[ -z "$cleanup_project_id" && -s "$state_file" ]]; then
        cleanup_project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$state_file")"
    fi
    if [[ -z "$cleanup_project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            cli project all \
            >"$workspace/cleanup-projects-before.stdout" \
            2>"$workspace/cleanup-projects-before.stderr"
        if [[ "$?" -eq 0 ]]; then
            cleanup_project_id="$(jq -r --arg name "$project_name" '[.[] | select(.name == $name)] | if length == 1 then .[0].projectId else empty end' "$workspace/cleanup-projects-before.stdout")"
        else
            cleanup_status=1
        fi
    fi
    if [[ -n "$cleanup_project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
            cli project remove \
            --body-project-id "$cleanup_project_id" \
            >"$workspace/cleanup-project-remove.stdout" \
            2>"$workspace/cleanup-project-remove.stderr"
        if [[ "$?" -ne 0 ]]; then
            cleanup_status=1
        fi
    fi

    DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$(<"$api_key_file")" \
        cli project all \
        >"$project_output" \
        2>"$workspace/cleanup-projects.stderr"
    if [[ "$?" -ne 0 ]] || ! jq -e --arg name "$project_name" '[.[] | select(.name == $name)] | length == 0' "$project_output" >/dev/null; then
        echo "Security cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    for identity in "${observed_security_ids[@]}"; do
        if ! capture_security_one_absence "$identity" "$workspace/cleanup-security.$identity.status"; then
            echo "Security cleanup could not prove Security identity absence" >&2
            cleanup_status=1
        fi
    done

    if ! scrub_security_private_evidence "$private_directory" "$workspace"; then
        cleanup_status=1
    fi

    if ! assert_security_retained_evidence_secret_free \
        "$workspace" \
        "$api_key_canary" \
        "$first_password" \
        "$second_password" \
        "$third_password" \
        "$adopted_password"; then
        cleanup_status=1
    fi

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-security-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "Security integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

# The password descriptor is a workspace-relative file inside the private
# directory, so cleanup discards the only plaintext copy before the scan.
write_config() {
    local owner="$1"
    local username="$2"
    local api_security='{}'
    local worker_security='{}'
    local entry="
        security:
          admin:
            username: $username
            password:
              file: private/admin-password"

    if [[ "$owner" == "api" ]]; then
        api_security="$entry"
    else
        worker_security="$entry"
    fi

    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Security executor validation
  environments:
    production:
      description: Managed by the Phase 8 Security integration check
      applications:
        api: $api_security
        worker: $worker_security
EOF
    chmod 600 "$config_file"
}

write_removed_config() {
    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Security executor validation
  environments:
    production:
      description: Managed by the Phase 8 Security integration check
      applications:
        api: {}
        worker: {}
removed:
  - from: security.admin
    destroy: true
EOF
    chmod 600 "$config_file"
}

run_apply() {
    local label="$1"

    cli apply \
        --file "$config_file" \
        --auto-approve \
        >"$workspace/$label.stdout" \
        2>"$workspace/$label.stderr"
}

require_noop_plan() {
    local label="$1"
    local file="${2:-$config_file}"

    cli plan \
        --file "$file" \
        --json \
        --detailed-exitcode \
        >"$workspace/$label.stdout" \
        2>"$workspace/$label.stderr"
}

cargo build --manifest-path "$repository_root/Cargo.toml" --locked -p dokploy-cli

export DOKPLOY_URL="$base_url"
export DOKPLOY_API_KEY="$(<"$api_key_file")"

# 1. Create through the declarative engine and converge immediately.
set_password "$first_password"
write_config api admin
run_apply create
require_noop_plan after-create

state_file="$workspace/.dokploy/state.json"
project_id="$(project_state_value "$state_file" "project.$project_name")"
environment_id="$(project_state_value "$state_file" environment.production)"
api_application_id="$(project_state_value "$state_file" application.api)"
worker_application_id="$(project_state_value "$state_file" application.worker)"
security_id="$(project_state_value "$state_file" security.admin)"
observed_security_ids+=("$security_id")
capture_security_one_evidence "$security_id" "$api_application_id" admin "$password_file" "$workspace/security-one.created.json"
capture_application_security "$api_application_id" "$workspace/application-api.created.json"
assert_application_security "$workspace/application-api.created.json" 1 "$security_id"
capture_application_security "$worker_application_id" "$workspace/application-worker.created.json"
assert_application_security "$workspace/application-worker.created.json" 0

# 2. Replace the username and rotate the password in one apply; the physical
# identity must not change and the server must hold the exact new credentials.
set_password "$second_password"
write_config api root
run_apply update
require_noop_plan after-update
updated_security_id="$(project_state_value "$state_file" security.admin)"
if [[ "$updated_security_id" != "$security_id" ]]; then
    echo "an in-place Security update changed the physical identity" >&2
    exit 1
fi
capture_security_one_evidence "$security_id" "$api_application_id" root "$password_file" "$workspace/security-one.updated.json"
capture_application_security "$api_application_id" "$workspace/application-api.updated.json"
assert_application_security "$workspace/application-api.updated.json" 1 "$security_id"

# 3. Rotate only the password; the username stays the collision key.
set_password "$third_password"
run_apply password-rotation
require_noop_plan after-password-rotation
capture_security_one_evidence "$security_id" "$api_application_id" root "$password_file" "$workspace/security-one.rotated.json"

# 4. Changing the containing application deletes before creating.
write_config worker root
run_apply containment-replacement
require_noop_plan after-containment-replacement
replacement_security_id="$(project_state_value "$state_file" security.admin)"
if [[ "$replacement_security_id" == "$security_id" ]]; then
    echo "a containment change did not replace the physical Security entry" >&2
    exit 1
fi
observed_security_ids+=("$replacement_security_id")
if ! capture_security_one_absence "$security_id" "$workspace/security-one.replaced.status"; then
    echo "security.one did not prove absence of the replaced Security identity" >&2
    exit 1
fi
capture_security_one_evidence "$replacement_security_id" "$worker_application_id" root "$password_file" "$workspace/security-one.replacement.json"
capture_application_security "$api_application_id" "$workspace/application-api.replaced.json"
assert_application_security "$workspace/application-api.replaced.json" 0
capture_application_security "$worker_application_id" "$workspace/application-worker.replaced.json"
assert_application_security "$workspace/application-worker.replaced.json" 1 "$replacement_security_id"

# 5. Delete declaratively and prove authoritative absence.
write_removed_config
run_apply delete
if ! capture_security_one_absence "$replacement_security_id" "$workspace/security-one.deleted.status"; then
    echo "security.one did not prove absence after declarative deletion" >&2
    exit 1
fi
capture_application_security "$worker_application_id" "$workspace/application-worker.deleted.json"
assert_application_security "$workspace/application-worker.deleted.json" 0
if jq -e '.resources["security.admin"]' "$state_file" >/dev/null; then
    echo "the deleted Security identity remains in durable state" >&2
    exit 1
fi
require_noop_plan after-delete

# 6. Adopt an out-of-band entry through protected, secret-free import.
printf '%s' "$adopted_password" >"$private_directory/adopted-password"
jq -n \
    --arg applicationId "$worker_application_id" \
    --rawfile password "$private_directory/adopted-password" '{
    applicationId: $applicationId,
    username: "adopted",
    password: $password
}' >"$private_directory/out-of-band-create.request.json"
create_status="$(api_post security.create "$private_directory/out-of-band-create.request.json" "$private_directory/out-of-band-create.response.json")"
if [[ "$create_status" != "200" ]]; then
    echo "the out-of-band security.create returned HTTP $create_status" >&2
    exit 1
fi
capture_application_security "$worker_application_id" "$workspace/application-worker.out-of-band.json"
adopted_security_id="$(jq -er '.security[] | select(.username == "adopted") | .securityId' "$workspace/application-worker.out-of-band.json")"
observed_security_ids+=("$adopted_security_id")
# Import adopts the whole project; the adopted Security entry is found by its
# remote identity because its address is derived from its natural key.
cli import project "$project_id" \
    --file "$import_config_file" \
    >"$workspace/import.stdout" \
    2>"$workspace/import.stderr"
require_noop_plan after-import "$import_config_file"
if ! jq -e --arg id "$adopted_security_id" '
    [.resources[] | select(.kind == "security" and .remoteId == $id)] as $adopted
    | ($adopted | length) == 1 and $adopted[0].protected == true
' "$import_directory/.dokploy/state.json" >/dev/null; then
    echo "the imported Security entry is not protected in durable state" >&2
    exit 1
fi
if grep -q 'password' "$import_config_file"; then
    echo "the imported configuration mentions a password; it must stay unmanaged" >&2
    exit 1
fi
capture_security_one_evidence "$adopted_security_id" "$worker_application_id" adopted "$private_directory/adopted-password" "$workspace/security-one.adopted.json"
capture_application_security "$worker_application_id" "$workspace/application-worker.adopted.json"
assert_application_security "$workspace/application-worker.adopted.json" 1 "$adopted_security_id"

echo "Security declarative create, no-op convergence, credential rotation, containment replacement, deletion, absence, secret-free protected import, canary scan, and zero-deployment checks passed."
