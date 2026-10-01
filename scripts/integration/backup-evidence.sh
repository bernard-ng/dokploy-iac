#!/usr/bin/env bash

# Safe failure-evidence helpers for the Backup SDK integration lifecycle.

backup_clear_evidence_directory() {
    local directory="$1"

    case "$directory" in
        */backup-sdk-test-*/evidence | */backup-sdk-test-*/evidence.staged) ;;
        *) echo "Refusing to clear an unexpected Backup SDK evidence directory." >&2; return 1 ;;
    esac
    if [[ -d "$directory" ]]; then
        find "$directory" -depth -delete
    fi
}

backup_publish_safe_failure_evidence() {
    local private_directory="$1" evidence_directory="$2" exit_code="$3"
    local mutation_attempted="$4" cleanup_confirmed="$5" tripwire_hit_count="$6"
    local staged_directory="$evidence_directory.staged"

    backup_clear_evidence_directory "$staged_directory"
    mkdir -p "$staged_directory"
    chmod 700 "$staged_directory"

    jq -n \
        --argjson exitCode "$exit_code" \
        --argjson mutationAttempted "$mutation_attempted" \
        --argjson cleanupConfirmed "$cleanup_confirmed" \
        --argjson tripwireHitCount "$tripwire_hit_count" \
        '{
            exitCode:$exitCode,
            mutationAttempted:$mutationAttempted,
            cleanupConfirmed:$cleanupConfirmed,
            tripwireHitCount:$tripwireHitCount
        }' >"$staged_directory/summary.json"

    local source label
    for label in updated after; do
        source="$private_directory/postgres-one.$label.json"
        if [[ -s "$source" ]]; then
            jq -e '
                {
                    targetIdentityPresent:(.postgresId | type == "string" and length > 0),
                    targetIdle:(.applicationStatus == "idle"),
                    targetDeploymentCount:((.deployments // []) | length),
                    backupCount:((.backups // []) | length),
                    backups:[(.backups // [])[] | {
                        identityPresent:(.backupId | type == "string" and length > 0),
                        destinationIdentityPresent:(.destinationId | type == "string" and length > 0),
                        disabled:(.enabled == false),
                        retentionCount:(.keepLatestCount // null),
                        encryptionKeyIncluded:(.includeEncryptionKey // null),
                        databaseBackup:(.backupType == "database"),
                        postgresTarget:(.databaseType == "postgres"),
                        deploymentCount:((.deployments // []) | length)
                    }]
                }
            ' "$source" >"$staged_directory/postgres-one.$label.safe.json" ||
                rm -f -- "$staged_directory/postgres-one.$label.safe.json"
        fi
    done

    backup_clear_evidence_directory "$evidence_directory"
    mv -- "$staged_directory" "$evidence_directory"
}

backup_scrub_private_directory() {
    local private_directory="$1"

    case "$private_directory" in
        */backup-sdk-test-*/private) ;;
        *) echo "Refusing to scrub an unexpected Backup SDK private directory." >&2; return 1 ;;
    esac
    if [[ -d "$private_directory" ]]; then
        find "$private_directory" -depth -delete
    fi
}
