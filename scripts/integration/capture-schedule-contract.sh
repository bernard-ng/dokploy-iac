#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then set +x; fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
fixture_directory="$repository_root/fixtures/api/live/v0.30.6"
sanitizer="$script_directory/sanitize-fixture.jq"
if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before capturing the Schedule contract." >&2
    exit 1
fi

umask 077
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/schedule-contract-$run_id"
publish_directory="$workspace/publish"
secret_canary_file="$workspace/schedule-canaries"
candidate_fixture_root="$workspace/candidate/api/live"
candidate_versioned_fixture_directory="$candidate_fixture_root/v0.30.6"
published_fixture_backup="$workspace/published-fixtures.backup"
mkdir -p "$publish_directory"
chmod 700 "$workspace" "$publish_directory"
: >"$secret_canary_file"
chmod 600 "$secret_canary_file"

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

project_name="schedule-sdk-contract-$run_id"
project_id=""
mutation_attempted=false
cleanup_confirmed=false
capture_succeeded=false
publication_started=false
publication_complete=false

urlencode() { jq -nr --arg value "$1" '$value | @uri'; }

api_request() {
    local method="$1" endpoint="$2" destination="$3" body_file="${4:-}"
    local arguments=(
        --silent --show-error --output "$destination" --write-out '%{http_code}'
        --header "@$auth_header_file" --request "$method"
    )
    if [[ -n "$body_file" ]]; then
        arguments+=(--header 'Content-Type: application/json' --data-binary "@$body_file")
    fi
    curl "${arguments[@]}" "$base_url/api/$endpoint"
}

require_status() {
    if [[ "$1" != "$2" ]]; then
        echo "$3 returned HTTP $1; expected HTTP $2." >&2
        return 1
    fi
}

recover_project_id() {
    local response_status count
    if [[ -n "$project_id" || "$mutation_attempted" != true ]]; then return; fi
    response_status="$(api_request GET project.all "$workspace/project-all.recovery.json")" || response_status=""
    if [[ "$response_status" != 200 ]]; then return; fi
    count="$(jq --arg name "$project_name" '[.[] | select(.name == $name)] | length' \
        "$workspace/project-all.recovery.json")" || count=""
    if [[ "$count" == 1 ]]; then
        project_id="$(jq -er --arg name "$project_name" \
            '.[] | select(.name == $name) | .projectId' \
            "$workspace/project-all.recovery.json")" || project_id=""
    elif [[ -n "$count" && "$count" != 0 ]]; then
        echo "Cleanup found multiple disposable project candidates." >&2
    fi
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/schedule-contract-*) ;;
        *) echo "Refusing to delete an unexpected Schedule capture workspace." >&2; return 1 ;;
    esac
    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?" response_status verify_status
    local restore_ready=true
    trap - EXIT INT TERM
    set +e
    if [[ "$publication_started" == true && "$publication_complete" == false ]]; then
        if [[ -d "$published_fixture_backup" ]]; then
            if [[ -e "$fixture_directory" ]] \
                && ! mv "$fixture_directory" "$workspace/failed-publication"
            then
                echo "Could not quarantine the partially published fixture directory." >&2
                restore_ready=false
                exit_code=1
            fi

            if [[ "$restore_ready" == true && ! -e "$fixture_directory" ]]; then
                if ! mv "$published_fixture_backup" "$fixture_directory"; then
                    echo "Could not restore the previous tracked fixture directory." >&2
                    exit_code=1
                fi
            else
                echo "Could not restore the previous tracked fixture directory." >&2
                exit_code=1
            fi
        else
            echo "Fixture publication was interrupted without a recoverable backup." >&2
            exit_code=1
        fi
    fi
    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        recover_project_id
        if [[ -n "$project_id" ]]; then
            jq -n --arg projectId "$project_id" '{projectId:$projectId}' \
                >"$workspace/project-remove.cleanup.request.json"
            response_status="$(api_request POST project.remove "$workspace/project-remove.cleanup.json" \
                "$workspace/project-remove.cleanup.request.json")"
            if [[ "$response_status" != 200 && "$response_status" != 404 ]]; then
                echo "Project cleanup returned HTTP $response_status." >&2
                exit_code=1
            else
                verify_status="$(api_request GET "project.one?projectId=$(urlencode "$project_id")" \
                    "$workspace/project-one.cleanup.json")"
                if [[ "$verify_status" != 404 ]]; then
                    echo "Project cleanup could not prove authoritative absence." >&2
                    exit_code=1
                fi
            fi
        else
            echo "Project cleanup could not recover the disposable identity." >&2
            exit_code=1
        fi
    fi
    if [[ "$capture_succeeded" == true ]]; then
        discard_private_workspace || exit_code=1
    else
        echo "Schedule capture evidence remains in $workspace" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

