#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

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
workspace="$(mktemp -d "$state_directory/phase8-redirect-apply.XXXXXX")"
private_directory="$workspace/private"
import_directory="$workspace/import"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
project_name="phase8-redirect-$run_id"
config_file="$workspace/dokploy.yaml"
import_config_file="$import_directory/dokploy.yaml"
api_header_file="$private_directory/api-header"
project_output="$workspace/cleanup-projects.json"
project_id=""
environment_id=""
api_application_id=""
worker_application_id=""
redirect_id=""
adopted_redirect_id=""
declare -a observed_redirect_ids=()

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

capture_redirect_one_evidence() {
    local expected_redirect_id="$1"
    local expected_application_id="$2"
    local expected_regex="$3"
    local expected_replacement="$4"
    local expected_permanent="$5"
    local destination="$6"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "redirects.one?redirectId=$(urlencode "$expected_redirect_id")" "$raw_response")"
    if [[ "$status" != "200" ]]; then
        rm -f -- "$raw_response"
        echo "redirects.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e \
        --arg id "$expected_redirect_id" \
        --arg application_id "$expected_application_id" \
        --arg regex "$expected_regex" \
        --arg replacement "$expected_replacement" \
        --argjson permanent "$expected_permanent" '
        .redirectId == $id
        and .applicationId == $application_id
        and .regex == $regex
        and .replacement == $replacement
        and .permanent == $permanent
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "redirects.one did not prove the exact declarative Redirect state" >&2
        return 1
    fi
    jq '{redirectId, applicationId, regex, replacement, permanent}' \
        "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

capture_redirect_one_absence() {
    local absent_redirect_id="$1"
    local destination="$2"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "redirects.one?redirectId=$(urlencode "$absent_redirect_id")" "$raw_response")"
    rm -f -- "$raw_response"
    printf '%s\n' "$status" >"$destination"

    # Dokploy v0.30.6 reports a missing Redirect as HTTP 404.
    [[ "$status" == "404" ]]
}

capture_application_redirects() {
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
        and (.redirects | type) == "array"
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "application.one did not prove an idle, undeployed application" >&2
        return 1
    fi
    jq '{
        applicationId,
        applicationStatus,
        deploymentCount: (.deployments | length),
        redirects: [.redirects[] | {redirectId, applicationId, regex, replacement, permanent}]
    }' "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

