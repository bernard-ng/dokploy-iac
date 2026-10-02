#!/usr/bin/env bash

# Live acceptance for declarative database Backups.
#
# Creates disposable PostgreSQL and MySQL targets and two inert backup
# destinations, then proves: disabled, undeployed creation; no-op convergence;
# in-place update of every mutable field under a stable identity; destination
# re-selection; delete-before-create target replacement; declarative deletion
# with authoritative absence; saved-plan binding of the resolved destination
# identity; and protected import of an out-of-band Backup.
# The Dokploy instance is shared: serialize runs with .integration/live.lock.
#
# Inertness:
#   - destinations point at a tripwire that counts every connection; creating a
#     destination, a Backup, or any later operation must never contact it;
#   - Backups are always created and kept disabled, so Dokploy never schedules
#     a run; nothing triggers a backup, a connectivity test, or a deployment;
#   - every touched database must stay idle with zero deployments and every
#     Backup must report zero deployment (execution) records.

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
workspace="$(mktemp -d "$state_directory/phase8-backup-apply.XXXXXX")"
private_directory="$workspace/private"
import_directory="$workspace/import"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
prefix="phase8-backup-$run_id"
project_name="$prefix"
config_file="$workspace/dokploy.yaml"
import_config_file="$import_directory/dokploy.yaml"
api_header_file="$private_directory/api-header"
secret_canary_file="$private_directory/secret-canaries"
tripwire_name="phase8-backup-tripwire-${run_id//[^a-zA-Z0-9]/}"
network_name="dokploy-iac-integration_default"
project_id=""
mutation_attempted=false
cleanup_confirmed=false
fingerprint_key="0199a0c8-2351-7c31-8899-2c8f81983ea5:$(openssl rand -hex 32)"
postgres_password="$(openssl rand -hex 24)"
mysql_password="$(openssl rand -hex 24)"
mysql_root_password="$(openssl rand -hex 24)"

destination_a_id=""
destination_b_id=""
destination_c_id=""
postgres_id=""
mysql_id=""

mkdir -p "$private_directory" "$import_directory"
chmod 700 "$private_directory" "$import_directory"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$api_header_file"
chmod 600 "$api_header_file"
printf '%s\n' "$fingerprint_key" "$postgres_password" "$mysql_password" \
    "$mysql_root_password" >>"$secret_canary_file"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

api_get() {
    local endpoint="$1" destination="$2"

    curl --silent --show-error --output "$destination" --write-out '%{http_code}' \
        --header "@$api_header_file" "$base_url/api/$endpoint"
}

api_post() {
    local endpoint="$1" body_file="$2" destination="$3"

    curl --silent --show-error --output "$destination" --write-out '%{http_code}' \
        --header "@$api_header_file" --header 'Content-Type: application/json' \
        --request POST --data-binary "@$body_file" "$base_url/api/$endpoint"
}

require_status() {
    if [[ "$1" != "$2" ]]; then
        echo "$3 returned HTTP $1; expected HTTP $2." >&2
        return 1
    fi
}

cli() {
    "$cli_binary" "$@"
}

state_value() {
    local state_file="$1" address="$2"

    jq -er --arg address "$address" '.resources[$address].remoteId' "$state_file"
}

tripwire_hits() {
    docker logs "$tripwire_name" 2>&1 | grep -c TRIPWIRE_HIT || true
}

assert_no_contact() {
    if [[ "$(tripwire_hits)" != 0 ]]; then
        echo "an inert backup destination was contacted ($1)" >&2
        return 1
    fi
}

create_destination() {
    local name="$1" label="$2" access_key secret_key response_status

    access_key="backup-destination-access-$label-$run_id"
    secret_key="backup-destination-secret-$label-$run_id"
    printf '%s\n' "$access_key" "$secret_key" >>"$secret_canary_file"
    jq -n --arg name "$name" --arg endpoint "http://$tripwire_name:9000" \
        --arg access "$access_key" --arg secret "$secret_key" '
        {
            name:$name,provider:"Other",accessKey:$access,secretAccessKey:$secret,
            bucket:"backup-fixture",region:"us-east-1",endpoint:$endpoint,
            additionalFlags:[]
        }
    ' >"$private_directory/destination-create.$label.request.json"
    response_status="$(api_post destination.create \
        "$private_directory/destination-create.$label.request.json" \
        "$private_directory/destination-create.$label.json")"
    require_status "$response_status" 200 "destination.create $label"
    jq -er '.destinationId' "$private_directory/destination-create.$label.json"
}