verify_schedule() {
    local file="$1" schedule_type="$2" target_id="$3" name="$4" description="$5"
    local cron="$6" shell_type="$7" command="$8" script="$9" timezone="${10}"
    jq -e --arg type "$schedule_type" --arg targetId "$target_id" --arg name "$name" \
        --arg description "$description" --arg cron "$cron" --arg shell "$shell_type" \
        --arg command "$command" --arg script "$script" --arg timezone "$timezone" '
        .scheduleId != null
        and .scheduleType == $type
        and .name == $name
        and .description == $description
        and .cronExpression == $cron
        and .shellType == $shell
        and .command == $command
        and .script == $script
        and .enabled == false
        and .timezone == $timezone
        and (if $type == "application" then
            .applicationId == $targetId and .composeId == null and .serviceName == null
        else
            .composeId == $targetId and .applicationId == null and .serviceName == "worker"
        end)
    ' "$file" >/dev/null
}

capture_target() {
    local schedule_type="$1" target_id="$2"
    local initial_command updated_command initial_script updated_script
    local response_status schedule_id encoded_schedule_id
    initial_command="schedule-$schedule_type-created-$(openssl rand -hex 24)"
    initial_script="schedule-$schedule_type-script-created-$(openssl rand -hex 24)"
    updated_command="schedule-$schedule_type-updated-$(openssl rand -hex 24)"
    updated_script="schedule-$schedule_type-script-updated-$(openssl rand -hex 24)"
    printf '%s\n' "$initial_command" "$initial_script" "$updated_command" "$updated_script" \
        >>"$secret_canary_file"

    response_status="$(api_request GET \
        "schedule.list?id=$(urlencode "$target_id")&scheduleType=$schedule_type" \
        "$workspace/schedule-list.$schedule_type-preflight.json")"
    require_status "$response_status" 200 "schedule.list before $schedule_type create"
    if ! jq -e 'length == 0' "$workspace/schedule-list.$schedule_type-preflight.json" >/dev/null; then
        echo "The disposable $schedule_type target already had Schedules." >&2
        return 1
    fi

    jq -n --arg type "$schedule_type" --arg id "$target_id" --arg command "$initial_command" \
        --arg script "$initial_script" '
        {
            name:($type + "-job"),description:"initial disabled schedule",
            cronExpression:"7 4 * * 0",shellType:"bash",scheduleType:$type,
            command:$command,script:$script,enabled:false,timezone:"UTC"
        }
        + (if $type == "application" then {applicationId:$id}
           else {composeId:$id,serviceName:"worker"} end)
    ' >"$workspace/schedule-create.$schedule_type.request.json"
    response_status="$(api_request POST schedule.create "$workspace/schedule-create.$schedule_type.json" \
        "$workspace/schedule-create.$schedule_type.request.json")"
    require_status "$response_status" 200 "schedule.create for $schedule_type"
    verify_schedule "$workspace/schedule-create.$schedule_type.json" "$schedule_type" "$target_id" \
        "$schedule_type-job" "initial disabled schedule" "7 4 * * 0" bash \
        "$initial_command" "$initial_script" UTC
    schedule_id="$(jq -er '.scheduleId' "$workspace/schedule-create.$schedule_type.json")"
    encoded_schedule_id="$(urlencode "$schedule_id")"

    response_status="$(api_request GET "schedule.one?scheduleId=$encoded_schedule_id" \
        "$workspace/schedule-one.$schedule_type-created.json")"
    require_status "$response_status" 200 "schedule.one after $schedule_type create"
    verify_schedule "$workspace/schedule-one.$schedule_type-created.json" "$schedule_type" "$target_id" \
        "$schedule_type-job" "initial disabled schedule" "7 4 * * 0" bash \
        "$initial_command" "$initial_script" UTC

    response_status="$(api_request GET \
        "schedule.list?id=$(urlencode "$target_id")&scheduleType=$schedule_type" \
        "$workspace/schedule-list.$schedule_type-created.json")"
    require_status "$response_status" 200 "schedule.list after $schedule_type create"
    if ! jq -e 'length == 1 and (.[0].deployments | length) == 0' \
        "$workspace/schedule-list.$schedule_type-created.json" >/dev/null; then
        echo "The created $schedule_type Schedule was ambiguous or executed." >&2
        return 1
    fi
    verify_schedule <(jq '.[0]' "$workspace/schedule-list.$schedule_type-created.json") \
        "$schedule_type" "$target_id" "$schedule_type-job" "initial disabled schedule" \
        "7 4 * * 0" bash "$initial_command" "$initial_script" UTC

    jq -n --arg scheduleId "$schedule_id" --arg type "$schedule_type" \
        --arg command "$updated_command" --arg script "$updated_script" '
        {
            scheduleId:$scheduleId,name:($type + "-job-updated"),
            description:"updated disabled schedule",cronExpression:"13 5 * * 1",
            shellType:"sh",command:$command,script:$script,enabled:false,
            timezone:"Africa/Lubumbashi"
        }
    ' >"$workspace/schedule-update.$schedule_type.request.json"
    response_status="$(api_request POST schedule.update "$workspace/schedule-update.$schedule_type.json" \
        "$workspace/schedule-update.$schedule_type.request.json")"
    require_status "$response_status" 200 "schedule.update for $schedule_type"
    verify_schedule "$workspace/schedule-update.$schedule_type.json" "$schedule_type" "$target_id" \
        "$schedule_type-job-updated" "updated disabled schedule" "13 5 * * 1" sh \
        "$updated_command" "$updated_script" Africa/Lubumbashi

    response_status="$(api_request GET "schedule.one?scheduleId=$encoded_schedule_id" \
        "$workspace/schedule-one.$schedule_type-updated.json")"
    require_status "$response_status" 200 "schedule.one after $schedule_type update"
    verify_schedule "$workspace/schedule-one.$schedule_type-updated.json" "$schedule_type" "$target_id" \
        "$schedule_type-job-updated" "updated disabled schedule" "13 5 * * 1" sh \
        "$updated_command" "$updated_script" Africa/Lubumbashi

    response_status="$(api_request GET \
        "schedule.list?id=$(urlencode "$target_id")&scheduleType=$schedule_type" \
        "$workspace/schedule-list.$schedule_type-updated.json")"
    require_status "$response_status" 200 "schedule.list after $schedule_type update"
    if ! jq -e 'length == 1 and (.[0].deployments | length) == 0' \
        "$workspace/schedule-list.$schedule_type-updated.json" >/dev/null; then
        echo "The updated $schedule_type Schedule was ambiguous or executed." >&2
        return 1
    fi
    verify_schedule <(jq '.[0]' "$workspace/schedule-list.$schedule_type-updated.json") \
        "$schedule_type" "$target_id" "$schedule_type-job-updated" "updated disabled schedule" \
        "13 5 * * 1" sh "$updated_command" "$updated_script" Africa/Lubumbashi

    jq -n --arg scheduleId "$schedule_id" '{scheduleId:$scheduleId}' \
        >"$workspace/schedule-delete.$schedule_type.request.json"
    response_status="$(api_request POST schedule.delete "$workspace/schedule-delete.$schedule_type.json" \
        "$workspace/schedule-delete.$schedule_type.request.json")"
    require_status "$response_status" 200 "schedule.delete for $schedule_type"

    response_status="$(api_request GET "schedule.one?scheduleId=$encoded_schedule_id" \
        "$workspace/schedule-one.$schedule_type-deleted.json")"
    require_status "$response_status" 404 "schedule.one after $schedule_type delete"
    response_status="$(api_request GET \
        "schedule.list?id=$(urlencode "$target_id")&scheduleType=$schedule_type" \
        "$workspace/schedule-list.$schedule_type-deleted.json")"
    require_status "$response_status" 200 "schedule.list after $schedule_type delete"
    if ! jq -e 'length == 0' "$workspace/schedule-list.$schedule_type-deleted.json" >/dev/null; then
        echo "The $schedule_type target retained Schedule state." >&2
        return 1
    fi
}

