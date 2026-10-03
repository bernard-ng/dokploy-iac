#!/usr/bin/env bash
#
# Shared helpers for the settings-kind contract captures (registry, tag). Sourced after
# common.sh. The older project-kind captures carry their own copies of these helpers.
#
#   capture_begin NAME        private workspace, API header, cleanup trap
#   api_request M EP OUT [B]  one request; prints the HTTP status
#   require_status GOT WANT OP
#   capture_publish NAME...   sanitise the named workspace files into the fixtures
#
# The caller defines `capture_cleanup` to remove what it created; it runs on every exit.

if [[ "$-" == *x* ]]; then set +x; fi

capture_api_key_file="$state_directory/api-key"
capture_sanitizer="$script_directory/sanitize-fixture.jq"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

capture_begin() {
    local name="$1"
    if [[ ! -s "$capture_api_key_file" ]]; then
        echo "Run scripts/integration/up.sh before capturing the $name contract." >&2
        exit 1
    fi
    umask 077
    mkdir -p "$state_directory"
    chmod 700 "$state_directory"
    capture_name="$name"
    run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
    workspace="$state_directory/$name-contract-$run_id"
    publish_directory="$workspace/publish"
    mkdir -p "$publish_directory"
    chmod 700 "$workspace" "$publish_directory"
    auth_header_file="$workspace/api-header"
    {
        printf 'x-api-key: '
        tr -d '\r\n' <"$capture_api_key_file"
        printf '\n'
    } >"$auth_header_file"
    chmod 600 "$auth_header_file"
    capture_succeeded=false
    trap capture_exit EXIT INT TERM
}

capture_exit() {
    local exit_code="$?"
    trap - EXIT INT TERM
    set +e
    if declare -F capture_cleanup >/dev/null; then
        capture_cleanup || exit_code=1
    fi
    if [[ "$capture_succeeded" == true ]]; then
        case "$workspace" in
            "$state_directory"/"$capture_name"-contract-*) find "$workspace" -depth -delete ;;
        esac
    else
        echo "The $capture_name capture evidence remains in $workspace" >&2
    fi
    exit "$exit_code"
}

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

# A JSON object from jq arguments into a request body file.
request_body() {
    local destination="$1"
    shift
    jq -n "$@" >"$destination"
}

require_status() {
    local actual="$1" expected="$2" operation="$3"
    if [[ "$actual" != "$expected" ]]; then
        echo "$operation returned HTTP $actual; expected HTTP $expected." >&2
        return 1
    fi
}

require_runtime_version() {
    local status
    status="$(api_request GET settings.getDokployVersion "$workspace/version.json")"
    require_status "$status" 200 settings.getDokployVersion
    runtime_version="$(jq -er '.' "$workspace/version.json")"
    if [[ "$runtime_version" != "$dokploy_version" ]]; then
        echo "Expected Dokploy $dokploy_version, received $runtime_version." >&2
        exit 1
    fi
}

# Sanitise the named workspace files (without .json) into fixtures, check the candidate
# directory with the fixture checker, and only then publish. Metadata files are written
# by the caller straight into $publish_directory.
capture_publish() {
    local fixture_name
    for fixture_name in "$@"; do
        jq --sort-keys --indent 2 --from-file "$capture_sanitizer" \
            "$workspace/$fixture_name.json" >"$publish_directory/$fixture_name.owner.json"
    done

    local candidate_root="$workspace/candidate/api/live"
    local candidate="$candidate_root/$dokploy_version"
    mkdir -p "$candidate"
    if [[ -d "$dokploy_fixture_directory" ]]; then
        cp -R "$dokploy_fixture_directory/." "$candidate/"
    fi
    local fixture
    for fixture in "$publish_directory"/*.json; do
        cp "$fixture" "$candidate/$(basename "$fixture")"
    done
    find "$candidate_root" -type d -exec chmod 755 {} +
    find "$candidate_root" -type f -exec chmod 644 {} +

    if grep -R -F -q -f "$capture_api_key_file" "$candidate_root"; then
        echo "The sanitised $capture_name fixtures contain the local API key." >&2
        return 1
    fi
    DOKPLOY_FIXTURE_DIRECTORY="$candidate_root" "$script_directory/check-fixtures.sh"

    mkdir -p "$dokploy_fixture_directory"
    for fixture in "$publish_directory"/*.json; do
        install -m 0644 "$fixture" "$dokploy_fixture_directory/$(basename "$fixture")"
    done
}