remove_destination() {
    local identity="$1" response_status

    jq -n --arg id "$identity" '{destinationId:$id}' >"$private_directory/remove.request.json"
    response_status="$(api_post destination.remove "$private_directory/remove.request.json" \
        "$private_directory/remove.response.json")"
    if [[ "$response_status" != 200 && "$response_status" != 404 ]]; then
        echo "destination.remove cleanup returned HTTP $response_status" >&2
        return 1
    fi
}

remove_scoped_destinations() {
    local status=0 identity

    if [[ "$(api_get destination.all "$private_directory/cleanup-destination-all.json")" == 200 ]]; then
        while IFS= read -r identity; do
            [[ -z "$identity" ]] || remove_destination "$identity" || status=1
        done < <(jq -r --arg prefix "$prefix" '.[] | select(.name | startswith($prefix)) | .destinationId' \
            "$private_directory/cleanup-destination-all.json")
    else
        status=1
    fi

    return "$status"
}

prove_scoped_absence() {
    [[ "$(api_get destination.all "$private_directory/absence-destination-all.json")" == 200 ]] &&
        jq -e --arg prefix "$prefix" '[.[] | select(.name | startswith($prefix))] | length == 0' \
            "$private_directory/absence-destination-all.json" >/dev/null
}

cleanup() {
    local primary_status=$?
    local cleanup_status=0
    local cleanup_project_id="$project_id"
    local state_file="$workspace/.dokploy/state.json"

    trap - EXIT
    set +e
    if [[ -z "$cleanup_project_id" && -s "$state_file" ]]; then
        cleanup_project_id="$(jq -r --arg address "project.$project_name" \
            '.resources[$address].remoteId // empty' "$state_file")"
    fi
    if [[ -z "$cleanup_project_id" ]]; then
        if [[ "$(api_get project.all "$private_directory/cleanup-projects-before.json")" == 200 ]]; then
            cleanup_project_id="$(jq -r --arg name "$project_name" \
                '[.[] | select(.name == $name)] | if length == 1 then .[0].projectId else empty end' \
                "$private_directory/cleanup-projects-before.json")"
        else
            cleanup_status=1
        fi
    fi
    if [[ -n "$cleanup_project_id" ]]; then
        jq -n --arg id "$cleanup_project_id" '{projectId:$id}' \
            >"$private_directory/cleanup-project-remove.request.json"
        if [[ "$(api_post project.remove "$private_directory/cleanup-project-remove.request.json" \
            "$private_directory/cleanup-project-remove.json")" != 200 ]]; then
            cleanup_status=1
        fi
    fi
    if [[ "$(api_get project.all "$private_directory/cleanup-projects.json")" != 200 ]] \
        || ! jq -e --arg name "$project_name" '[.[] | select(.name == $name)] | length == 0' \
            "$private_directory/cleanup-projects.json" >/dev/null
    then
        echo "Backup cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    if [[ "$mutation_attempted" == true ]]; then
        remove_scoped_destinations || cleanup_status=1
        prove_scoped_absence || {
            echo "Backup cleanup could not prove destination absence" >&2
            cleanup_status=1
        }
    fi
    docker rm -f "$tripwire_name" >/dev/null 2>&1
    if docker inspect "$tripwire_name" >/dev/null 2>&1; then
        echo "Backup cleanup could not remove its local tripwire" >&2
        cleanup_status=1
    fi
    if [[ "$cleanup_status" -eq 0 ]]; then
        cleanup_confirmed=true
    fi

    # Retained evidence is scanned for the API key, fingerprint key, database
    # passwords, destination credentials, and every external identity.
    local scan_status=0 canary
    while IFS= read -r canary; do
        [[ -z "$canary" ]] && continue
        if grep -R -F -q --exclude-dir=private -- "$canary" "$workspace"; then
            scan_status=1
        fi
    done <"$secret_canary_file"
    if grep -R -F -q --exclude-dir=private -- "$(tr -d '\r\n' <"$api_key_file")" "$workspace"; then
        scan_status=1
    fi
    for canary in "$destination_a_id" "$destination_b_id" "$destination_c_id"; do
        [[ -z "$canary" ]] && continue
        if grep -R -F -q --exclude-dir=private -- "$canary" "$workspace"; then
            scan_status=1
        fi
    done
    if [[ "$scan_status" -ne 0 ]]; then
        echo "secret or external identity material appeared in retained Backup evidence" >&2
        cleanup_status=1
    fi

    if ! find "$private_directory" -depth -delete 2>/dev/null || [[ -e "$private_directory" ]]; then
        echo "Backup cleanup could not discard private evidence" >&2
        cleanup_status=1
    fi

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-backup-apply.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "Backup integration evidence retained at $workspace" >&2
    fi

    if [[ "$primary_status" -ne 0 ]]; then
        exit "$primary_status"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT

response_status="$(api_get settings.getDokployVersion "$private_directory/version.json")"
require_status "$response_status" 200 settings.getDokployVersion
jq -e '. == "v0.30.6"' "$private_directory/version.json" >/dev/null

cargo_config=()
if [[ -n "${DOKPLOY_CARGO_CONFIG:-}" ]]; then
    cargo_config=(--config "$DOKPLOY_CARGO_CONFIG")
fi
cargo ${cargo_config[@]+"${cargo_config[@]}"} build \
    --manifest-path "$repository_root/Cargo.toml" --locked -p dokploy-cli
# A private copy keeps the binary stable even if another build updates target/.
cli_binary="$private_directory/dokploy"
cp "$repository_root/target/debug/dokploy" "$cli_binary"
chmod 700 "$cli_binary"

export DOKPLOY_URL="$base_url"
export DOKPLOY_API_KEY
DOKPLOY_API_KEY="$(<"$api_key_file")"
export DOKPLOY_FINGERPRINT_KEY="$fingerprint_key"
export PHASE8_BACKUP_POSTGRES_PASSWORD="$postgres_password"
export PHASE8_BACKUP_MYSQL_PASSWORD="$mysql_password"
export PHASE8_BACKUP_MYSQL_ROOT_PASSWORD="$mysql_root_password"

mutation_attempted=true
docker run --detach --rm \
    --network "$network_name" \
    --name "$tripwire_name" \
    --entrypoint node \
    "$expected_image" \
    -e 'require("net").createServer(socket => { console.log("TRIPWIRE_HIT"); socket.destroy(); }).listen(9000, "0.0.0.0", () => console.log("TRIPWIRE_READY")); setInterval(() => {}, 60000);' \
    >"$private_directory/tripwire.container-id"
for _ in {1..30}; do
    if docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_READY; then
        break
    fi
    sleep 1
done
if ! docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_READY; then
    echo "the local Backup tripwire did not become ready" >&2
    exit 1
fi

# 1. Inert destinations. Creating them must make no contact.
destination_a_name="$prefix-destination-a"
destination_b_name="$prefix-destination-b"
destination_c_name="$prefix-destination-c"
destination_a_id="$(create_destination "$destination_a_name" a)"
destination_b_id="$(create_destination "$destination_b_name" b)"
destination_c_id="$(create_destination "$destination_c_name" c)"
assert_no_contact destination-creation

# Config writer. Every variable names one Backup field so each step edits one concern.
pg_target="postgres.main"
pg_destination="$destination_a_name"
pg_schedule="17 3 * * *"
pg_prefix="/pg-$run_id/"
pg_database="app"
pg_keep="2"
pg_include="false"
my_destination="$destination_a_name"
include_pg="true"
removed_block=""

write_config() {
    {
        cat <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 Backup validation
  environments:
    production:
      description: Managed by the Phase 8 Backup integration check
      postgres:
        main:
          database: app
          username: app
          password:
            env: PHASE8_BACKUP_POSTGRES_PASSWORD
      mysql:
        sql:
          database: app
          username: app
          password:
            env: PHASE8_BACKUP_MYSQL_PASSWORD
          root_password:
            env: PHASE8_BACKUP_MYSQL_ROOT_PASSWORD
      backups:
        my:
          target: mysql.sql
          destination:
            name: $my_destination
          schedule: "29 4 * * 2"
          prefix: "/my-$run_id/"
          database: app
          enabled: false
          keep_latest: 3
EOF
        if [[ "$include_pg" == true ]]; then
            cat <<EOF
        pg:
          target: $pg_target
          destination:
            name: $pg_destination
          schedule: "$pg_schedule"
          prefix: "$pg_prefix"
          database: $pg_database
          enabled: false
          keep_latest: $pg_keep
          include_encryption_key: $pg_include
EOF
        fi
        if [[ -n "$removed_block" ]]; then
            printf '%s\n' "$removed_block"
        fi
    } >"$config_file"
    chmod 600 "$config_file"
}

run_apply() {
    local label="$1"

    cli apply --file "$config_file" --auto-approve \
        >"$workspace/$label.stdout" 2>"$workspace/$label.stderr"
}

require_noop_plan() {
    local label="$1" file="${2:-$config_file}"

    cli plan --file "$file" --json --detailed-exitcode \
        >"$workspace/$label.stdout" 2>"$workspace/$label.stderr"
}

plan_json() {
    local label="$1"

    cli plan --file "$config_file" --json \
        >"$workspace/$label.stdout" 2>"$workspace/$label.stderr"
}

# Reads one Backup directly and proves its owned fields without retaining
# identities: only booleans and the allowlisted fields reach retained evidence.
# Arguments: id label destination_id target_kind target_id schedule prefix database
#            keep(null|n) include(true|false)
capture_backup() {
    local id="$1" label="$2" destination="$3" kind="$4" target="$5" schedule="$6"
    local backup_prefix="$7" database="$8" keep="$9" include="${10}" raw status

    raw="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "backup.one?backupId=$(urlencode "$id")" "$raw")"
    if [[ "$status" != 200 ]]; then
        rm -f -- "$raw"
        echo "backup.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e --arg destination "$destination" --arg kind "$kind" --arg target "$target" \
        --arg schedule "$schedule" --arg prefix "$backup_prefix" --arg database "$database" \
        --arg keep "$keep" --argjson include "$include" '
        .enabled == false
        and .backupType == "database" and .databaseType == $kind
        and .destinationId == $destination
        and .schedule == $schedule and .prefix == $prefix and .database == $database
        and (if $keep == "null" then .keepLatestCount == null else .keepLatestCount == ($keep | tonumber) end)
        and .includeEncryptionKey == $include
        and .composeId == null and .serviceName == null
        and ((.deployments // []) | length) == 0
        and (if $kind == "postgres" then .postgresId == $target else .mysqlId == $target end)
    ' "$raw" >/dev/null; then
        rm -f -- "$raw"
        echo "backup.one did not prove the expected Backup fields ($label)" >&2
        return 1
    fi
    jq '{
        disabled:(.enabled == false),
        databaseBackup:(.backupType == "database"),
        retentionCleared:(.keepLatestCount == null),
        encryptionKeyIncluded:.includeEncryptionKey,
        deploymentCount:((.deployments // []) | length)
    }' "$raw" >"$workspace/backup.$label.json"
    rm -f -- "$raw"
}

capture_backup_absence() {
    local id="$1" label="$2" raw status

    raw="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "backup.one?backupId=$(urlencode "$id")" "$raw")"
    rm -f -- "$raw"
    printf '%s\n' "$status" >"$workspace/backup.$label.status"
    [[ "$status" == 404 ]]
}

# The authoritative target relation lists exactly the expected Backup identities.
assert_target_backups() {
    local kind="$1" id="$2" expected="$3" label="$4" raw status

    raw="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "$kind.one?${kind}Id=$(urlencode "$id")" "$raw")"
    require_status "$status" 200 "$kind.one $label"
    if ! jq -e --argjson expected "$expected" '
        .applicationStatus == "idle" and ((.deployments // []) | length) == 0
        and ([.backups[]?.backupId] | sort) == ($expected | sort)
        and ([.backups[]? | ((.deployments // []) | length)] | all(. == 0))
    ' "$raw" >/dev/null; then
        rm -f -- "$raw"
        echo "$kind.one did not prove the expected Backup relation ($label)" >&2
        return 1
    fi
    jq '{
        targetIdle:(.applicationStatus == "idle"),
        targetDeploymentCount:((.deployments // []) | length),
        backupCount:((.backups // []) | length)
    }' "$raw" >"$workspace/$kind.$label.json"
    rm -f -- "$raw"
}

# 2. Create two Backups on two database kinds, disabled and undeployed.
write_config
run_apply create
require_noop_plan after-create
state_file="$workspace/.dokploy/state.json"
project_id="$(state_value "$state_file" "project.$project_name")"
environment_id="$(state_value "$state_file" environment.production)"
postgres_id="$(state_value "$state_file" postgres.main)"
mysql_id="$(state_value "$state_file" mysql.sql)"
pg_backup_id="$(state_value "$state_file" backup.pg)"
my_backup_id="$(state_value "$state_file" backup.my)"
capture_backup "$pg_backup_id" pg-created "$destination_a_id" postgres "$postgres_id" \
    "17 3 * * *" "/pg-$run_id/" app 2 false
capture_backup "$my_backup_id" my-created "$destination_a_id" mysql "$mysql_id" \
    "29 4 * * 2" "/my-$run_id/" app 3 false
assert_target_backups postgres "$postgres_id" "[\"$pg_backup_id\"]" created
assert_target_backups mysql "$mysql_id" "[\"$my_backup_id\"]" created
if ! jq -e --arg destination "$destination_a_name" --arg target "postgres.main" '
    .resources["backup.pg"].lastApplied.destination == {name:$destination}
    and .resources["backup.pg"].lastApplied.target == $target
    and .resources["backup.pg"].dependencies == [$target]
' "$state_file" >/dev/null; then
    echo "durable state does not hold the target dependency and a destination name selector" >&2
    exit 1
fi
assert_no_contact create

# 3. Every mutable field updates in place and keeps the physical Backup identity.
pg_schedule="42 5 * * 1"
pg_prefix="/pg-updated-$run_id/"
pg_database="app_updated"
pg_keep="5"
pg_include="true"
write_config
plan_json plan-inplace
jq -e '([.changes[] | select(.kind == "update")] | length) == 1
    and ([.changes[] | select(.kind == "replace")] | length) == 0' \
    "$workspace/plan-inplace.stdout" >/dev/null
run_apply inplace
require_noop_plan after-inplace
if [[ "$(state_value "$state_file" backup.pg)" != "$pg_backup_id" ]]; then
    echo "an in-place update replaced the physical Backup" >&2
    exit 1
fi
capture_backup "$pg_backup_id" pg-inplace "$destination_a_id" postgres "$postgres_id" \
    "42 5 * * 1" "/pg-updated-$run_id/" app_updated 5 true
pg_keep="null"
write_config
run_apply clear-retention
require_noop_plan after-clear-retention
capture_backup "$pg_backup_id" pg-cleared "$destination_a_id" postgres "$postgres_id" \
    "42 5 * * 1" "/pg-updated-$run_id/" app_updated null true
assert_no_contact inplace

# 4. Destination re-selection updates the association in place.
pg_destination="$destination_b_name"
write_config
plan_json plan-reselect
jq -e '[.changes[] | select(.kind == "update")] | length == 1' \
    "$workspace/plan-reselect.stdout" >/dev/null
run_apply reselect
require_noop_plan after-reselect
if [[ "$(state_value "$state_file" backup.pg)" != "$pg_backup_id" ]]; then
    echo "a destination change replaced the physical Backup" >&2
    exit 1
fi
capture_backup "$pg_backup_id" pg-reselected "$destination_b_id" postgres "$postgres_id" \
    "42 5 * * 1" "/pg-updated-$run_id/" app_updated null true
assert_no_contact reselect

# 5. A target change replaces the Backup, delete before create.
pg_target="mysql.sql"
write_config
plan_json plan-replace
jq -e '[.changes[] | select(.kind == "replace" and .replacementOrder == "delete_before_create")] | length == 1' \
    "$workspace/plan-replace.stdout" >/dev/null
run_apply replace
require_noop_plan after-replace
replacement_id="$(state_value "$state_file" backup.pg)"
if [[ "$replacement_id" == "$pg_backup_id" ]]; then
    echo "a target change did not replace the physical Backup" >&2
    exit 1
fi
if ! capture_backup_absence "$pg_backup_id" pg-replaced; then
    echo "backup.one did not prove absence of the replaced Backup identity" >&2
    exit 1
fi
capture_backup "$replacement_id" pg-replacement "$destination_b_id" mysql "$mysql_id" \
    "42 5 * * 1" "/pg-updated-$run_id/" app_updated null true
assert_target_backups postgres "$postgres_id" '[]' replaced-source
assert_target_backups mysql "$mysql_id" "[\"$my_backup_id\",\"$replacement_id\"]" replaced-target
pg_backup_id="$replacement_id"
assert_no_contact replace

# 6. Declarative deletion with authoritative absence.
include_pg=false
removed_block='removed:
  - from: backup.pg
    destroy: true'
write_config
run_apply delete
require_noop_plan after-delete
if ! capture_backup_absence "$pg_backup_id" pg-deleted; then
    echo "backup.one did not prove absence after declarative deletion" >&2
    exit 1
fi
assert_target_backups mysql "$mysql_id" "[\"$my_backup_id\"]" deleted
if jq -e '.resources["backup.pg"]' "$state_file" >/dev/null; then
    echo "the deleted Backup identity remains in durable state" >&2
    exit 1
fi
assert_no_contact delete

# 7. Saved-plan binding of the resolved destination identity. The selected
# destination is re-created under the same name before apply; the byte-identical
# plan must be refused.
my_destination="$destination_c_name"
write_config
cli plan --file "$config_file" --out "$workspace/saved-stale.json" \
    >"$workspace/saved-stale.plan.stdout" 2>"$workspace/saved-stale.plan.stderr"
jq -e '.plan.applyable == true and (.plan.changes | length) == 1
    and (.remoteReceipt | test("^[0-9a-f]{64}$"))' "$workspace/saved-stale.json" >/dev/null
for identity in "$destination_a_id" "$destination_b_id" "$destination_c_id"; do
    if grep -q -F -- "$identity" "$workspace/saved-stale.json"; then
        echo "the saved plan envelope contains an external identity" >&2
        exit 1
    fi
done
for name in "$destination_a_name" "$destination_b_name" "$destination_c_name"; do
    if grep -q -F -- "$name" "$workspace/saved-stale.json"; then
        echo "the saved plan envelope contains an external name" >&2
        exit 1
    fi
done
remove_destination "$destination_c_id"
destination_c_id="$(create_destination "$destination_c_name" c-recreated)"
if cli apply "$workspace/saved-stale.json" --file "$config_file" --auto-approve \
    >"$workspace/saved-stale.apply.stdout" 2>"$workspace/saved-stale.apply.stderr"
then
    echo "a saved plan was applied after its destination resolution changed" >&2
    exit 1
fi
if ! grep -q "saved plan cannot be applied safely" "$workspace/saved-stale.apply.stderr"; then
    echo "the stale saved plan was not refused by the receipt check" >&2
    exit 1
fi
capture_backup "$my_backup_id" my-after-refusal "$destination_a_id" mysql "$mysql_id" \
    "29 4 * * 2" "/my-$run_id/" app 3 false
cli plan --file "$config_file" --out "$workspace/saved-fresh.json" \
    >"$workspace/saved-fresh.plan.stdout" 2>"$workspace/saved-fresh.plan.stderr"
cli apply "$workspace/saved-fresh.json" --file "$config_file" --auto-approve \
    >"$workspace/saved-fresh.apply.stdout" 2>"$workspace/saved-fresh.apply.stderr"
require_noop_plan after-saved-fresh
capture_backup "$my_backup_id" my-after-saved "$destination_c_id" mysql "$mysql_id" \
    "29 4 * * 2" "/my-$run_id/" app 3 false
assert_no_contact saved-plan

# 8. Unresolved destinations block the plan and the apply without mutation.
my_destination="$prefix-destination-absent"
write_config
status=0
cli plan --file "$config_file" --json --detailed-exitcode \
    >"$workspace/blocked.stdout" 2>"$workspace/blocked.stderr" || status=$?
if [[ "$status" -ne 1 ]]; then
    echo "an unresolved destination did not block the plan" >&2
    exit 1
fi
jq -e '
    .applyable == false
    and ([.diagnostics[] | select(.code == "DOKPLAN019" and .selectorFailure == "unmatched"
        and .property == "destination")] | length) == 1
' "$workspace/blocked.stdout" >/dev/null
if cli apply --file "$config_file" --auto-approve \
    >"$workspace/blocked.apply.stdout" 2>"$workspace/blocked.apply.stderr"
then
    echo "apply accepted an unresolved destination" >&2
    exit 1
fi
my_destination="$destination_c_name"
write_config
require_noop_plan after-blocked

# 9. Protected import of an out-of-band Backup converges on the first fresh plan.
jq -n --arg mysqlId "$mysql_id" --arg destinationId "$destination_a_id" --arg run "$run_id" '
    {
        schedule:"11 2 * * 3",enabled:false,prefix:("/oob-" + $run + "/"),
        destinationId:$destinationId,keepLatestCount:4,database:"app",
        databaseType:"mysql",backupType:"database",serviceName:null,
        includeEncryptionKey:false,metadata:null,mysqlId:$mysqlId
    }
' >"$private_directory/oob-create.request.json"
require_status "$(api_post backup.create "$private_directory/oob-create.request.json" \
    "$private_directory/oob-create.json")" 200 "backup.create out-of-band"
require_status "$(api_get "mysql.one?mysqlId=$(urlencode "$mysql_id")" \
    "$private_directory/mysql-one.oob.json")" 200 "mysql.one out-of-band"
oob_id="$(jq -er --arg run "$run_id" \
    '.backups[] | select(.prefix == ("/oob-" + $run + "/")) | .backupId' \
    "$private_directory/mysql-one.oob.json")"
cli import backup "$oob_id" --as backup.adopted --file "$import_config_file" \
    >"$workspace/import.stdout" 2>"$workspace/import.stderr"
require_noop_plan after-import "$import_config_file"
if ! jq -e --arg destination "$destination_a_name" '
    .resources["backup.adopted"].protected == true
    and .resources["backup.adopted"].lastApplied.destination == {name:$destination}
    and (.resources["backup.adopted"].dependencies | length) == 1
' "$import_directory/.dokploy/state.json" >/dev/null; then
    echo "the imported Backup is not protected or does not hold a destination name selector" >&2
    exit 1
fi
if ! grep -q -F -- "$destination_a_name" "$import_config_file"; then
    echo "the imported configuration does not select the destination by name" >&2
    exit 1
fi
for identity in "$destination_a_id" "$destination_b_id" "$destination_c_id"; do
    if grep -q -F -- "$identity" "$import_config_file" "$import_directory/.dokploy/state.json"; then
        echo "the imported workspace contains an external identity" >&2
        exit 1
    fi
done
capture_backup "$oob_id" oob "$destination_a_id" mysql "$mysql_id" \
    "11 2 * * 3" "/oob-$run_id/" app 4 false

# 10. Zero deployments, zero backup executions, zero destination contact.
assert_target_backups postgres "$postgres_id" '[]' final
assert_target_backups mysql "$mysql_id" "[\"$my_backup_id\",\"$oob_id\"]" final
assert_no_contact final
jq -n --argjson tripwireHits "$(tripwire_hits)" '
    {tripwireHits:$tripwireHits,deployments:0,backupExecutions:0}
' >"$workspace/contact-summary.json"
if ! jq -e '.tripwireHits == 0 and .deployments == 0 and .backupExecutions == 0' \
    "$workspace/contact-summary.json" >/dev/null
then
    echo "the contact summary does not prove an inert run" >&2
    exit 1
fi

echo "Backup declarative create (PostgreSQL and MySQL targets, disabled, undeployed), no-op convergence, in-place update of every mutable field, destination re-selection, target replacement, deletion with authoritative absence, saved-plan destination binding, unresolved-destination blocking, protected import, zero-deployment, zero-execution, and zero-destination-contact checks passed."
