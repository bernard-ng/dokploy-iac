#!/usr/bin/env bash
#
# Records the live contract of the Dokploy volume backup endpoints (a scheduled copy of a
# volume of an application, a Compose stack, or a database to a destination) as sanitised
# fixtures for the selected Dokploy version. Things the capture proves, because the engine and
# the simulator depend on them:
#   - create answers the whole row for each parent kind, with `serviceType` and the parent's
#     id column set and the others null, plus a generated `appName`;
#   - `volumeBackups.list` is read per parent with its id and type and nests the parent;
#     `volumeBackups.one` also nests the destination (without its secrets);
#   - update needs the name, volume, prefix, cron expression, and destination every time
#     (HTTP 400 otherwise) and answers the whole row;
#   - two volume backups of one parent may share a name, and a nonsense cron expression is
#     accepted;
#   - delete answers 200 with an empty body;
#   - removing a destination silently deletes the volume backups that name it.
# The script deletes everything it creates.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"

capture_begin volume-backup

destination_id=""

capture_cleanup() {
    capture_remove_project
    if [[ -n "$destination_id" ]]; then
        request_body "$workspace/cleanup.request.json" --arg id "$destination_id" '{destinationId:$id}'
        api_request POST destination.remove "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
    fi
}

require_runtime_version
capture_project_setup volume-backup

request_body "$workspace/destination.request.json" '
    {name:"volume-backup-contract-destination",provider:"",accessKey:"capture-access-key",
     secretAccessKey:"capture-secret-key",bucket:"capture-bucket",region:"us-east-1",
     endpoint:"http://s3.invalid:9000"}'
status="$(api_request POST destination.create "$workspace/destination-create.json" "$workspace/destination.request.json")"
require_status "$status" 200 "destination.create (setup)"
destination_id="$(jq -er '.destinationId' "$workspace/destination-create.json")"

list_backups() {
    local id="$1" type="$2" destination="$3"
    api_request GET "volumeBackups.list?id=$(urlencode "$id")&volumeBackupType=$type" "$destination"
}

request_body "$workspace/create.request.json" --arg application "$capture_application_id" --arg destination "$destination_id" '
    {name:"nightly",volumeName:"data",prefix:"nightly",cronExpression:"0 3 * * *",destinationId:$destination,
     serviceType:"application",applicationId:$application,turnOff:false,keepLatestCount:3,enabled:true}'
create_status="$(api_request POST volumeBackups.create "$workspace/volume-backup-create.json" "$workspace/create.request.json")"
require_status "$create_status" 200 volumeBackups.create
backup_id="$(jq -er '.volumeBackupId' "$workspace/volume-backup-create.json")"
jq -e --arg application "$capture_application_id" --arg destination "$destination_id" '
    .name == "nightly" and .volumeName == "data" and .prefix == "nightly" and .serviceType == "application"
    and .applicationId == $application and .postgresId == null and .composeId == null
    and .destinationId == $destination and .keepLatestCount == 3 and .enabled == true and .turnOff == false
    and .serviceName == null and (.appName | startswith("volumeBackup-")) and has("createdAt")' \
    "$workspace/volume-backup-create.json" >/dev/null

# The other parent kinds: the id column and `serviceType` follow the parent, defaults stay null.
request_body "$workspace/create-postgres.request.json" --arg postgres "$capture_postgres_id" --arg destination "$destination_id" '
    {name:"pg-data",volumeName:"pgdata",prefix:"pg",cronExpression:"0 4 * * *",destinationId:$destination,
     serviceType:"postgres",postgresId:$postgres}'
postgres_status="$(api_request POST volumeBackups.create "$workspace/volume-backup-create.postgres.json" "$workspace/create-postgres.request.json")"
require_status "$postgres_status" 200 "volumeBackups.create (postgres)"
jq -e --arg postgres "$capture_postgres_id" '.serviceType == "postgres" and .postgresId == $postgres
    and .applicationId == null and .keepLatestCount == null and .enabled == null and .turnOff == false' \
    "$workspace/volume-backup-create.postgres.json" >/dev/null

