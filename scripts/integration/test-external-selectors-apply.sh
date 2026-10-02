#!/usr/bin/env bash

# Live acceptance for external selector resolution and saved-plan binding.
#
# Servers, registries, and backup destinations are external infrastructure. This
# script creates disposable, inert records, resolves them through the real
# selector seam and the declarative engine, and proves identity-scoped cleanup.
# The Dokploy instance is shared: serialize runs with .integration/live.lock.
#
# Inertness:
#   - servers and the backup destination point at a tripwire that counts every
#     connection; any SSH, connectivity-test, or destination contact fails the run;
#   - registry.create performs `docker login` before it stores a registry, so the
#     registry URL is a loopback /v2/ responder. The run asserts that it saw only
#     `GET /v2/` probes and that the probe count never changes after the records
#     exist, so no pull, push, or later login happens;
#   - every touched application must stay idle with zero deployments.

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
workspace="$(mktemp -d "$state_directory/phase8-external-selectors.XXXXXX")"
private_directory="$workspace/private"
import_directory="$workspace/import"
ambiguous_import_directory="$workspace/ambiguous-import"
run_id="$(date -u +%Y%m%d%H%M%S)-$(openssl rand -hex 5)"
prefix="phase8-selectors-$run_id"
project_name="$prefix"
config_file="$workspace/dokploy.yaml"
import_config_file="$import_directory/dokploy.yaml"
api_header_file="$private_directory/api-header"
secret_canary_file="$private_directory/secret-canaries"
tripwire_name="phase8-selectors-tripwire-${run_id//[^a-zA-Z0-9]/}"
registry_responder_name="phase8-selectors-registry-${run_id//[^a-zA-Z0-9]/}"
network_name="dokploy-iac-integration_default"
registry_port=""
project_id=""
mutation_attempted=false
cleanup_confirmed=false
registry_ping_baseline=0
fingerprint_key="0199a0c8-2351-7c31-8899-2c8f81983ea5:$(openssl rand -hex 32)"

server_a_id=""
server_b_id=""
build_server_id=""
registry_one_id=""
registry_two_id=""
registry_three_id=""
registry_four_id=""
duplicate_one_id=""
duplicate_two_id=""
destination_id=""

mkdir -p "$private_directory" "$import_directory" "$ambiguous_import_directory"
chmod 700 "$private_directory" "$import_directory" "$ambiguous_import_directory"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$api_header_file"
chmod 600 "$api_header_file"
printf '%s\n' "$fingerprint_key" >>"$secret_canary_file"

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

registry_requests() {
    docker logs "$registry_responder_name" 2>&1 | grep -c REGISTRY_REQUEST || true
}

assert_registry_responder_untouched() {
    local label="$1" count

    count="$(registry_requests)"
    if [[ "$count" != "$registry_ping_baseline" ]]; then
        echo "the registry responder saw traffic after record creation ($label)" >&2
        return 1
    fi
    if docker logs "$registry_responder_name" 2>&1 | grep REGISTRY_REQUEST \
        | grep -v -x 'REGISTRY_REQUEST GET /v2/' | grep -q .
    then
        echo "the registry responder saw more than login probes ($label)" >&2
        return 1
    fi
}

assert_no_contact() {
    local label="$1"

    if [[ "$(tripwire_hits)" != 0 ]]; then
        echo "an inert server or destination was contacted ($label)" >&2
        return 1
    fi
    assert_registry_responder_untouched "$label"
}

create_server() {
    local name="$1" type="$2" label="$3" response_status

    jq -n --arg name "$name" --arg host "$tripwire_name" --arg type "$type" '
        {
            name:$name,description:"Disposable external selector fixture",
            ipAddress:$host,port:9000,username:"root",sshKeyId:null,
            serverType:$type,enableDockerCleanup:false
        }
    ' >"$private_directory/server-create.$label.request.json"
    response_status="$(api_post server.create \
        "$private_directory/server-create.$label.request.json" \
        "$private_directory/server-create.$label.json")"
    require_status "$response_status" 200 "server.create $label"
    jq -er '.serverId' "$private_directory/server-create.$label.json"
}