assert_application_redirects() {
    local evidence_file="$1"
    local expected_count="$2"
    local expected_redirect_id="${3:-}"

    if ! jq -e \
        --argjson count "$expected_count" \
        --arg id "$expected_redirect_id" '
        .deploymentCount == 0
        and (.redirects | length) == $count
        and ($id == "" or .redirects[0].redirectId == $id)
    ' "$evidence_file" >/dev/null; then
        echo "application.one did not contain the expected authoritative Redirect collection" >&2
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
        echo "Redirect cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    for identity in "${observed_redirect_ids[@]}"; do
        if ! capture_redirect_one_absence "$identity" "$workspace/cleanup-redirect.$identity.status"; then
            echo "Redirect cleanup could not prove Redirect identity absence" >&2
            cleanup_status=1
        fi
    done

    if ! find "$private_directory" -depth -delete 2>/dev/null || [[ -e "$private_directory" ]]; then
        echo "Redirect cleanup could not discard private authentication evidence" >&2
        cleanup_status=1
    fi

    if grep -R -F -q -- "$(tr -d '\r\n' <"$api_key_file")" "$workspace"; then
        echo "the integration API key appeared in retained Redirect evidence" >&2
        cleanup_status=1
    fi

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-redirect-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "Redirect integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

write_config() {
    local owner="$1"
    local replacement="$2"
    local permanent="$3"
    local api_redirects='{}'
    local worker_redirects='{}'

    if [[ "$owner" == "api" ]]; then
        api_redirects="
        redirects:
          legacy:
            regex: \"^/legacy/(.*)\$\"
            replacement: \"$replacement\"
            permanent: $permanent"
    else
        worker_redirects="
        redirects:
          legacy:
            regex: \"^/legacy/(.*)\$\"
            replacement: \"$replacement\"
            permanent: $permanent"
    fi

    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Redirect executor validation
environments:
  production:
    description: Managed by the Phase 8 Redirect integration check
    applications:
      api: $api_redirects
      worker: $worker_redirects
EOF
    chmod 600 "$config_file"
}

write_removed_config() {
    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Redirect executor validation
environments:
  production:
    description: Managed by the Phase 8 Redirect integration check
    applications:
      api: {}
      worker: {}
removed:
  - from: redirect.legacy
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
write_config api '/current/$1' false
run_apply create
require_noop_plan after-create

state_file="$workspace/.dokploy/state.json"
project_id="$(project_state_value "$state_file" "project.$project_name")"
environment_id="$(project_state_value "$state_file" environment.production)"
api_application_id="$(project_state_value "$state_file" application.api)"
worker_application_id="$(project_state_value "$state_file" application.worker)"
redirect_id="$(project_state_value "$state_file" redirect.legacy)"
observed_redirect_ids+=("$redirect_id")
capture_redirect_one_evidence "$redirect_id" "$api_application_id" '^/legacy/(.*)$' '/current/$1' false "$workspace/redirect-one.created.json"
capture_application_redirects "$api_application_id" "$workspace/application-api.created.json"
assert_application_redirects "$workspace/application-api.created.json" 1 "$redirect_id"
capture_application_redirects "$worker_application_id" "$workspace/application-worker.created.json"
assert_application_redirects "$workspace/application-worker.created.json" 0

# 2. Replace every owned field in place; the physical identity must not change.
write_config api '/updated/$1' true
run_apply update
require_noop_plan after-update
updated_redirect_id="$(project_state_value "$state_file" redirect.legacy)"
if [[ "$updated_redirect_id" != "$redirect_id" ]]; then
    echo "an in-place Redirect update changed the physical identity" >&2
    exit 1
fi
capture_redirect_one_evidence "$redirect_id" "$api_application_id" '^/legacy/(.*)$' '/updated/$1' true "$workspace/redirect-one.updated.json"
capture_application_redirects "$api_application_id" "$workspace/application-api.updated.json"
assert_application_redirects "$workspace/application-api.updated.json" 1 "$redirect_id"

# 3. Changing the containing application deletes before creating.
write_config worker '/updated/$1' true
run_apply containment-replacement
require_noop_plan after-containment-replacement
replacement_redirect_id="$(project_state_value "$state_file" redirect.legacy)"
if [[ "$replacement_redirect_id" == "$redirect_id" ]]; then
    echo "a containment change did not replace the physical Redirect" >&2
    exit 1
fi
observed_redirect_ids+=("$replacement_redirect_id")
if ! capture_redirect_one_absence "$redirect_id" "$workspace/redirect-one.replaced.status"; then
    echo "redirects.one did not prove absence of the replaced Redirect identity" >&2
    exit 1
fi
capture_redirect_one_evidence "$replacement_redirect_id" "$worker_application_id" '^/legacy/(.*)$' '/updated/$1' true "$workspace/redirect-one.replacement.json"
capture_application_redirects "$api_application_id" "$workspace/application-api.replaced.json"
assert_application_redirects "$workspace/application-api.replaced.json" 0
capture_application_redirects "$worker_application_id" "$workspace/application-worker.replaced.json"
assert_application_redirects "$workspace/application-worker.replaced.json" 1 "$replacement_redirect_id"

# 4. Delete declaratively and prove authoritative absence.
write_removed_config
run_apply delete
if ! capture_redirect_one_absence "$replacement_redirect_id" "$workspace/redirect-one.deleted.status"; then
    echo "redirects.one did not prove absence after declarative deletion" >&2
    exit 1
fi
capture_application_redirects "$worker_application_id" "$workspace/application-worker.deleted.json"
assert_application_redirects "$workspace/application-worker.deleted.json" 0
if jq -e '.resources["redirect.legacy"]' "$state_file" >/dev/null; then
    echo "the deleted Redirect identity remains in durable state" >&2
    exit 1
fi
require_noop_plan after-delete

# 5. Adopt an out-of-band Redirect through protected import and converge.
jq -n --arg applicationId "$worker_application_id" '{
    applicationId: $applicationId,
    regex: "^/adopted/(.*)$",
    replacement: "/adopted-target/$1",
    permanent: true
}' >"$private_directory/out-of-band-create.request.json"
create_status="$(api_post redirects.create "$private_directory/out-of-band-create.request.json" "$private_directory/out-of-band-create.response.json")"
if [[ "$create_status" != "200" ]]; then
    echo "the out-of-band redirects.create returned HTTP $create_status" >&2
    exit 1
fi
adopted_redirect_id="$(
    capture_application_redirects "$worker_application_id" "$workspace/application-worker.out-of-band.json" &&
        jq -er '.redirects[] | select(.regex == "^/adopted/(.*)$") | .redirectId' "$workspace/application-worker.out-of-band.json"
)"
observed_redirect_ids+=("$adopted_redirect_id")
cli import redirect "$adopted_redirect_id" \
    --as redirect.adopted \
    --file "$import_config_file" \
    >"$workspace/import.stdout" \
    2>"$workspace/import.stderr"
require_noop_plan after-import "$import_config_file"
if ! jq -e '.resources["redirect.adopted"].protected == true' \
    "$import_directory/.dokploy/state.json" >/dev/null; then
    echo "the imported Redirect is not protected in durable state" >&2
    exit 1
fi
capture_redirect_one_evidence "$adopted_redirect_id" "$worker_application_id" '^/adopted/(.*)$' '/adopted-target/$1' true "$workspace/redirect-one.adopted.json"
capture_application_redirects "$worker_application_id" "$workspace/application-worker.adopted.json"
assert_application_redirects "$workspace/application-worker.adopted.json" 1 "$adopted_redirect_id"

echo "Redirect declarative create, no-op convergence, complete update, containment replacement, deletion, absence, protected import, and zero-deployment checks passed."
