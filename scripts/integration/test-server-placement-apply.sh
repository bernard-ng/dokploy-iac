#!/usr/bin/env bash

# Live acceptance for Compose and database server placement selectors.
#
# Compose, PostgreSQL, MySQL, MariaDB, MongoDB, LibSQL, and Redis accept a server
# only on their create endpoints. This script creates disposable, inert server
# records, places one service of every kind on a named server, on the local host,
# and with no placement, and proves creation, no-op convergence, create-only
# replacement (non-local to non-local, local to non-local, non-local to local),
# unmatched and ambiguous blocking, saved-plan receipt binding, protected
# name-selector import, and identity-scoped cleanup. The Dokploy instance is
# shared: serialize runs with .integration/live.lock.
#
# Inertness:
#   - server records point at a tripwire that counts every connection and have no
#     SSH key, so Dokploy cannot reach them; any contact fails the run;
#   - no service is ever deployed, started, or redeployed, and every touched
#     service must stay idle with zero deployments.

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

kinds="compose postgres mysql mariadb mongo libsql redis"

umask 077
workspace="$(mktemp -d "$state_directory/phase8-server-placement.XXXXXX")"
private_directory="$workspace/private"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
prefix="phase8-placement-$run_id"
project_name="$prefix"
config_file="$workspace/dokploy.yaml"
api_header_file="$private_directory/api-header"
secret_canary_file="$private_directory/secret-canaries"
identity_file="$private_directory/external-identities"
tripwire_name="phase8-placement-tripwire-${run_id//[^a-zA-Z0-9]/}"
network_name="dokploy-iac-integration_default"
project_id=""
mutation_attempted=false
cleanup_confirmed=false
fingerprint_key="0199a0c8-2351-7c31-8899-2c8f81983ea5:$(openssl rand -hex 32)"
placement_password="$(openssl rand -hex 24)"
placement_root_password="$(openssl rand -hex 24)"

server_a_id=""
server_b_id=""
server_c_id=""
server_d_id=""
duplicate_one_id=""
duplicate_two_id=""

mkdir -p "$private_directory"
chmod 700 "$private_directory"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$api_header_file"
chmod 600 "$api_header_file"
printf '%s\n' "$fingerprint_key" "$placement_password" "$placement_root_password" \
    >>"$secret_canary_file"
: >"$identity_file"
printf 'services:\n  placement:\n    image: alpine:3.20\n' >"$workspace/compose.yaml"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

# Raw responses stay in the private directory; only allowlisted booleans and
# counts are written next to the retained evidence.
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
    local label="$1"

    if [[ "$(tripwire_hits)" != 0 ]]; then
        echo "an inert server was contacted ($label)" >&2
        return 1
    fi
}

create_server() {
    local name="$1" label="$2" response_status identity

    jq -n --arg name "$name" --arg host "$tripwire_name" '
        {
            name:$name,description:"Disposable server placement fixture",
            ipAddress:$host,port:9000,username:"root",sshKeyId:null,
            serverType:"deploy",enableDockerCleanup:false
        }
    ' >"$private_directory/server-create.$label.request.json"
    response_status="$(api_post server.create \
        "$private_directory/server-create.$label.request.json" \
        "$private_directory/server-create.$label.json")"
    require_status "$response_status" 200 "server.create $label"
    identity="$(jq -er '.serverId' "$private_directory/server-create.$label.json")"
    printf '%s\n' "$identity" >>"$identity_file"
    printf '%s' "$identity"
}

remove_server() {
    local identity="$1" response_status

    jq -n --arg id "$identity" '{serverId:$id}' >"$private_directory/server-remove.request.json"
    response_status="$(api_post server.remove "$private_directory/server-remove.request.json" \
        "$private_directory/server-remove.response.json")"
    if [[ "$response_status" != 200 && "$response_status" != 404 ]]; then
        echo "server.remove cleanup returned HTTP $response_status" >&2
        return 1
    fi
}

