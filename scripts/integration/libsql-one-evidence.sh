#!/usr/bin/env bash

pending_libsql_raw_response=""

discard_pending_libsql_raw_response() {
    local raw_response="$pending_libsql_raw_response"

    if [[ -z "$raw_response" ]]; then
        return 0
    fi

    rm -f -- "$raw_response"
    if [[ -e "$raw_response" ]]; then
        echo "could not discard a private LibSQL response" >&2
        return 1
    fi

    pending_libsql_raw_response=""
}

capture_libsql_one_evidence() {
    local endpoint="$1"
    local destination="$2"
    local candidate=""
    local status=""

    if ! discard_pending_libsql_raw_response; then
        return 1
    fi
    if ! pending_libsql_raw_response="$(mktemp "${TMPDIR:-/tmp}/dokploy-libsql-one.XXXXXX")"; then
        pending_libsql_raw_response=""
        return 1
    fi
    if ! chmod 600 "$pending_libsql_raw_response"; then
        discard_pending_libsql_raw_response
        return 1
    fi

    if ! candidate="$(mktemp "$destination.candidate.XXXXXX")"; then
        discard_pending_libsql_raw_response
        return 1
    fi
    if ! chmod 600 "$candidate"; then
        rm -f -- "$candidate"
        discard_pending_libsql_raw_response
        return 1
    fi

    if ! status="$(api_get "$endpoint" "$pending_libsql_raw_response")"; then
        rm -f -- "$candidate"
        discard_pending_libsql_raw_response
        echo "libsql.one request failed" >&2
        return 1
    fi
    if [[ "$status" != "200" ]]; then
        rm -f -- "$candidate"
        discard_pending_libsql_raw_response
        echo "libsql.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq '{libsqlId, sqldNode, applicationStatus}' \
        "$pending_libsql_raw_response" >"$candidate"; then
        rm -f -- "$candidate"
        discard_pending_libsql_raw_response
        echo "libsql.one returned an invalid response" >&2
        return 1
    fi
    if ! jq -e '
        (keys | sort) == ["applicationStatus", "libsqlId", "sqldNode"]
        and (.libsqlId | type) == "string"
        and (.sqldNode | type) == "string"
        and (.applicationStatus | type) == "string"
    ' "$candidate" >/dev/null; then
        rm -f -- "$candidate"
        discard_pending_libsql_raw_response
        echo "libsql.one returned an invalid response" >&2
        return 1
    fi
    if ! discard_pending_libsql_raw_response; then
        rm -f -- "$candidate"
        return 1
    fi

    mv -- "$candidate" "$destination"
}

capture_libsql_one_absence_evidence() {
    local endpoint="$1"
    local destination="$2"
    local candidate=""
    local status=""

    if ! discard_pending_libsql_raw_response; then
        return 1
    fi
    if ! pending_libsql_raw_response="$(mktemp "${TMPDIR:-/tmp}/dokploy-libsql-one.XXXXXX")"; then
        pending_libsql_raw_response=""
        return 1
    fi
    if ! chmod 600 "$pending_libsql_raw_response"; then
        discard_pending_libsql_raw_response
        return 1
    fi

    if ! status="$(api_get "$endpoint" "$pending_libsql_raw_response")"; then
        discard_pending_libsql_raw_response
        return 1
    fi
    if ! discard_pending_libsql_raw_response; then
        return 1
    fi

    if ! candidate="$(mktemp "$destination.candidate.XXXXXX")"; then
        return 1
    fi
    if ! chmod 600 "$candidate"; then
        rm -f -- "$candidate"
        return 1
    fi
    if ! printf '%s\n' "$status" >"$candidate"; then
        rm -f -- "$candidate"
        return 1
    fi
    mv -- "$candidate" "$destination"

    [[ "$status" == "404" ]]
}