create_registry() {
    local name="$1" label="$2" password response_status

    password="selector-registry-secret-$label-$run_id"
    printf '%s\n' "$password" >>"$secret_canary_file"
    jq -n --arg name "$name" --arg password "$password" \
        --arg url "localhost:$registry_port" '
        {
            registryName:$name,username:"selector-user",password:$password,
            registryUrl:$url,registryType:"cloud",imagePrefix:null
        }
    ' >"$private_directory/registry-create.$label.request.json"
    response_status="$(api_post registry.create \
        "$private_directory/registry-create.$label.request.json" \
        "$private_directory/registry-create.$label.json")"
    require_status "$response_status" 200 "registry.create $label"
    jq -er '.registryId' "$private_directory/registry-create.$label.json"
}

create_destination() {
    local name="$1" label="$2" access_key secret_key response_status

    access_key="selector-destination-access-$label-$run_id"
    secret_key="selector-destination-secret-$label-$run_id"
    printf '%s\n' "$access_key" "$secret_key" >>"$secret_canary_file"
    jq -n --arg name "$name" --arg endpoint "http://$tripwire_name:9000" \
        --arg access "$access_key" --arg secret "$secret_key" '
        {
            name:$name,provider:"Other",accessKey:$access,secretAccessKey:$secret,
            bucket:"selector-fixture",region:"us-east-1",endpoint:$endpoint,
            additionalFlags:[]
        }
    ' >"$private_directory/destination-create.$label.request.json"
    response_status="$(api_post destination.create \
        "$private_directory/destination-create.$label.request.json" \
        "$private_directory/destination-create.$label.json")"
    require_status "$response_status" 200 "destination.create $label"
    jq -er '.destinationId' "$private_directory/destination-create.$label.json"
}

remove_external() {
    local endpoint="$1" field="$2" identity="$3" response_status

    jq -n --arg field "$field" --arg id "$identity" '{($field):$id}' \
        >"$private_directory/remove.request.json"
    response_status="$(api_post "$endpoint" "$private_directory/remove.request.json" \
        "$private_directory/remove.response.json")"
    if [[ "$response_status" != 200 && "$response_status" != 404 ]]; then
        echo "$endpoint cleanup returned HTTP $response_status" >&2
        return 1
    fi
}

# Removes every disposable record whose name carries this run's prefix.
remove_scoped_records() {
    local status=0 identity

    if [[ "$(api_get server.all "$private_directory/cleanup-server-all.json")" == 200 ]]; then
        while IFS= read -r identity; do
            [[ -z "$identity" ]] || remove_external server.remove serverId "$identity" || status=1
        done < <(jq -r --arg prefix "$prefix" '.[] | select(.name | startswith($prefix)) | .serverId' \
            "$private_directory/cleanup-server-all.json")
    else
        status=1
    fi
    if [[ "$(api_get registry.all "$private_directory/cleanup-registry-all.json")" == 200 ]]; then
        while IFS= read -r identity; do
            [[ -z "$identity" ]] || remove_external registry.remove registryId "$identity" || status=1
        done < <(jq -r --arg prefix "$prefix" \
            '.[] | select(.registryName | startswith($prefix)) | .registryId' \
            "$private_directory/cleanup-registry-all.json")
    else
        status=1
    fi
    if [[ "$(api_get destination.all "$private_directory/cleanup-destination-all.json")" == 200 ]]; then
        while IFS= read -r identity; do
            [[ -z "$identity" ]] || remove_external destination.remove destinationId "$identity" || status=1
        done < <(jq -r --arg prefix "$prefix" '.[] | select(.name | startswith($prefix)) | .destinationId' \
            "$private_directory/cleanup-destination-all.json")
    else
        status=1
    fi

    return "$status"
}

