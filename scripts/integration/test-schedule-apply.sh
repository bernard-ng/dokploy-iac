#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=schedule-evidence.sh
source "$script_directory/schedule-evidence.sh"

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
workspace="$(mktemp -d "$state_directory/phase8-schedule-apply.XXXXXX")"
private_directory="$workspace/private"
import_directory="$workspace/import"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
project_name="phase8-schedule-$run_id"
config_file="$workspace/dokploy.yaml"
import_config_file="$import_directory/dokploy.yaml"
api_header_file="$private_directory/api-header"
project_output="$workspace/cleanup-projects.json"
state_file="$workspace/.dokploy/state.json"
# Executable text and the API key are generated per run and exist only in the
# private directory; they are compared with the retained evidence at the end.
# Every Schedule stays disabled and is never run, so these are inert strings.
command_nightly="phase8-nightly-command-$(openssl rand -hex 16)"
script_nightly="phase8-nightly-script-$(openssl rand -hex 16)"
rotated_command="phase8-rotated-command-$(openssl rand -hex 16)"
rotated_script="phase8-rotated-script-$(openssl rand -hex 16)"
command_reports="phase8-reports-command-$(openssl rand -hex 16)"
command_compose="phase8-compose-command-$(openssl rand -hex 16)"
command_adopted="phase8-adopted-command-$(openssl rand -hex 16)"
api_key_value="$(tr -d '\r\n' <"$api_key_file")"
project_id=""
api_application_id=""
worker_application_id=""
compose_id=""
declare -a observed_schedule_ids=()

mkdir -p "$private_directory" "$import_directory"
chmod 700 "$private_directory" "$import_directory"
{
    printf 'x-api-key: %s\n' "$api_key_value"
} >"$api_header_file"
chmod 600 "$api_header_file"
write_private_text() {
    printf '%s' "$2" >"$private_directory/$1"
    chmod 600 "$private_directory/$1"
}
write_private_text command-nightly "$command_nightly"
write_private_text script-nightly "$script_nightly"
write_private_text command-reports "$command_reports"
write_private_text command-compose "$command_compose"

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

state_value() {
    jq -er --arg address "$1" '.resources[$address].remoteId' "$state_file"
}

# Proves the exact Schedule record, including the exact remote command and
# script bytes, without ever retaining those bytes. Every Schedule must be
# disabled and must have zero deployments, which proves it never executed.
capture_schedule_one_evidence() {
    local schedule_id="$1"
    local schedule_type="$2"
    local target_id="$3"
    local name="$4"
    local cron="$5"
    local shell_type="$6"
    local command_file="$7"
    local script_file="$8"
    local destination="$9"
    local service_name="${10:-}"
    local id_field="${schedule_type}Id"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "schedule.one?scheduleId=$(urlencode "$schedule_id")" "$raw_response")"
    if [[ "$status" != "200" ]]; then
        rm -f -- "$raw_response"
        echo "schedule.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e \
        --arg id "$schedule_id" \
        --arg schedule_type "$schedule_type" \
        --arg id_field "$id_field" \
        --arg target_id "$target_id" \
        --arg name "$name" \
        --arg cron "$cron" \
        --arg shell_type "$shell_type" \
        --arg service_name "$service_name" '
        .scheduleId == $id
        and .scheduleType == $schedule_type
        and .[$id_field] == $target_id
        and .name == $name
        and .cronExpression == $cron
        and .shellType == $shell_type
        and .enabled == false
        and (if $service_name == "" then (.serviceName == null or .serviceName == "") else .serviceName == $service_name end)
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "schedule.one did not prove the exact declarative disabled, never-run Schedule state" >&2
        return 1
    fi
    if ! jq -e --rawfile command "$command_file" '.command == $command' \
        "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "schedule.one did not prove the exact remote command bytes" >&2
        return 1
    fi
    if [[ -n "$script_file" ]]; then
        if ! jq -e --rawfile script "$script_file" '.script == $script' \
            "$raw_response" >/dev/null; then
            rm -f -- "$raw_response"
            echo "schedule.one did not prove the exact remote script bytes" >&2
            return 1
        fi
    elif ! jq -e '.script == null or .script == ""' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "schedule.one reported an unexpected remote script" >&2
        return 1
    fi
    # The collection is the only read that reports a Schedule's deployments, so
    # it proves the Schedule never ran.
    local list_response
    list_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "schedule.list?id=$(urlencode "$target_id")&scheduleType=$schedule_type" "$list_response")"
    if [[ "$status" != "200" ]] || ! jq -e --arg id "$schedule_id" '
        [.[] | select(.scheduleId == $id)]
        | length == 1
        and ((.[0].deployments | type) == "array" and (.[0].deployments | length) == 0)
    ' "$list_response" >/dev/null; then
        rm -f -- "$raw_response" "$list_response"
        echo "schedule.list did not prove the Schedule never executed" >&2
        return 1
    fi
    rm -f -- "$list_response"
    jq '{scheduleId, scheduleType, name, cronExpression, shellType, enabled, description, timezone, serviceName, deploymentCount: 0}' \
        "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