response_status="$(api_request GET settings.getDokployVersion "$workspace/version.json")"
require_status "$response_status" 200 settings.getDokployVersion
runtime_version="$(jq -er '.' "$workspace/version.json")"
if [[ "$runtime_version" != v0.30.6 ]]; then
    echo "Expected Dokploy v0.30.6, received $runtime_version." >&2
    exit 1
fi

jq -n --arg name "$project_name" '{name:$name,description:"Disposable Schedule SDK contract"}' \
    >"$workspace/project-create.request.json"
mutation_attempted=true
response_status="$(api_request POST project.create "$workspace/project-create.json" \
    "$workspace/project-create.request.json")"
require_status "$response_status" 200 project.create
project_id="$(jq -er '.project.projectId' "$workspace/project-create.json")"
environment_id="$(jq -er '.environment.environmentId' "$workspace/project-create.json")"

jq -n --arg environmentId "$environment_id" \
    '{name:"Schedule Contract Application",environmentId:$environmentId}' \
    >"$workspace/application-create.request.json"
response_status="$(api_request POST application.create "$workspace/application-create.json" \
    "$workspace/application-create.request.json")"
require_status "$response_status" 200 application.create
application_id="$(jq -er '.applicationId' "$workspace/application-create.json")"

compose_file=$'services:\n  worker:\n    image: busybox:1.36.1\n    command: ["sh", "-c", "sleep infinity"]\n'
jq -n --arg environmentId "$environment_id" --arg appName "schedule-contract-$run_id" \
    --arg composeFile "$compose_file" '
    {
        name:"Schedule Contract Compose",environmentId:$environmentId,
        composeType:"docker-compose",appName:$appName,serverId:null,
        composeFile:$composeFile,sourceType:"raw"
    }