prove_scoped_absence() {
    local status=0

    [[ "$(api_get server.all "$private_directory/absence-server-all.json")" == 200 ]] &&
        jq -e --arg prefix "$prefix" '[.[] | select(.name | startswith($prefix))] | length == 0' \
            "$private_directory/absence-server-all.json" >/dev/null || status=1
    [[ "$(api_get registry.all "$private_directory/absence-registry-all.json")" == 200 ]] &&
        jq -e --arg prefix "$prefix" '[.[] | select(.registryName | startswith($prefix))] | length == 0' \
            "$private_directory/absence-registry-all.json" >/dev/null || status=1
    [[ "$(api_get destination.all "$private_directory/absence-destination-all.json")" == 200 ]] &&
        jq -e --arg prefix "$prefix" '[.[] | select(.name | startswith($prefix))] | length == 0' \
            "$private_directory/absence-destination-all.json" >/dev/null || status=1

    return "$status"
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
        echo "External selector cleanup could not prove project absence" >&2
        cleanup_status=1
    fi

    if [[ "$mutation_attempted" == true ]]; then
        remove_scoped_records || cleanup_status=1
        prove_scoped_absence || {
            echo "External selector cleanup could not prove record absence" >&2
            cleanup_status=1
        }
    fi
    docker rm -f "$tripwire_name" "$registry_responder_name" >/dev/null 2>&1
    if docker inspect "$tripwire_name" >/dev/null 2>&1 \
        || docker inspect "$registry_responder_name" >/dev/null 2>&1
    then
        echo "External selector cleanup could not remove its local fixtures" >&2
        cleanup_status=1
    fi
    if [[ "$cleanup_status" -eq 0 ]]; then
        cleanup_confirmed=true
    fi

    # Retained evidence is scanned for the API key, fingerprint key, and every
    # secret canary before the private directory is discarded.
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
    for canary in "$server_a_id" "$server_b_id" "$build_server_id" "$registry_one_id" \
        "$registry_two_id" "$registry_three_id" "$registry_four_id" "$duplicate_one_id" \
        "$duplicate_two_id" "$destination_id"; do
        [[ -z "$canary" ]] && continue
        if grep -R -F -q --exclude-dir=private -- "$canary" "$workspace"; then
            scan_status=1
        fi
    done
    if [[ "$scan_status" -ne 0 ]]; then
        echo "secret or external identity material appeared in retained selector evidence" >&2
        cleanup_status=1
    fi

    if ! find "$private_directory" -depth -delete 2>/dev/null || [[ -e "$private_directory" ]]; then
        echo "External selector cleanup could not discard private evidence" >&2
        cleanup_status=1
    fi

    if [[ "$primary_status" -eq 0 && "$cleanup_status" -eq 0 ]]; then
        case "$workspace" in
            "$state_directory"/phase8-external-selectors.*)
                rm -rf -- "$workspace"
                ;;
            *)
                echo "refusing to remove an unexpected integration workspace" >&2
                cleanup_status=1
                ;;
        esac
    else
        echo "External selector integration evidence retained at $workspace" >&2
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

# Fixtures: a connection tripwire and a loopback registry /v2/ responder.
registry_port=$((35000 + RANDOM % 2000))
while (exec 3<>"/dev/tcp/127.0.0.1/$registry_port") 2>/dev/null; do
    registry_port=$((35000 + RANDOM % 2000))
done
mutation_attempted=true
docker run --detach --rm \
    --network "$network_name" \
    --name "$tripwire_name" \
    --entrypoint node \
    "$expected_image" \
    -e 'require("net").createServer(socket => { console.log("TRIPWIRE_HIT"); socket.destroy(); }).listen(9000, "0.0.0.0", () => console.log("TRIPWIRE_READY")); setInterval(() => {}, 60000);' \
    >"$private_directory/tripwire.container-id"