capture_schedule_absence() {
    local absent_schedule_id="$1"
    local destination="$2"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "schedule.one?scheduleId=$(urlencode "$absent_schedule_id")" "$raw_response")"
    rm -f -- "$raw_response"
    printf '%s\n' "$status" >"$destination"

    [[ "$status" == "404" ]]
}

capture_schedule_collection() {
    local schedule_type="$1"
    local target_id="$2"
    local destination="$3"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "schedule.list?id=$(urlencode "$target_id")&scheduleType=$schedule_type" "$raw_response")"
    if [[ "$status" != "200" ]]; then
        rm -f -- "$raw_response"
        echo "schedule.list returned HTTP $status; expected 200" >&2
        return 1
    fi
    jq '{scheduleIds: ([.[].scheduleId] | sort)}' "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

assert_schedule_collection() {
    local evidence_file="$1"
    shift
    local expected
    expected="$(printf '%s\n' "$@" | jq -R . | jq -sc 'map(select(. != "")) | sort')"

    if ! jq -e --argjson expected "$expected" '.scheduleIds == $expected' "$evidence_file" >/dev/null; then
        echo "the authoritative Schedule collection did not match the expected identities" >&2
        return 1
    fi
}

capture_zero_deployments() {
    local kind="$1"
    local id="$2"
    local destination="$3"
    local raw_response
    local status

    raw_response="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "$kind.one?${kind}Id=$(urlencode "$id")" "$raw_response")"
    if [[ "$status" != "200" ]] || ! jq -e '
        (.deployments | type) == "array" and (.deployments | length) == 0
    ' "$raw_response" >/dev/null; then
        rm -f -- "$raw_response"
        echo "$kind.one did not prove an undeployed service" >&2
        return 1
    fi
    jq '{deploymentCount: (.deployments | length)}' "$raw_response" >"$destination"
    rm -f -- "$raw_response"
}

cli() {
    "$repository_root/target/debug/dokploy" "$@"
}