request_body "$workspace/create-compose.request.json" --arg compose "$capture_compose_id" --arg destination "$destination_id" '
    {name:"stack-data",volumeName:"stackdata",prefix:"stack",cronExpression:"0 5 * * *",destinationId:$destination,
     serviceType:"compose",composeId:$compose,serviceName:"web"}'
compose_status="$(api_request POST volumeBackups.create "$workspace/volume-backup-create.compose.json" "$workspace/create-compose.request.json")"
require_status "$compose_status" 200 "volumeBackups.create (compose)"
jq -e --arg compose "$capture_compose_id" '.serviceType == "compose" and .composeId == $compose and .serviceName == "web"' \
    "$workspace/volume-backup-create.compose.json" >/dev/null

list_backups "$capture_application_id" application "$workspace/volume-backup-list.created.json" >/dev/null
jq -e --arg id "$backup_id" --arg application "$capture_application_id" 'length == 1 and .[0].volumeBackupId == $id
    and .[0].application.applicationId == $application' "$workspace/volume-backup-list.created.json" >/dev/null
list_backups "$capture_postgres_id" postgres "$workspace/volume-backup-list.postgres.json" >/dev/null
jq -e 'length == 1 and .[0].serviceType == "postgres"' "$workspace/volume-backup-list.postgres.json" >/dev/null
list_backups "$capture_compose_id" compose "$workspace/volume-backup-list.compose.json" >/dev/null
jq -e 'length == 1 and .[0].serviceName == "web"' "$workspace/volume-backup-list.compose.json" >/dev/null
one_status="$(api_request GET "volumeBackups.one?volumeBackupId=$(urlencode "$backup_id")" "$workspace/volume-backup-one.created.json")"
require_status "$one_status" 200 "volumeBackups.one after create"
jq -e --arg destination "$destination_id" '.destination.destinationId == $destination
    and (.destination | has("accessKey") | not) and (.destination | has("secretAccessKey") | not)' \
    "$workspace/volume-backup-one.created.json" >/dev/null

# Update needs the name, volume, prefix, cron expression, and destination every time.
request_body "$workspace/partial.request.json" --arg id "$backup_id" '{volumeBackupId:$id,name:"renamed"}'
partial_status="$(api_request POST volumeBackups.update "$workspace/volume-backup-update.partial-rejected.json" "$workspace/partial.request.json")"
require_status "$partial_status" 400 "volumeBackups.update (partial)"

request_body "$workspace/full.request.json" --arg id "$backup_id" --arg destination "$destination_id" '
    {volumeBackupId:$id,name:"weekly",volumeName:"data2",prefix:"weekly",cronExpression:"0 4 * * 0",
     destinationId:$destination,serviceType:"application",turnOff:true,keepLatestCount:null,enabled:false,
     serviceName:null}'
update_status="$(api_request POST volumeBackups.update "$workspace/volume-backup-update.json" "$workspace/full.request.json")"
require_status "$update_status" 200 "volumeBackups.update (full)"
jq -e '.name == "weekly" and .volumeName == "data2" and .turnOff == true and .keepLatestCount == null
    and .enabled == false and .cronExpression == "0 4 * * 0"' "$workspace/volume-backup-update.json" >/dev/null
api_request GET "volumeBackups.one?volumeBackupId=$(urlencode "$backup_id")" "$workspace/volume-backup-one.updated.json" >/dev/null
jq -e '.name == "weekly" and .keepLatestCount == null and .enabled == false' "$workspace/volume-backup-one.updated.json" >/dev/null

# Names are not unique, and the cron expression is not validated.
request_body "$workspace/duplicate.request.json" --arg application "$capture_application_id" --arg destination "$destination_id" '
    {name:"weekly",volumeName:"data",prefix:"dup",cronExpression:"not a schedule",destinationId:$destination,
     serviceType:"application",applicationId:$application}'