docker run --detach --rm \
    --name "$registry_responder_name" \
    --publish "127.0.0.1:$registry_port:9000" \
    --entrypoint node \
    "$expected_image" \
    -e 'require("http").createServer((request, response) => { console.log("REGISTRY_REQUEST " + request.method + " " + request.url); response.writeHead(200, {"content-type": "application/json", "docker-distribution-api-version": "registry/2.0"}); response.end("{}"); }).listen(9000, "0.0.0.0", () => console.log("REGISTRY_READY"));' \
    >"$private_directory/registry-responder.container-id"
for _ in {1..30}; do
    if docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_READY \
        && docker logs "$registry_responder_name" 2>&1 | grep -q REGISTRY_READY
    then
        break
    fi
    sleep 1
done
if ! docker logs "$tripwire_name" 2>&1 | grep -q TRIPWIRE_READY \
    || ! docker logs "$registry_responder_name" 2>&1 | grep -q REGISTRY_READY
then
    echo "the local selector fixtures did not become ready" >&2
    exit 1
fi

# 1. Create inert disposable external records.
server_a_name="$prefix-edge-a"
server_b_name="$prefix-edge-b"
build_server_name="$prefix-builder"
registry_one_name="$prefix-registry-1"
registry_two_name="$prefix-registry-2"
registry_three_name="$prefix-registry-3"
registry_four_name="$prefix-registry-4"
duplicate_name="$prefix-registry-dup"
destination_name="$prefix-destination"
server_a_id="$(create_server "$server_a_name" deploy a)"
server_b_id="$(create_server "$server_b_name" deploy b)"
build_server_id="$(create_server "$build_server_name" build builder)"
registry_one_id="$(create_registry "$registry_one_name" one)"
registry_two_id="$(create_registry "$registry_two_name" two)"
registry_three_id="$(create_registry "$registry_three_name" three)"
registry_four_id="$(create_registry "$registry_four_name" four)"
duplicate_one_id="$(create_registry "$duplicate_name" duplicate-one)"
duplicate_two_id="$(create_registry "$duplicate_name" duplicate-two)"
destination_id="$(create_destination "$destination_name" one)"
registry_ping_baseline="$(registry_requests)"
assert_no_contact record-creation

# 2. Resolve the local selector and NON-LOCAL server, registry, and destination
# records, plus ambiguity, through the real selector seam.
DOKPLOY_EXTERNAL_SELECTOR_RESOLUTION_LIVE_TEST=1 \
LIVE_SELECTOR_SERVER_NAME="$server_a_name" \
LIVE_SELECTOR_REGISTRY_NAME="$registry_one_name" \
LIVE_SELECTOR_DUPLICATE_REGISTRY_NAME="$duplicate_name" \
LIVE_SELECTOR_DESTINATION_NAME="$destination_name" \
    cargo ${cargo_config[@]+"${cargo_config[@]}"} test \
        --manifest-path "$repository_root/Cargo.toml" --locked \
        --package dokploy-cli --test live_external_selectors -- \
        --ignored --exact live_selectors_resolve_exact_unique_names_and_reject_ambiguity \
    >"$workspace/selector-seam.stdout" 2>"$workspace/selector-seam.stderr"
assert_no_contact selector-seam

capture_application() {
    local application_id="$1" destination="$2" server="$3" build_server="$4"
    local registry="$5" build_registry="$6" rollback_registry="$7"
    local raw status

    raw="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "application.one?applicationId=$(urlencode "$application_id")" "$raw")"
    if [[ "$status" != 200 ]]; then
        rm -f -- "$raw"
        echo "application.one returned HTTP $status; expected 200" >&2
        return 1
    fi
    if ! jq -e \
        --arg server "$server" --arg build_server "$build_server" \
        --arg registry "$registry" --arg build_registry "$build_registry" \
        --arg rollback_registry "$rollback_registry" '
        def matches($actual; $expected):
            if $expected == "null" then $actual == null else $actual == $expected end;
        .applicationStatus == "idle"
        and (.deployments | length) == 0
        and matches(.serverId; $server)
        and matches(.buildServerId; $build_server)
        and matches(.registryId; $registry)
        and matches(.buildRegistryId; $build_registry)
        and matches(.rollbackRegistryId; $rollback_registry)
    ' "$raw" >/dev/null; then
        rm -f -- "$raw"
        echo "application.one did not prove the expected external associations" >&2
        return 1
    fi
    jq '{
        applicationStatus,
        deploymentCount: (.deployments | length),
        serverAssociated: (.serverId != null),
        buildServerAssociated: (.buildServerId != null),
        registryAssociated: (.registryId != null),
        buildRegistryAssociated: (.buildRegistryId != null),
        rollbackRegistryAssociated: (.rollbackRegistryId != null)
    }' "$raw" >"$destination"
    rm -f -- "$raw"
}