cleanup() {
    local primary_status=$?
    local cleanup_status=0
    local cleanup_project_id="$project_id"
    local identity

    trap - EXIT
    set +e
    if [[ -z "$cleanup_project_id" && -s "$state_file" ]]; then
        cleanup_project_id="$(jq -r --arg address "project.$project_name" '.resources[$address].remoteId // empty' "$state_file")"
    fi
    if [[ -z "$cleanup_project_id" ]]; then
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$api_key_value" \
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
        DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$api_key_value" \
            cli project remove \
            --body-project-id "$cleanup_project_id" \
            >"$workspace/cleanup-project-remove.stdout" \
            2>"$workspace/cleanup-project-remove.stderr"
        if [[ "$?" -ne 0 ]]; then
            cleanup_status=1
        fi
    fi

    DOKPLOY_URL="$base_url" DOKPLOY_API_KEY="$api_key_value" \
        cli project all \
        >"$project_output" \
        2>"$workspace/cleanup-projects.stderr"
    if [[ "$?" -ne 0 ]] || ! jq -e --arg name "$project_name" '[.[] | select(.name == $name)] | length == 0' "$project_output" >/dev/null; then
        echo "Schedule cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    for identity in "${observed_schedule_ids[@]}"; do
        if ! capture_schedule_absence "$identity" "$workspace/cleanup-schedule.$identity.status"; then
            echo "Schedule cleanup could not prove Schedule identity absence" >&2
            cleanup_status=1
        fi
    done

    if ! scrub_schedule_private_evidence "$private_directory" "$workspace"; then
        echo "Schedule cleanup could not discard private evidence" >&2
        cleanup_status=1
    fi
    if ! assert_schedule_retained_evidence_secret_free \
        "$workspace" \
        "$api_key_value" \
        "$command_nightly" \
        "$script_nightly" \
        "$rotated_command" \
        "$rotated_script" \
        "$command_reports" \
        "$command_compose" \
        "$command_adopted"; then
        cleanup_status=1
    fi

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-schedule-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "Schedule integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

# Configuration variables mutated between applies. Every Schedule is disabled.
nightly_target="api"
nightly_cron="7 4 1 1 *"
nightly_description="Phase 8 nightly Schedule"
nightly_timezone="UTC"
nightly_command_file="private/command-nightly"
nightly_script_file="private/script-nightly"
include_reports="true"
removed_block=""

write_config() {
    local reports=""

    if [[ "$include_reports" == "true" ]]; then
        reports="
      reports:
        name: reports-$run_id
        target: application.api
        cron_expression: \"13 5 1 1 *\"
        shell_type: sh
        enabled: false
        command:
          file: private/command-reports"
    fi

    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Schedule executor validation
environments:
  production:
    description: Managed by the Phase 8 Schedule integration check
    applications:
      api: {}
      worker: {}
    compose:
      web:
        document:
          file: compose.yaml
    schedules:
      nightly:
        name: nightly-$run_id
        target: application.$nightly_target
        cron_expression: "$nightly_cron"
        shell_type: bash
        enabled: false
        description: $nightly_description
        timezone: $nightly_timezone
        command:
          file: $nightly_command_file
        script:
          file: $nightly_script_file
      compose-job:
        name: compose-job-$run_id
        target: compose.web
        service_name: web
        cron_expression: "17 6 1 1 *"
        shell_type: sh
        enabled: false
        command:
          file: private/command-compose$reports
$removed_block
EOF
    chmod 600 "$config_file"
}

printf 'services:\n  web:\n    image: busybox:1.36\n    command: ["sleep", "infinity"]\n' \
    >"$workspace/compose.yaml"

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
export DOKPLOY_API_KEY="$api_key_value"

# 1. Create undeployed and disabled against an application and a Compose
# service, with executable text from descriptors only; then converge.
write_config
run_apply create
require_noop_plan after-create

project_id="$(state_value "project.$project_name")"
api_application_id="$(state_value application.api)"
worker_application_id="$(state_value application.worker)"
compose_id="$(state_value compose.web)"
nightly_id="$(state_value schedule.nightly)"
reports_id="$(state_value schedule.reports)"
compose_job_id="$(state_value schedule.compose-job)"
observed_schedule_ids+=("$nightly_id" "$reports_id" "$compose_job_id")
capture_schedule_one_evidence "$nightly_id" application "$api_application_id" "nightly-$run_id" "7 4 1 1 *" bash "$private_directory/command-nightly" "$private_directory/script-nightly" "$workspace/schedule-nightly.created.json"
capture_schedule_one_evidence "$reports_id" application "$api_application_id" "reports-$run_id" "13 5 1 1 *" sh "$private_directory/command-reports" "" "$workspace/schedule-reports.created.json"
capture_schedule_one_evidence "$compose_job_id" compose "$compose_id" "compose-job-$run_id" "17 6 1 1 *" sh "$private_directory/command-compose" "" "$workspace/schedule-compose-job.created.json" web
capture_schedule_collection application "$api_application_id" "$workspace/collection-api.created.json"
assert_schedule_collection "$workspace/collection-api.created.json" "$nightly_id" "$reports_id"
capture_schedule_collection compose "$compose_id" "$workspace/collection-compose.created.json"
assert_schedule_collection "$workspace/collection-compose.created.json" "$compose_job_id"
if jq -e '.. | strings | select(test("phase8-(nightly|rotated|reports|compose|adopted)-(command|script)-"))' "$state_file" >/dev/null; then
    echo "durable state contains executable text" >&2
    exit 1
fi

# 2. Update cron, description, and timezone in place and rotate the command and
# script; the physical identities must not change.
nightly_cron="9 6 2 2 *"
nightly_description="Phase 8 nightly Schedule updated"
nightly_timezone="Africa/Lubumbashi"
write_private_text command-nightly "$rotated_command"
write_private_text script-nightly "$rotated_script"
write_config
run_apply update
require_noop_plan after-update
if [[ "$(state_value schedule.nightly)" != "$nightly_id" || "$(state_value schedule.compose-job)" != "$compose_job_id" ]]; then
    echo "an in-place Schedule update changed a physical identity" >&2
    exit 1
fi
capture_schedule_one_evidence "$nightly_id" application "$api_application_id" "nightly-$run_id" "9 6 2 2 *" bash "$private_directory/command-nightly" "$private_directory/script-nightly" "$workspace/schedule-nightly.updated.json"
if ! jq -e --arg description "$nightly_description" --arg timezone "$nightly_timezone" '.description == $description and .timezone == $timezone' "$workspace/schedule-nightly.updated.json" >/dev/null; then
    echo "the in-place update did not persist the description and timezone" >&2
    exit 1
fi

# 3. Changing the target service replaces the Schedule (delete before create).
nightly_target="worker"
write_config
run_apply target-replacement
require_noop_plan after-target-replacement
replacement_id="$(state_value schedule.nightly)"
if [[ "$replacement_id" == "$nightly_id" ]]; then
    echo "a target change did not replace the physical Schedule" >&2
    exit 1
fi
observed_schedule_ids+=("$replacement_id")
if ! capture_schedule_absence "$nightly_id" "$workspace/schedule-nightly.replaced.status"; then
    echo "schedule.one did not prove absence of the replaced Schedule identity" >&2
    exit 1
fi
capture_schedule_one_evidence "$replacement_id" application "$worker_application_id" "nightly-$run_id" "9 6 2 2 *" bash "$private_directory/command-nightly" "$private_directory/script-nightly" "$workspace/schedule-nightly.replacement.json"
capture_schedule_collection application "$api_application_id" "$workspace/collection-api.replaced.json"
assert_schedule_collection "$workspace/collection-api.replaced.json" "$reports_id"
nightly_id="$replacement_id"

# 4. Delete declaratively and prove authoritative absence.
include_reports="false"
removed_block='removed:
  - from: schedule.reports
    destroy: true'
write_config
run_apply delete
if ! capture_schedule_absence "$reports_id" "$workspace/schedule-reports.deleted.status"; then
    echo "schedule.one did not prove absence after declarative deletion" >&2
    exit 1
fi
capture_schedule_collection application "$api_application_id" "$workspace/collection-api.deleted.json"
assert_schedule_collection "$workspace/collection-api.deleted.json"
if jq -e '.resources["schedule.reports"]' "$state_file" >/dev/null; then
    echo "the deleted Schedule identity remains in durable state" >&2
    exit 1
fi
require_noop_plan after-delete

# 5. Adopt an out-of-band Schedule through protected import and converge. The
# Schedule is created disabled, through a private request body.
write_private_text command-adopted "$command_adopted"
jq -n \
    --arg name "adopted-$run_id" \
    --rawfile command "$private_directory/command-adopted" \
    --arg application_id "$worker_application_id" '{
        name: $name,
        description: null,
        cronExpression: "21 7 1 1 *",
        shellType: "bash",
        command: $command,
        script: null,
        enabled: false,
        timezone: null,
        scheduleType: "application",
        applicationId: $application_id
    }' >"$private_directory/adopted-body.json"