# Removes every disposable server record whose name carries this run's prefix.
remove_scoped_servers() {
    local status=0 identity

    if [[ "$(api_get server.all "$private_directory/cleanup-server-all.json")" == 200 ]]; then
        while IFS= read -r identity; do
            [[ -z "$identity" ]] || remove_server "$identity" || status=1
        done < <(jq -r --arg prefix "$prefix" '.[] | select(.name | startswith($prefix)) | .serverId' \
            "$private_directory/cleanup-server-all.json")
    else
        status=1
    fi

    return "$status"
}

prove_scoped_absence() {
    [[ "$(api_get server.all "$private_directory/absence-server-all.json")" == 200 ]] &&
        jq -e --arg prefix "$prefix" '[.[] | select(.name | startswith($prefix))] | length == 0' \
            "$private_directory/absence-server-all.json" >/dev/null
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
        echo "Server placement cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    if [[ "$mutation_attempted" == true ]]; then
        remove_scoped_servers || cleanup_status=1
        prove_scoped_absence || {
            echo "Server placement cleanup could not prove server absence" >&2
            cleanup_status=1
        }
    fi
    docker rm -f "$tripwire_name" >/dev/null 2>&1
    if docker inspect "$tripwire_name" >/dev/null 2>&1; then
        echo "Server placement cleanup could not remove its local fixture" >&2
        cleanup_status=1
    fi
    if [[ "$cleanup_status" -eq 0 ]]; then
        cleanup_confirmed=true
    fi

    # Retained evidence is scanned for the API key, fingerprint key, every secret
    # canary, and every external identity before the private directory is discarded.
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
    while IFS= read -r canary; do
        [[ -z "$canary" ]] && continue
        if grep -R -F -q --exclude-dir=private -- "$canary" "$workspace"; then
            scan_status=1
        fi
    done <"$identity_file"
    if [[ "$scan_status" -ne 0 ]]; then
        echo "secret or external identity material appeared in retained placement evidence" >&2
        cleanup_status=1
    fi

    if ! find "$private_directory" -depth -delete 2>/dev/null || [[ -e "$private_directory" ]]; then
        echo "Server placement cleanup could not discard private evidence" >&2
        cleanup_status=1
    fi

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-server-placement.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "Server placement integration evidence retained at $workspace" >&2
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
    # Optional per-worktree profile override that keeps workspace artifacts private
    # when several worktrees share one target directory.
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
export PLACEMENT_PASSWORD="$placement_password"
export PLACEMENT_ROOT_PASSWORD="$placement_root_password"

# Fixture: a connection tripwire every inert server record points at.
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
    echo "the local placement tripwire did not become ready" >&2
    exit 1
fi

# 1. Create inert, non-local server records.
server_a_name="$prefix-edge-a"
server_b_name="$prefix-edge-b"
server_c_name="$prefix-edge-c"
server_d_name="$prefix-edge-d"
duplicate_name="$prefix-edge-dup"
absent_name="$prefix-edge-absent"
server_a_id="$(create_server "$server_a_name" a)"
server_b_id="$(create_server "$server_b_name" b)"
server_c_id="$(create_server "$server_c_name" c)"
server_d_id="$(create_server "$server_d_name" d)"
duplicate_one_id="$(create_server "$duplicate_name" duplicate-one)"
duplicate_two_id="$(create_server "$duplicate_name" duplicate-two)"
assert_no_contact server-creation

# The configuration places the `placed` group on a selected server, the `local`
# group on the explicit local selector, leaves the `plain` group unmanaged, and
# optionally adds a `late` and a `late2` group, each on its own selector.
service_body() {
    case "$1" in
        compose)
            printf '        document:\n          file: compose.yaml\n'
            ;;
        postgres)
            printf '        database: app\n        username: app\n        password:\n          env: PLACEMENT_PASSWORD\n'
            ;;
        mysql | mariadb)
            printf '        database: app\n        username: app\n        password:\n          env: PLACEMENT_PASSWORD\n        root_password:\n          env: PLACEMENT_ROOT_PASSWORD\n'
            ;;
        mongo)
            printf '        username: app\n        password:\n          env: PLACEMENT_PASSWORD\n'
            ;;
        libsql)
            printf '        username: app\n        password:\n          env: PLACEMENT_PASSWORD\n        node:\n          type: primary\n'
            ;;
        redis)
            printf '        password:\n          env: PLACEMENT_PASSWORD\n'
            ;;
    esac
}