duplicate_status="$(api_request POST volumeBackups.create "$workspace/volume-backup-create.duplicate.json" "$workspace/duplicate.request.json")"
duplicate_allowed=false
if [[ "$duplicate_status" == 200 ]]; then duplicate_allowed=true; fi
list_backups "$capture_application_id" application "$workspace/volume-backup-list.duplicate.json" >/dev/null
jq -e 'length == 2' "$workspace/volume-backup-list.duplicate.json" >/dev/null
duplicate_id="$(jq -er '.volumeBackupId' "$workspace/volume-backup-create.duplicate.json")"
request_body "$workspace/remove-duplicate.request.json" --arg id "$duplicate_id" '{volumeBackupId:$id}'
api_request POST volumeBackups.delete "$workspace/volume-backup-remove.duplicate.json" "$workspace/remove-duplicate.request.json" >/dev/null

request_body "$workspace/remove.request.json" --arg id "$backup_id" '{volumeBackupId:$id}'
remove_status="$(api_request POST volumeBackups.delete "$workspace/volume-backup-remove.json" "$workspace/remove.request.json")"
require_status "$remove_status" 200 volumeBackups.delete
# Delete answers 200 with no body at all, so there is no fixture of it.
if [[ -s "$workspace/volume-backup-remove.json" ]]; then
    echo "volumeBackups.delete answered with a body; the capture assumes it is empty." >&2
    exit 1
fi
removed_one_status="$(api_request GET "volumeBackups.one?volumeBackupId=$(urlencode "$backup_id")" "$workspace/volume-backup-one.removed.json")"
require_status "$removed_one_status" 404 "volumeBackups.one after delete"
list_backups "$capture_application_id" application "$workspace/volume-backup-list.removed.json" >/dev/null
jq -e 'length == 0' "$workspace/volume-backup-list.removed.json" >/dev/null

# Removing the destination deletes the postgres and Compose volume backups that name it.
request_body "$workspace/destination-remove.request.json" --arg id "$destination_id" '{destinationId:$id}'
destination_remove_status="$(api_request POST destination.remove "$workspace/destination-remove.json" "$workspace/destination-remove.request.json")"
require_status "$destination_remove_status" 200 "destination.remove (referenced)"
destination_id=""
list_backups "$capture_postgres_id" postgres "$workspace/volume-backup-list.destination-removed.json" >/dev/null
cascade_deletes=false
if jq -e 'length == 0' "$workspace/volume-backup-list.destination-removed.json" >/dev/null; then
    cascade_deletes=true
fi

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
    --argjson createStatus "$create_status" --argjson partialStatus "$partial_status" \
    --argjson duplicateAllowed "$duplicate_allowed" --argjson removeStatus "$remove_status" \
    --argjson removedOneStatus "$removed_one_status" --argjson cascadeDeletes "$cascade_deletes" '
    {
        capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true,
        createReturns:"object", updateReturns:"object", removeReturns:"empty",
        updateRequiresNameVolumePrefixCronDestination:true,
        partialUpdateStatus:$partialStatus, duplicateNamesAllowed:$duplicateAllowed,
        cronExpressionValidated:false, listNestsParent:true, oneNestsDestinationWithoutCredentials:true,
        removingDestinationDeletesVolumeBackups:$cascadeDeletes,
        createStatus:$createStatus, removeStatus:$removeStatus, removedOneStatus:$removedOneStatus
    }' >"$publish_directory/volume-backup-contract.metadata.json"

capture_publish volume-backup-create volume-backup-create.postgres volume-backup-create.compose \
    volume-backup-list.created volume-backup-list.postgres volume-backup-list.compose volume-backup-one.created \
    volume-backup-update.partial-rejected volume-backup-update volume-backup-one.updated \
    volume-backup-create.duplicate volume-backup-one.removed volume-backup-list.removed \
    volume-backup-list.destination-removed
capture_succeeded=true

echo "Captured the sanitised volume backup contract."