' >"$workspace/compose-create.request.json"
response_status="$(api_request POST compose.create "$workspace/compose-create.json" \
    "$workspace/compose-create.request.json")"
require_status "$response_status" 200 compose.create
compose_id="$(jq -er '.composeId' "$workspace/compose-create.json")"

capture_target application "$application_id"
capture_target compose "$compose_id"

response_status="$(api_request GET "application.one?applicationId=$(urlencode "$application_id")" \
    "$workspace/application-one.schedule-deleted.json")"
require_status "$response_status" 200 "application.one after Schedule deletion"
if ! jq -e '.applicationStatus == "idle" and (.deployments | length) == 0' \
    "$workspace/application-one.schedule-deleted.json" >/dev/null; then
    echo "The Schedule capture deployed its application." >&2
    exit 1
fi
response_status="$(api_request GET "compose.one?composeId=$(urlencode "$compose_id")" \
    "$workspace/compose-one.schedule-deleted.json")"
require_status "$response_status" 200 "compose.one after Schedule deletion"
if ! jq -e '.composeStatus == "idle" and (.deployments | length) == 0' \
    "$workspace/compose-one.schedule-deleted.json" >/dev/null; then
    echo "The Schedule capture deployed its Compose target." >&2
    exit 1
fi

jq -n --arg projectId "$project_id" '{projectId:$projectId}' >"$workspace/project-remove.request.json"
response_status="$(api_request POST project.remove "$workspace/project-remove.json" \
    "$workspace/project-remove.request.json")"
require_status "$response_status" 200 project.remove
project_one_status="$(api_request GET "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.schedule-deleted.json")"
require_status "$project_one_status" 404 "project.one after cleanup"
project_id=""
cleanup_confirmed=true

fixtures=(
    schedule-create.application schedule-one.application-created schedule-list.application-created
    schedule-update.application schedule-one.application-updated schedule-list.application-updated
    schedule-delete.application schedule-one.application-deleted schedule-list.application-deleted
    schedule-create.compose schedule-one.compose-created schedule-list.compose-created
    schedule-update.compose schedule-one.compose-updated schedule-list.compose-updated
    schedule-delete.compose schedule-one.compose-deleted schedule-list.compose-deleted
    application-one.schedule-deleted compose-one.schedule-deleted project-one.schedule-deleted
)
for name in "${fixtures[@]}"; do
    jq --sort-keys --indent 2 --from-file "$sanitizer" "$workspace/$name.json" \
        >"$publish_directory/$name.owner.json"
done

jq -n --sort-keys --indent 2 \
    --arg capturedAt "2026-09-30" --arg role owner --arg version "$runtime_version" \
    --arg image "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8" '
    {
        capturedAt:$capturedAt,role:$role,version:$version,image:$image,sanitized:true,
        deployed:false,executed:false,collisionKey:"target+name",
        supportedTargets:["application","compose"],privilegedTargetsSupported:false,
        executableTextVerifiedPrivately:true,
        createIdentity:{direct:true,parentVerified:true},
        update:{allSafeMutableFieldsPersisted:true,parentVerified:true,enabled:false},
        cleanupEvidence:{applicationSchedulesEmpty:true,composeSchedulesEmpty:true,projectOneStatus:404}
    }
' >"$publish_directory/schedule-contract.metadata.json"

mkdir -p "$candidate_versioned_fixture_directory"
cp -R "$fixture_directory/." "$candidate_versioned_fixture_directory/"
for fixture in "$publish_directory"/*.json; do
    cp "$fixture" "$candidate_versioned_fixture_directory/$(basename "$fixture")"
done

find "$candidate_fixture_root" -type d -exec chmod 755 {} +
find "$candidate_fixture_root" -type f -exec chmod 644 {} +

if grep -R -F -q -f "$api_key_file" "$candidate_fixture_root"; then
    echo "Sanitized Schedule fixtures contain the local API key." >&2
    exit 1
fi

if grep -R -F -q -f "$secret_canary_file" "$candidate_fixture_root"; then
    echo "Sanitized Schedule fixtures contain a command or script canary." >&2
    exit 1
fi

DOKPLOY_FIXTURE_DIRECTORY="$candidate_fixture_root" "$script_directory/check-fixtures.sh"
publication_started=true
mv "$fixture_directory" "$published_fixture_backup"
mv "$candidate_versioned_fixture_directory" "$fixture_directory"
publication_complete=true
capture_succeeded=true

echo "Captured sanitized Application and Compose Schedule contracts without execution or deployment."