named() {
    if [[ -n "$1" ]]; then
        printf '        server:\n          name: %s\n' "$1"
    fi
}

local_server() {
    printf '        server:\n          local: true\n'
}

# write_config PLACED LOCAL LATE LATE2: each argument is a server fragment or the
# word "none" to omit the group; LOCAL is "local" or "plain" for the local group.
write_config() {
    local placed="$1" local_group="$2" late="$3" late2="$4" kind

    {
        printf 'version: 1\nproject:\n  name: %s\n  description: Disposable Phase 8 server placement validation\n' "$project_name"
        printf 'environments:\n  production:\n    description: Managed by the Phase 8 server placement integration check\n'
        for kind in $kinds; do
            printf '    %s:\n' "$kind"
            printf '      placed:\n'
            service_body "$kind"
            printf '%s\n' "$placed"
            printf '      local:\n'
            service_body "$kind"
            printf '%s\n' "$local_group"
            printf '      plain:\n'
            service_body "$kind"
            if [[ "$late" != none ]]; then
                printf '      late:\n'
                service_body "$kind"
                printf '%s\n' "$late"
            fi
            if [[ "$late2" != none ]]; then
                printf '      late2:\n'
                service_body "$kind"
                printf '%s\n' "$late2"
            fi
        done
    } >"$config_file"
    chmod 600 "$config_file"
}

# Dokploy v0.30.6 loses services from its `*.search` views when several services are
# created concurrently in one environment (a lost-update race in its access list),
# which the CLI correctly reports as conflicting topology. Every apply here is serial.
run_apply() {
    local label="$1"

    cli apply --file "$config_file" --auto-approve --parallelism 1 \
        >"$workspace/$label.stdout" 2>"$workspace/$label.stderr"
}

require_noop_plan() {
    local label="$1"

    cli plan --file "$config_file" --json --detailed-exitcode \
        >"$workspace/$label.stdout" 2>"$workspace/$label.stderr"
}

# Plans that must be blocked exit with failure and name the typed diagnostic once
# per service kind that selects the unresolved record.
require_blocked_plan() {
    local label="$1" failure="$2" expected="$3" status=0 identity

    cli plan --file "$config_file" --json --detailed-exitcode \
        >"$workspace/$label.stdout" 2>"$workspace/$label.stderr" || status=$?
    if [[ "$status" -ne 1 ]]; then
        echo "an unresolved selector did not block the plan ($label)" >&2
        return 1
    fi
    jq -e --arg failure "$failure" --argjson expected "$expected" '
        .applyable == false
        and ([.diagnostics[] | select(.code == "DOKPLAN019" and .selectorFailure == $failure)] | length) == $expected
        and (.changes | length) == 0
    ' "$workspace/$label.stdout" >/dev/null
    while IFS= read -r identity; do
        [[ -z "$identity" ]] && continue
        if grep -q -F -- "$identity" "$workspace/$label.stdout"; then
            echo "a blocked plan exposed an external identity ($label)" >&2
            return 1
        fi
    done <"$identity_file"
    if cli apply --file "$config_file" --auto-approve --parallelism 1 \
        >"$workspace/$label.apply.stdout" 2>"$workspace/$label.apply.stderr"; then
        echo "apply accepted a blocked plan ($label)" >&2
        return 1
    fi
}

service_endpoint() {
    printf '%s.one?%sId=' "$1" "$1"
}