adopted_response="$private_directory/adopted-response.json"
adopted_status="$(curl \
    --silent \
    --show-error \
    --output "$adopted_response" \
    --write-out '%{http_code}' \
    --request POST \
    --header "@$api_header_file" \
    --header 'content-type: application/json' \
    --data "@$private_directory/adopted-body.json" \
    "$base_url/api/schedule.create")"
if [[ "$adopted_status" != "200" ]]; then
    echo "the out-of-band Schedule could not be created (HTTP $adopted_status)" >&2
    exit 1
fi
adopted_id="$(jq -er '.scheduleId' "$adopted_response")"
observed_schedule_ids+=("$adopted_id")
cli import schedule "$adopted_id" \
    --as schedule.adopted \
    --file "$import_config_file" \
    >"$workspace/import.stdout" \
    2>"$workspace/import.stderr"
require_noop_plan after-import "$import_config_file"
if ! jq -e '.resources["schedule.adopted"].protected == true' \
    "$import_directory/.dokploy/state.json" >/dev/null; then
    echo "the imported Schedule is not protected in durable state" >&2
    exit 1
fi
if grep -R -F -q -- "$command_adopted" "$import_directory"; then
    echo "the imported workspace contains executable text" >&2
    exit 1
fi
capture_schedule_one_evidence "$adopted_id" application "$worker_application_id" "adopted-$run_id" "21 7 1 1 *" bash "$private_directory/command-adopted" "" "$workspace/schedule-adopted.json"

# 6. No deployment was created for any target and no Schedule ever ran.
capture_zero_deployments application "$api_application_id" "$workspace/deployments-api.json"
capture_zero_deployments application "$worker_application_id" "$workspace/deployments-worker.json"
capture_zero_deployments compose "$compose_id" "$workspace/deployments-compose.json"

echo "Schedule declarative create (application and Compose targets, disabled, exact executable text), no-op convergence, in-place update and executable rotation, target replacement, deletion, absence, protected import, and zero-deployment and zero-execution checks passed."
