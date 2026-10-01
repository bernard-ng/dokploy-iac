#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=backup-evidence.sh
source "$script_directory/backup-evidence.sh"

test_root="$(mktemp -d "${TMPDIR:-/tmp}/backup-sdk-test-canary.XXXXXX")"
private_directory="$test_root/private"
evidence_directory="$test_root/evidence"
trap 'find "$test_root" -depth -delete' EXIT INT TERM
mkdir -p "$private_directory"

api_canary='backup-api-key-canary-do-not-retain'
database_canary='backup-db-password-canary-do-not-retain'
access_canary='backup-access-canary-do-not-retain'
secret_canary='backup-secret-canary-do-not-retain'

printf 'x-api-key: %s\n' "$api_canary" >"$private_directory/api-header"
jq -n --arg password "$database_canary" '{databasePassword:$password}' \
    >"$private_directory/postgres-create.request.json"
jq -n --arg access "$access_canary" --arg secret "$secret_canary" \
    '{accessKey:$access,secretAccessKey:$secret}' \
    >"$private_directory/destination-create.initial.request.json"
jq -n \
    --arg password "$database_canary" \
    --arg access "$access_canary" \
    --arg secret "$secret_canary" '
    {
        postgresId:"postgres-id",applicationStatus:"idle",password:$password,
        deployments:[],backups:[{
            backupId:"backup-id",destinationId:"destination-id",enabled:false,
            keepLatestCount:3,includeEncryptionKey:false,backupType:"database",
            databaseType:"postgres",deployments:[],
            destination:{accessKey:$access,secretAccessKey:$secret},
            postgres:{password:$password}
        }]
    }
' >"$private_directory/postgres-one.updated.json"

backup_publish_safe_failure_evidence \
    "$private_directory" "$evidence_directory" 1 true false 0
backup_scrub_private_directory "$private_directory"

if [[ -e "$private_directory" ]]; then
    echo "Backup private evidence was not scrubbed." >&2
    exit 1
fi
if rg -F \
    -e "$api_canary" \
    -e "$database_canary" \
    -e "$access_canary" \
    -e "$secret_canary" \
    "$evidence_directory" >/dev/null; then
    echo "Sanitized Backup evidence retained a secret canary." >&2
    exit 1
fi
if rg -n 'api-header|databasePassword|accessKey|secretAccessKey|"password"' \
    "$evidence_directory" >/dev/null; then
    echo "Sanitized Backup evidence retained a credential-bearing field." >&2
    exit 1
fi
jq -e '
    .targetIdentityPresent == true
    and .targetIdle == true
    and .targetDeploymentCount == 0
    and .backupCount == 1
    and .backups[0].identityPresent == true
    and .backups[0].destinationIdentityPresent == true
    and .backups[0].deploymentCount == 0
' "$evidence_directory/postgres-one.updated.safe.json" >/dev/null

echo "Backup failure evidence publication is secret-safe."