# Proves one service's placement and that it never deployed, keeping only allowlisted
# fields. EXPECTED is "null" or the exact server identity.
capture_service() {
    local kind="$1" service_id="$2" destination="$3" expected="$4" raw status

    raw="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "$(service_endpoint "$kind")$(urlencode "$service_id")" "$raw")"
    if [[ "$status" != 200 ]]; then
        rm -f -- "$raw"
        echo "$kind.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e --arg expected "$expected" '
        (if $expected == "null" then .serverId == null else .serverId == $expected end)
        and ((.applicationStatus // .composeStatus // "idle") == "idle")
        and ((.deployments // []) | length) == 0
    ' "$raw" >/dev/null; then
        rm -f -- "$raw"
        echo "$kind.one did not prove the expected placement and an idle undeployed service" >&2
        return 1
    fi
    jq '{
        placed: (.serverId != null),
        status: (.applicationStatus // .composeStatus // "idle"),
        deploymentCount: ((.deployments // []) | length)
    }' "$raw" >"$destination"
    rm -f -- "$raw"
}

capture_absence() {
    local kind="$1" service_id="$2" destination="$3" raw status

    raw="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "$(service_endpoint "$kind")$(urlencode "$service_id")" "$raw")"
    rm -f -- "$raw"
    printf '%s\n' "$status" >"$destination"
    [[ "$status" != 200 ]]
}

state_file="$workspace/.dokploy/state.json"

# Remote identities of one group, captured as "kind=id" lines for later comparison.
group_ids() {
    local group="$1" kind

    for kind in $kinds; do
        printf '%s=%s\n' "$kind" "$(state_value "$state_file" "$kind.$group")"
    done
}

id_of() {
    local listing="$1" kind="$2"

    printf '%s\n' "$listing" | sed -n "s/^$kind=//p"
}

# Asserts that every service of a group is on the given server ("null" for local).
assert_group_placement() {
    local group="$1" expected="$2" label="$3" kind

    for kind in $kinds; do
        capture_service "$kind" "$(state_value "$state_file" "$kind.$group")" \
            "$workspace/$label.$kind.$group.json" "$expected"
    done
}

assert_state_selector() {
    local group="$1" selector="$2" kind

    for kind in $kinds; do
        if ! jq -e --arg address "$kind.$group" --argjson selector "$selector" '
            .resources[$address].lastApplied.server == $selector
        ' "$state_file" >/dev/null; then
            echo "durable state does not hold $kind.$group as a stable selector" >&2
            return 1
        fi
    done
}

# 2. Create every kind with a named, local, and unmanaged placement.
write_config "$(named "$server_a_name")" "$(local_server)" none none
run_apply create
require_noop_plan after-create
project_id="$(state_value "$state_file" "project.$project_name")"
assert_group_placement placed "$server_a_id" created
assert_group_placement local null created
assert_group_placement plain null created
assert_state_selector placed "$(jq -n --arg name "$server_a_name" '{name:$name}')"
assert_state_selector local '{"local":true}'
for kind in $kinds; do
    if jq -e --arg address "$kind.plain" '.resources[$address].lastApplied | has("server")' \
        "$state_file" >/dev/null
    then
        echo "an unmanaged placement was recorded in durable state" >&2
        exit 1
    fi
done
# `libsql.create` declares `serverId` required, so a body that omits it is rejected;
# the SDK therefore sends an explicit null for an unmanaged LibSQL placement (the
# `libsql.plain` service above was created that way).
environment_id="$(state_value "$state_file" environment.production)"
jq -n --arg environment "$environment_id" --arg password "$placement_password" '
    {
        name:"omitted-server-probe",appName:"omitted-server-probe",
        dockerImage:"ghcr.io/tursodatabase/libsql-server:v0.24.32",
        environmentId:$environment,description:null,databaseUser:"app",
        databasePassword:$password,sqldNode:"primary",sqldPrimaryUrl:null,
        enableNamespaces:false
    }
' >"$private_directory/libsql-omitted-server.request.json"
omitted_status="$(api_post libsql.create "$private_directory/libsql-omitted-server.request.json" \
    "$private_directory/libsql-omitted-server.json")"
if [[ "$omitted_status" != 4* ]]; then
    echo "libsql.create accepted a body without serverId (HTTP $omitted_status)" >&2
    exit 1
fi
require_status "$(api_get "project.one?projectId=$(urlencode "$project_id")" \
    "$private_directory/project-after-omitted.json")" 200 project.one
if ! jq -e '[.environments[].libsql[]? | select(.name == "omitted-server-probe")] | length == 0' \
    "$private_directory/project-after-omitted.json" >/dev/null
then
    echo "a rejected libsql.create left a LibSQL record behind" >&2
    exit 1
fi
printf '%s\n' "$omitted_status" >"$workspace/libsql-omitted-server.status"
placed_before="$(group_ids placed)"
local_before="$(group_ids local)"
plain_before="$(group_ids plain)"
assert_no_contact create

# 3. A changed server replaces each service delete-before-create: non-local to
# non-local for the placed group, local to non-local for the local group.
write_config "$(named "$server_b_name")" "$(named "$server_a_name")" none none
cli plan --file "$config_file" --json >"$workspace/plan-replace.stdout" 2>"$workspace/plan-replace.stderr"
jq -e '[.changes[] | select(.kind == "replace" and .replacementOrder == "delete_before_create")] | length == 14' \
    "$workspace/plan-replace.stdout" >/dev/null
jq -e '[.changes[] | select(.kind != "replace")] | length == 0' \
    "$workspace/plan-replace.stdout" >/dev/null
run_apply replace
require_noop_plan after-replace
placed_after="$(group_ids placed)"
local_after="$(group_ids local)"
for kind in $kinds; do
    if [[ "$(id_of "$placed_before" "$kind")" == "$(id_of "$placed_after" "$kind")" ]] \
        || [[ "$(id_of "$local_before" "$kind")" == "$(id_of "$local_after" "$kind")" ]]
    then
        echo "a server placement change did not replace the physical $kind service" >&2
        exit 1
    fi
    if ! capture_absence "$kind" "$(id_of "$placed_before" "$kind")" \
        "$workspace/replaced.$kind.placed.status" \
        || ! capture_absence "$kind" "$(id_of "$local_before" "$kind")" \
            "$workspace/replaced.$kind.local.status"
    then
        echo "$kind.one did not prove absence of a replaced service" >&2
        exit 1
    fi
done
if [[ "$(group_ids plain)" != "$plain_before" ]]; then
    echo "an unmanaged placement was replaced" >&2
    exit 1
fi
assert_group_placement placed "$server_b_id" replaced
assert_group_placement local "$server_a_id" replaced
assert_group_placement plain null replaced
assert_no_contact replace

# 4. Non-local to local also replaces.
write_config "$(local_server)" "$(named "$server_a_name")" none none
run_apply to-local
require_noop_plan after-to-local
placed_local="$(group_ids placed)"
for kind in $kinds; do
    if [[ "$(id_of "$placed_after" "$kind")" == "$(id_of "$placed_local" "$kind")" ]]; then
        echo "a local placement change did not replace the physical $kind service" >&2
        exit 1
    fi
    if ! capture_absence "$kind" "$(id_of "$placed_after" "$kind")" \
        "$workspace/to-local.$kind.status"
    then
        echo "$kind.one did not prove absence after the move to the local host" >&2
        exit 1
    fi
done
assert_group_placement placed null to-local
assert_group_placement local "$server_a_id" to-local
assert_no_contact to-local

# 5. Unmatched and ambiguous names block planning and apply before any mutation.
write_config "$(named "$absent_name")" "$(named "$server_a_name")" none none
require_blocked_plan blocked-unmatched unmatched 7
write_config "$(named "$duplicate_name")" "$(named "$server_a_name")" none none
require_blocked_plan blocked-ambiguous ambiguous 7
write_config "$(local_server)" "$(named "$server_a_name")" none none
require_noop_plan after-blocked
assert_group_placement placed null blocked
assert_group_placement local "$server_a_id" blocked

# 6. A saved plan applies while its resolution is unchanged and is refused when the
# selected record is re-created (same name, new identity) before apply.
write_config "$(local_server)" "$(named "$server_a_name")" "$(named "$server_c_name")" none
cli plan --file "$config_file" --out "$workspace/saved-unchanged.json" \
    >"$workspace/saved-unchanged.plan.stdout" 2>"$workspace/saved-unchanged.plan.stderr"
for identity in "$server_a_id" "$server_b_id" "$server_c_id" "$server_d_id" "$duplicate_one_id" "$duplicate_two_id"; do
    if grep -q -F -- "$identity" "$workspace/saved-unchanged.json"; then
        echo "the saved plan envelope contains an external identity" >&2
        exit 1
    fi
done
for name in "$server_a_name" "$server_b_name" "$server_c_name" "$server_d_name" "$duplicate_name"; do
    if grep -q -F -- "$name" "$workspace/saved-unchanged.json"; then
        echo "the saved plan envelope contains an external name" >&2
        exit 1
    fi
done
jq -e '(.remoteReceipt | test("^[0-9a-f]{64}$")) and .plan.applyable == true
    and (.plan.changes | length) == 7' "$workspace/saved-unchanged.json" >/dev/null
cli apply "$workspace/saved-unchanged.json" --file "$config_file" --auto-approve --parallelism 1 \
    >"$workspace/saved-unchanged.apply.stdout" 2>"$workspace/saved-unchanged.apply.stderr"
require_noop_plan after-saved-unchanged
assert_group_placement late "$server_c_id" saved

write_config "$(local_server)" "$(named "$server_a_name")" "$(named "$server_c_name")" "$(named "$server_d_name")"
cli plan --file "$config_file" --out "$workspace/saved-stale.json" \
    >"$workspace/saved-stale.plan.stdout" 2>"$workspace/saved-stale.plan.stderr"
jq -e '.plan.applyable == true and (.plan.changes | length) == 7' \
    "$workspace/saved-stale.json" >/dev/null
# Remove and re-create the not yet used fourth server so its name selects a new identity.
remove_server "$server_d_id"
server_d_id="$(create_server "$server_d_name" d-recreated)"
if cli apply "$workspace/saved-stale.json" --file "$config_file" --auto-approve \
    >"$workspace/saved-stale.apply.stdout" 2>"$workspace/saved-stale.apply.stderr"
then
    echo "a saved plan was applied after its external resolution changed" >&2
    exit 1
fi
if ! grep -q "saved plan cannot be applied safely" "$workspace/saved-stale.apply.stderr"; then
    echo "the stale saved plan was not refused by the receipt check" >&2
    exit 1
fi
for kind in $kinds; do
    if jq -e --arg address "$kind.late2" '.resources | has($address)' "$state_file" >/dev/null; then
        echo "a refused saved plan created a service" >&2
        exit 1
    fi
done
run_apply after-refusal
require_noop_plan after-fresh-apply
assert_group_placement late2 "$server_d_id" fresh
assert_no_contact saved-plan

# 7. Protected import writes name selectors, converges, and fails closed on ambiguity.
import_target() {
    local kind="$1" group="$2" directory="$3"

    mkdir -p "$directory"
    chmod 700 "$directory"
    # Dokploy identities may begin with a dash, so the positionals follow `--`.
    cli import --as "$kind.adopted" --file "$directory/dokploy.yaml" \
        -- "$kind" "$(state_value "$state_file" "$kind.$group")" \
        >"$workspace/import.$kind.$group.stdout" 2>"$workspace/import.$kind.$group.stderr"
}

for kind in $kinds; do
    import_directory="$workspace/import-$kind"
    import_target "$kind" late "$import_directory"
    if ! jq -e --arg address "$kind.adopted" --arg server "$server_c_name" '
        .resources[$address].lastApplied.server == {name:$server}
    ' "$import_directory/.dokploy/state.json" >/dev/null; then
        echo "the imported $kind does not hold a name selector in durable state" >&2
        exit 1
    fi
    if ! grep -q -F -- "$server_c_name" "$import_directory/dokploy.yaml" \
        || ! grep -q 'protect: true' "$import_directory/dokploy.yaml"
    then
        echo "the imported $kind configuration is not a protected name selector" >&2
        exit 1
    fi
    for identity in "$server_c_id" "$server_a_id" "$server_b_id"; do
        if grep -q -F -- "$identity" "$import_directory/dokploy.yaml" \
            "$import_directory/.dokploy/state.json"; then
            echo "the imported $kind workspace contains an external identity" >&2
            exit 1
        fi
    done
    cli plan --file "$import_directory/dokploy.yaml" --json --detailed-exitcode \
        >"$workspace/after-import.$kind.stdout" 2>"$workspace/after-import.$kind.stderr"

    # A service on the local host stays unmanaged: nothing is invented.
    plain_directory="$workspace/import-plain-$kind"
    import_target "$kind" plain "$plain_directory"
    if grep -q 'server:' "$plain_directory/dokploy.yaml"; then
        echo "an unplaced $kind was imported with a server selector" >&2
        exit 1
    fi
    cli plan --file "$plain_directory/dokploy.yaml" --json --detailed-exitcode \
        >"$workspace/after-import-plain.$kind.stdout" 2>"$workspace/after-import-plain.$kind.stderr"
done

# A second record with the same name makes the attached server ambiguous.
server_c_duplicate_id="$(create_server "$server_c_name" c-duplicate)"
for kind in $kinds; do
    ambiguous_directory="$workspace/import-ambiguous-$kind"
    mkdir -p "$ambiguous_directory"
    if cli import --as "$kind.ambiguous" --file "$ambiguous_directory/dokploy.yaml" \
        -- "$kind" "$(state_value "$state_file" "$kind.late")" \
        >"$workspace/import-ambiguous.$kind.stdout" 2>"$workspace/import-ambiguous.$kind.stderr"
    then
        echo "an ambiguous $kind placement was imported" >&2
        exit 1
    fi
    if ! grep -q "shared by another record" "$workspace/import-ambiguous.$kind.stderr"; then
        echo "the ambiguous $kind import did not report the closed diagnostic" >&2
        exit 1
    fi
    if [[ -e "$ambiguous_directory/dokploy.yaml" || -e "$ambiguous_directory/.dokploy" ]]; then
        echo "a failed $kind import left files behind" >&2
        exit 1
    fi
done
remove_server "$server_c_duplicate_id"

# 8. The managed workspace still converges and nothing was deployed or contacted.
write_config "$(local_server)" "$(named "$server_a_name")" "$(named "$server_c_name")" "$(named "$server_d_name")"
require_noop_plan final
assert_group_placement placed null final
assert_group_placement local "$server_a_id" final
assert_group_placement plain null final
assert_group_placement late "$server_c_id" final
assert_group_placement late2 "$server_d_id" final
assert_no_contact final
jq -n --argjson tripwireHits "$(tripwire_hits)" '{tripwireHits:$tripwireHits,deployments:0}' \
    >"$workspace/contact-summary.json"
if ! jq -e '.tripwireHits == 0 and .deployments == 0' "$workspace/contact-summary.json" >/dev/null; then
    echo "the contact summary does not prove an inert run" >&2
    exit 1
fi

echo "Compose and database server placement (named, local, unmanaged), create-only replacement in every direction, unmatched and ambiguous blocking, saved-plan receipt binding, protected name-selector import, zero-deployment, and inert-contact checks passed for compose, postgres, mysql, mariadb, mongo, libsql, and redis."
