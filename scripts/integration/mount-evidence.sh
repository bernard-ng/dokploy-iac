#!/usr/bin/env bash

scrub_mount_private_evidence() {
    local private_directory="$1"
    local workspace="$2"

    case "$private_directory" in
        "$workspace"/private)
            find "$private_directory" -depth -delete 2>/dev/null
            ;;
        *)
            echo "refusing to remove an unexpected private Mount evidence directory" >&2
            return 1
            ;;
    esac

    if [[ -e "$private_directory" ]]; then
        echo "could not discard private Mount evidence" >&2
        return 1
    fi
}

assert_mount_retained_evidence_secret_free() {
    local workspace="$1"
    shift
    local secret

    for secret in "$@"; do
        if [[ -n "$secret" ]] && grep -R -F -q -- "$secret" "$workspace"; then
            echo "a Mount secret appeared in retained integration evidence" >&2
            return 1
        fi
    done
}