capture_application_absence() {
    local application_id="$1" destination="$2" raw status

    raw="$(mktemp "$private_directory/response.XXXXXX")"
    status="$(api_get "application.one?applicationId=$(urlencode "$application_id")" "$raw")"
    rm -f -- "$raw"
    printf '%s\n' "$status" >"$destination"
    [[ "$status" != 200 ]]
}

write_config() {
    local server="$1" build_server="$2" registry="$3" build_registry="$4" rollback="$5"

    cat >"$config_file" <<EOF
version: 1
project:
  name: $project_name
  description: Disposable Phase 8 external selector validation
  environments:
    production:
      description: Managed by the Phase 8 external selector integration check
      applications:
        api:
$server
$build_server
$registry
$build_registry
$rollback
EOF
    chmod 600 "$config_file"
}

named() {
    printf '          %s:\n            name: %s' "$1" "$2"
}

local_server() {
    printf '          server:\n            local: true'
}

null_field() {
    printf '          %s: null' "$1"
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

# Plans that must be blocked exit with failure and name the typed diagnostic.
require_blocked_plan() {
    local label="$1" failure="$2" status=0

    cli plan --file "$config_file" --json --detailed-exitcode \
        >"$workspace/$label.stdout" 2>"$workspace/$label.stderr" || status=$?
    if [[ "$status" -ne 1 ]]; then
        echo "an unresolved selector did not block the plan ($label)" >&2
        return 1
    fi
    jq -e --arg failure "$failure" '
        .applyable == false
        and ([.diagnostics[] | select(.code == "DOKPLAN019" and .selectorFailure == $failure)] | length) == 1
        and (.changes | length) == 0
    ' "$workspace/$label.stdout" >/dev/null
    if grep -q -F -e "$server_a_id" -e "$registry_one_id" -e "$duplicate_one_id" \
        "$workspace/$label.stdout"; then
        echo "a blocked plan exposed an external identity ($label)" >&2
        return 1
    fi
    if cli apply --file "$config_file" --auto-approve \
        >"$workspace/$label.apply.stdout" 2>"$workspace/$label.apply.stderr"; then
        echo "apply accepted a blocked plan ($label)" >&2
        return 1
    fi
}

# 3. Create an application whose associations are resolved by name.
write_config \
    "$(named server "$server_a_name")" \
    "$(named build_server "$build_server_name")" \
    "$(named registry "$registry_one_name")" \
    "" \
    "$(named rollback_registry "$registry_two_name")"
run_apply create
require_noop_plan after-create
state_file="$workspace/.dokploy/state.json"
project_id="$(state_value "$state_file" "project.$project_name")"
environment_id="$(state_value "$state_file" environment.production)"
application_id="$(state_value "$state_file" application.api)"
capture_application "$application_id" "$workspace/application.created.json" \
    "$server_a_id" "$build_server_id" "$registry_one_id" null "$registry_two_id"
if ! jq -e --arg server "$server_a_name" '
    .resources["application.api"].lastApplied.server == {name:$server}
' "$state_file" >/dev/null; then
    echo "durable state does not hold the server as a name selector" >&2
    exit 1
fi
assert_no_contact create

# 4. Registry associations change in place and keep the physical application.
write_config \
    "$(named server "$server_a_name")" \
    "$(named build_server "$build_server_name")" \
    "$(named registry "$registry_two_name")" \
    "$(named build_registry "$registry_one_name")" \
    "$(null_field rollback_registry)"
cli plan --file "$config_file" --json >"$workspace/plan-inplace.stdout" 2>"$workspace/plan-inplace.stderr"
jq -e '[.changes[] | select(.kind == "update")] | length == 1' "$workspace/plan-inplace.stdout" >/dev/null
run_apply inplace
require_noop_plan after-inplace
if [[ "$(state_value "$state_file" application.api)" != "$application_id" ]]; then
    echo "an in-place association change replaced the physical application" >&2
    exit 1
fi
capture_application "$application_id" "$workspace/application.inplace.json" \
    "$server_a_id" "$build_server_id" "$registry_two_id" "$registry_one_id" null
assert_no_contact inplace

# 5. Unmatched and ambiguous selectors block the plan and apply.
write_config \
    "$(named server "$server_a_name")" \
    "$(named build_server "$build_server_name")" \
    "$(named registry "$prefix-registry-absent")" \
    "$(named build_registry "$registry_one_name")" \
    "$(null_field rollback_registry)"
require_blocked_plan blocked-unmatched unmatched
write_config \
    "$(named server "$server_a_name")" \
    "$(named build_server "$build_server_name")" \
    "$(named registry "$duplicate_name")" \
    "$(named build_registry "$registry_one_name")" \
    "$(null_field rollback_registry)"
require_blocked_plan blocked-ambiguous ambiguous
capture_application "$application_id" "$workspace/application.blocked.json" \
    "$server_a_id" "$build_server_id" "$registry_two_id" "$registry_one_id" null

# 6. A saved plan applies while its resolution is unchanged and is refused when
# the selected record is re-created (same name, new identity) before apply.
write_config \
    "$(named server "$server_a_name")" \
    "$(named build_server "$build_server_name")" \
    "$(named registry "$registry_two_name")" \
    "$(named build_registry "$registry_one_name")" \
    "$(named rollback_registry "$registry_three_name")"
cli plan --file "$config_file" --out "$workspace/saved-unchanged.json" \
    >"$workspace/saved-unchanged.plan.stdout" 2>"$workspace/saved-unchanged.plan.stderr"
for identity in "$server_a_id" "$build_server_id" "$registry_one_id" "$registry_two_id" \
    "$registry_three_id" "$duplicate_one_id"; do
    if grep -q -F -- "$identity" "$workspace/saved-unchanged.json"; then
        echo "the saved plan envelope contains an external identity" >&2
        exit 1
    fi
done
for name in "$registry_three_name" "$registry_two_name" "$registry_one_name" "$server_a_name"; do
    if grep -q -F -- "$name" "$workspace/saved-unchanged.json"; then
        echo "the saved plan envelope contains an external name" >&2
        exit 1
    fi
done
jq -e '(.remoteReceipt | test("^[0-9a-f]{64}$")) and .plan.applyable == true' \
    "$workspace/saved-unchanged.json" >/dev/null
cli apply "$workspace/saved-unchanged.json" --file "$config_file" --auto-approve \
    >"$workspace/saved-unchanged.apply.stdout" 2>"$workspace/saved-unchanged.apply.stderr"
require_noop_plan after-saved-unchanged
capture_application "$application_id" "$workspace/application.saved.json" \
    "$server_a_id" "$build_server_id" "$registry_two_id" "$registry_one_id" "$registry_three_id"

write_config \
    "$(named server "$server_a_name")" \
    "$(named build_server "$build_server_name")" \
    "$(named registry "$registry_two_name")" \
    "$(named build_registry "$registry_one_name")" \
    "$(named rollback_registry "$registry_four_name")"
cli plan --file "$config_file" --out "$workspace/saved-stale.json" \
    >"$workspace/saved-stale.plan.stdout" 2>"$workspace/saved-stale.plan.stderr"
jq -e '.plan.applyable == true and (.plan.changes | length) == 1' \
    "$workspace/saved-stale.json" >/dev/null
# Remove and re-create the unattached fourth registry so its name selects a new identity.
remove_external registry.remove registryId "$registry_four_id"
registry_four_id="$(create_registry "$registry_four_name" four-recreated)"
registry_ping_baseline="$(registry_requests)"
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
capture_application "$application_id" "$workspace/application.after-refusal.json" \
    "$server_a_id" "$build_server_id" "$registry_two_id" "$registry_one_id" "$registry_three_id"
run_apply after-refusal
require_noop_plan after-fresh-apply
capture_application "$application_id" "$workspace/application.after-fresh-apply.json" \
    "$server_a_id" "$build_server_id" "$registry_two_id" "$registry_one_id" "$registry_four_id"
assert_no_contact saved-plan

# 7. The create-only server placement replaces the application, delete before create.
write_config \
    "$(named server "$server_b_name")" \
    "$(named build_server "$build_server_name")" \
    "$(named registry "$registry_two_name")" \
    "$(named build_registry "$registry_one_name")" \
    "$(null_field rollback_registry)"
cli plan --file "$config_file" --json >"$workspace/plan-replace.stdout" 2>"$workspace/plan-replace.stderr"
jq -e '[.changes[] | select(.kind == "replace" and .replacementOrder == "delete_before_create")] | length == 1' \
    "$workspace/plan-replace.stdout" >/dev/null
run_apply replace
require_noop_plan after-replace
replacement_id="$(state_value "$state_file" application.api)"
if [[ "$replacement_id" == "$application_id" ]]; then
    echo "a server placement change did not replace the physical application" >&2
    exit 1
fi
if ! capture_application_absence "$application_id" "$workspace/application.replaced.status"; then
    echo "application.one did not prove absence of the replaced application" >&2
    exit 1
fi
capture_application "$replacement_id" "$workspace/application.replacement.json" \
    "$server_b_id" "$build_server_id" "$registry_two_id" "$registry_one_id" null
application_id="$replacement_id"
assert_no_contact replace

# 8. The explicit local selector replaces the application again, placing it locally.
write_config \
    "$(local_server)" \
    "$(named build_server "$build_server_name")" \
    "$(named registry "$registry_two_name")" \
    "$(named build_registry "$registry_one_name")" \
    "$(null_field rollback_registry)"
run_apply local
require_noop_plan after-local
local_id="$(state_value "$state_file" application.api)"
if [[ "$local_id" == "$application_id" ]]; then
    echo "a local placement change did not replace the physical application" >&2
    exit 1
fi
capture_application "$local_id" "$workspace/application.local.json" \
    null "$build_server_id" "$registry_two_id" "$registry_one_id" null
application_id="$local_id"
assert_no_contact local

# 9. Import writes name selectors and converges; an ambiguous name fails closed.
jq -n --arg name "$prefix-adopted" --arg environmentId "$environment_id" --arg serverId "$server_a_id" '
    {name:$name,environmentId:$environmentId,serverId:$serverId}
' >"$private_directory/adopted-create.request.json"
require_status "$(api_post application.create "$private_directory/adopted-create.request.json" \
    "$private_directory/adopted-create.json")" 200 "application.create adopted"
adopted_id="$(jq -er '.applicationId' "$private_directory/adopted-create.json")"
jq -n --arg id "$adopted_id" --arg registry "$registry_one_id" --arg rollback "$registry_two_id" '
    {applicationId:$id,registryId:$registry,rollbackRegistryId:$rollback}
' >"$private_directory/adopted-update.request.json"
require_status "$(api_post application.update "$private_directory/adopted-update.request.json" \
    "$private_directory/adopted-update.json")" 200 "application.update adopted"
# Import adopts the whole project, so the managed application is imported with
# the adopted one. The adopted application is found by its remote identity
# because its address is derived from its name.
cli import project "$project_id" --file "$import_config_file" \
    >"$workspace/import.stdout" 2>"$workspace/import.stderr"
require_noop_plan after-import "$import_config_file"
if ! jq -e --arg id "$adopted_id" --arg server "$server_a_name" \
    --arg registry "$registry_one_name" --arg rollback "$registry_two_name" '
    [.resources[] | select(.kind == "application" and .remoteId == $id) | .lastApplied] as $adopted
    | ($adopted | length) == 1
    and $adopted[0].server == {name:$server}
    and $adopted[0].registry == {name:$registry}
    and $adopted[0].rollback_registry == {name:$rollback}
' "$import_directory/.dokploy/state.json" >/dev/null; then
    echo "the imported application does not hold name selectors in durable state" >&2
    exit 1
fi
if ! grep -q -F -- "$server_a_name" "$import_config_file" \
    || ! grep -q -F -- "$registry_one_name" "$import_config_file"
then
    echo "the imported configuration does not select associations by name" >&2
    exit 1
fi
for identity in "$server_a_id" "$registry_one_id" "$registry_two_id"; do
    if grep -q -F -- "$identity" "$import_config_file" \
        "$import_directory/.dokploy/state.json"; then
        echo "the imported workspace contains an external identity" >&2
        exit 1
    fi
done
capture_application "$adopted_id" "$workspace/application.adopted.json" \
    "$server_a_id" null "$registry_one_id" null "$registry_two_id"

jq -n --arg name "$prefix-ambiguous" --arg environmentId "$environment_id" '
    {name:$name,environmentId:$environmentId}
' >"$private_directory/ambiguous-create.request.json"
require_status "$(api_post application.create "$private_directory/ambiguous-create.request.json" \
    "$private_directory/ambiguous-create.json")" 200 "application.create ambiguous"
ambiguous_id="$(jq -er '.applicationId' "$private_directory/ambiguous-create.json")"
jq -n --arg id "$ambiguous_id" --arg registry "$duplicate_one_id" \
    '{applicationId:$id,registryId:$registry}' \
    >"$private_directory/ambiguous-update.request.json"
require_status "$(api_post application.update "$private_directory/ambiguous-update.request.json" \
    "$private_directory/ambiguous-update.json")" 200 "application.update ambiguous"
# The project now holds an application attached to a shared registry name, so
# the whole project import fails closed.
if cli import project "$project_id" \
    --file "$ambiguous_import_directory/dokploy.yaml" \
    >"$workspace/import-ambiguous.stdout" 2>"$workspace/import-ambiguous.stderr"
then
    echo "an ambiguous association was imported" >&2
    exit 1
fi
if ! grep -q "shared by another record" "$workspace/import-ambiguous.stderr"; then
    echo "the ambiguous import did not report the closed diagnostic" >&2
    exit 1
fi
if [[ -e "$ambiguous_import_directory/dokploy.yaml" || -e "$ambiguous_import_directory/.dokploy" ]]; then
    echo "a failed import left files behind" >&2
    exit 1
fi
capture_application "$ambiguous_id" "$workspace/application.ambiguous.json" \
    null null "$duplicate_one_id" null null

# 10. Zero deployments, zero contact, and a stable registry probe count.
assert_no_contact final
jq -n --argjson tripwireHits "$(tripwire_hits)" \
    --argjson registryProbes "$(registry_requests)" '
    {tripwireHits:$tripwireHits,registryLoginProbes:$registryProbes,deployments:0}
' >"$workspace/contact-summary.json"
if ! jq -e '.tripwireHits == 0 and .deployments == 0' "$workspace/contact-summary.json" >/dev/null; then
    echo "the contact summary does not prove an inert run" >&2
    exit 1
fi

echo "External selector resolution (local, server, registry, destination), ambiguity and absence blocking, in-place association updates, create-only server replacement, saved-plan receipt binding, name-selector import, zero-deployment, and inert-contact checks passed."
