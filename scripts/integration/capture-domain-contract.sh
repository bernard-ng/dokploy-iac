#!/usr/bin/env bash

set -euo pipefail

if [[ "$-" == *x* ]]; then
    set +x
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
fixture_directory="$repository_root/fixtures/api/live/$dokploy_version"
sanitizer="$script_directory/sanitize-fixture.jq"

if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before capturing the Domain contract." >&2
    exit 1
fi

umask 077
mkdir -p "$state_directory"
chmod 700 "$state_directory"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/domain-contract-$run_id"
mkdir "$workspace"
chmod 700 "$workspace"

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

application_id=""
domain_id=""
created_host=""
updated_host=""
dokploy_container_id=""
mutation_attempted=false
cleanup_confirmed=false
capture_succeeded=false
publication_started=false
publication_complete=false
published_fixture_backup="$workspace/published-fixtures.backup"

urlencode() {
    jq -nr --arg value "$1" '$value | @uri'
}

api_request() {
    local method="$1"
    local endpoint="$2"
    local destination="$3"
    local body_file="${4:-}"
    local curl_arguments=(
        --silent
        --show-error
        --output "$destination"
        --write-out '%{http_code}'
        --header "@$auth_header_file"
        --request "$method"
    )

    if [[ -n "$body_file" ]]; then
        curl_arguments+=(
            --header 'Content-Type: application/json'
            --data-binary "@$body_file"
        )
    fi

    curl "${curl_arguments[@]}" "$base_url/api/$endpoint"
}

record_status() {
    local status="$1"
    local destination="$2"

    printf '%s\n' "$status" >"$destination"
    chmod 600 "$destination"
}

require_status() {
    local actual="$1"
    local expected="$2"
    local operation="$3"

    if [[ "$actual" != "$expected" ]]; then
        echo "$operation returned HTTP $actual; expected HTTP $expected." >&2
        return 1
    fi
}

capture_traefik_manifest() {
    local destination="$1"

    docker exec "$dokploy_container_id" sh -c \
        'find /etc/dokploy/traefik -type f -exec sha256sum {} \; | LC_ALL=C sort' \
        >"$destination"
    chmod 600 "$destination"
}

lookup_owned_domains() {
    local destination="$1"
    local status

    if [[ -z "$application_id" ]]; then
        return 1
    fi

    status="$(api_request \
        GET \
        "domain.byApplicationId?applicationId=$(urlencode "$application_id")" \
        "$destination")" || status=""
    record_status "$status" "$destination.status"

    [[ "$status" == "200" ]]
}

recover_domain_id() {
    local recovery_attempt
    local match_count
    local recovered_id

    if [[ -n "$domain_id" || -z "$created_host" || -z "$updated_host" ]]; then
        return
    fi

    for recovery_attempt in 1 2 3 4 5; do
        if lookup_owned_domains "$workspace/domain-by-application.recovery.json"; then
            match_count="$(jq \
                --arg createdHost "$created_host" \
                --arg updatedHost "$updated_host" \
                '[.[] | select(.host == $createdHost or .host == $updatedHost)] | length' \
                "$workspace/domain-by-application.recovery.json")" || match_count=""

            if [[ "$match_count" == "1" ]]; then
                recovered_id="$(jq -er \
                    --arg createdHost "$created_host" \
                    --arg updatedHost "$updated_host" \
                    '.[] | select(.host == $createdHost or .host == $updatedHost) | .domainId' \
                    "$workspace/domain-by-application.recovery.json")" || recovered_id=""

                if [[ -n "$recovered_id" ]]; then
                    domain_id="$recovered_id"
                    return
                fi
            elif [[ -n "$match_count" && "$match_count" != "0" ]]; then
                echo "Cleanup found multiple domains using the owned hosts; refusing ambiguous deletion." >&2
                return
            fi
        fi

        if [[ "$recovery_attempt" != "5" ]]; then
            sleep 1
        fi
    done
}

discard_private_workspace() {
    case "$workspace" in
        "$state_directory"/domain-contract-*) ;;
        *)
            echo "Refusing to delete an unexpected capture workspace." >&2
            return 1
            ;;
    esac

    find "$workspace" -depth -delete
}

cleanup() {
    local exit_code="$?"
    local lookup_status
    local delete_status
    local remaining_count
    local application_cleanup_status
    local owned_domain=false
    local application_clean=false
    local traefik_clean=false
    local restore_ready=true
    local delete_body="$workspace/domain-delete.cleanup.request.json"

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
        recover_domain_id

        if [[ -n "$domain_id" ]]; then
            lookup_status="$(api_request \
                GET \
                "domain.one?domainId=$(urlencode "$domain_id")" \
                "$workspace/domain-one.cleanup-before.json")"
            record_status "$lookup_status" "$workspace/domain-one.cleanup-before.status"

            if [[ "$lookup_status" == "200" ]]; then
                if jq -e \
                    --arg applicationId "$application_id" \
                    --arg createdHost "$created_host" \
                    --arg updatedHost "$updated_host" \
                    '.applicationId == $applicationId
                        and (.host == $createdHost or .host == $updatedHost)' \
                    "$workspace/domain-one.cleanup-before.json" \
                    >/dev/null
                then
                    owned_domain=true
                fi

                if [[ "$owned_domain" == true ]]; then
                    jq -n --arg domainId "$domain_id" '{domainId:$domainId}' >"$delete_body"
                    delete_status="$(api_request \
                        POST \
                        "domain.delete" \
                        "$workspace/domain-delete.cleanup.json" \
                        "$delete_body")"
                    record_status "$delete_status" "$workspace/domain-delete.cleanup.status"

                    if [[ "$delete_status" != "200" ]]; then
                        echo "Domain cleanup returned HTTP $delete_status; inspect the private capture workspace." >&2
                    fi
                else
                    echo "Domain cleanup lookup did not match exactly one owned host and application; refusing deletion." >&2
                fi
            elif [[ "$lookup_status" != "404" ]]; then
                echo "Domain cleanup lookup returned HTTP $lookup_status; inspect the private capture workspace." >&2
            fi

            lookup_status="$(api_request \
                GET \
                "domain.one?domainId=$(urlencode "$domain_id")" \
                "$workspace/domain-one.cleanup-after.json")"
            record_status "$lookup_status" "$workspace/domain-one.cleanup-after.status"

            if [[ "$lookup_status" == "404" ]] \
                && lookup_owned_domains "$workspace/domain-by-application.cleanup-after.json"
            then
                remaining_count="$(jq \
                    --arg domainId "$domain_id" \
                    --arg createdHost "$created_host" \
                    --arg updatedHost "$updated_host" \
                    '[.[] | select(
                        .domainId == $domainId
                        or .host == $createdHost
                        or .host == $updatedHost
                    )] | length' \
                    "$workspace/domain-by-application.cleanup-after.json")" || remaining_count=""

                if [[ "$remaining_count" == "0" ]]; then
                    application_cleanup_status="$(api_request \
                        GET \
                        "application.one?applicationId=$(urlencode "$application_id")" \
                        "$workspace/application-one.cleanup-after.json")"
                    record_status \
                        "$application_cleanup_status" \
                        "$workspace/application-one.cleanup-after.status"

                    if [[ "$application_cleanup_status" == "200" ]] \
                        && jq -e \
                            --arg domainId "$domain_id" \
                            --arg createdHost "$created_host" \
                            --arg updatedHost "$updated_host" \
                            '.applicationStatus == "idle"
                                and .serverId == null
                                and .server == null
                                and ([.domains[] | select(
                                    .domainId == $domainId
                                    or .host == $createdHost
                                    or .host == $updatedHost
                                )] | length == 0)' \
                            "$workspace/application-one.cleanup-after.json" \
                            >/dev/null
                    then
                        application_clean=true
                    fi

                    if capture_traefik_manifest "$workspace/traefik.cleanup-after.manifest" \
                        && cmp -s \
                            "$workspace/traefik.before.manifest" \
                            "$workspace/traefik.cleanup-after.manifest" \
                        && ! docker exec "$dokploy_container_id" \
                            grep -R -F -q -- "$created_host" /etc/dokploy/traefik \
                        && ! docker exec "$dokploy_container_id" \
                            grep -R -F -q -- "$updated_host" /etc/dokploy/traefik
                    then
                        traefik_clean=true
                    fi

                    if [[ "$application_clean" == true && "$traefik_clean" == true ]]; then
                        cleanup_confirmed=true
                    fi
                fi
            fi
        else
            echo "Domain cleanup could not recover an unambiguous owned record; inspect the private capture workspace." >&2
        fi
    fi

    if [[ "$mutation_attempted" == true && "$cleanup_confirmed" == false ]]; then
        exit_code=1
    fi

    if [[ "$capture_succeeded" == true && "$exit_code" == "0" ]]; then
        if ! discard_private_workspace; then
            echo "Could not discard the verified private capture workspace." >&2
            exit_code=1
        fi
    else
        chmod -R go-rwx "$workspace"
    fi

    exit "$exit_code"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

version_status="$(api_request GET "settings.getDokployVersion" "$workspace/version.json")"
record_status "$version_status" "$workspace/version.status"
require_status "$version_status" "200" "settings.getDokployVersion"

runtime_version="$(jq -er '.' "$workspace/version.json")"

if [[ "$runtime_version" != "$dokploy_version" ]]; then
    echo "Expected Dokploy $dokploy_version, received $runtime_version." >&2
    exit 1
fi

expected_runtime_image="$dokploy_image"
dokploy_container_id="$(compose ps --quiet dokploy)"

if [[ -z "$dokploy_container_id" ]]; then
    echo "The local Dokploy integration container is not running." >&2
    exit 1
fi

runtime_image="$(docker inspect --format '{{.Config.Image}}' "$dokploy_container_id")"

if [[ "$runtime_image" != "$expected_runtime_image" ]]; then
    echo "The running Dokploy container does not use the pinned integration image." >&2
    exit 1
fi

projects_status="$(api_request GET "project.all" "$workspace/project-all.json")"
record_status "$projects_status" "$workspace/project-all.status"
require_status "$projects_status" "200" "project.all"

project_count="$(jq '[.[] | select(.name == "IaC Contract Test")] | length' "$workspace/project-all.json")"

if [[ "$project_count" != "1" ]]; then
    echo "Domain contract capture requires exactly one IaC Contract Test project." >&2
    exit 1
fi

project_id="$(jq -er '.[] | select(.name == "IaC Contract Test") | .projectId' "$workspace/project-all.json")"
environment_count="$(jq \
    '[.[] | select(.name == "IaC Contract Test") | .environments[]? | select(.name == "production")] | length' \
    "$workspace/project-all.json")"

if [[ "$environment_count" != "1" ]]; then
    echo "Domain contract capture requires exactly one production environment." >&2
    exit 1
fi

project_one_status="$(api_request \
    GET \
    "project.one?projectId=$(urlencode "$project_id")" \
    "$workspace/project-one.preflight.json")"
record_status "$project_one_status" "$workspace/project-one.preflight.status"
require_status "$project_one_status" "200" "project.one preflight"

application_count="$(jq \
    '[.environments[]? | select(.name == "production") | .applications[]?] | length' \
    "$workspace/project-one.preflight.json")"

if [[ "$application_count" != "1" ]]; then
    echo "Domain contract capture requires exactly one existing fixture application." >&2
    exit 1
fi

application_id="$(jq -er \
    '.environments[] | select(.name == "production") | .applications[0].applicationId' \
    "$workspace/project-one.preflight.json")"

application_id_encoded="$(urlencode "$application_id")"
application_status="$(api_request \
    GET \
    "application.one?applicationId=$application_id_encoded" \
    "$workspace/application-one.preflight.json")"
record_status "$application_status" "$workspace/application-one.preflight.status"
require_status "$application_status" "200" "application.one preflight"

if ! jq -e \
    --arg applicationId "$application_id" \
    '.applicationId == $applicationId
        and .applicationStatus == "idle"
        and .serverId == null
        and .server == null
        and .domains == []' \
    "$workspace/application-one.preflight.json" \
    >/dev/null
then
    echo "The fixture application must be idle, serverless, and domain-free." >&2
    exit 1
fi

preflight_domain_status="$(api_request \
    GET \
    "domain.byApplicationId?applicationId=$application_id_encoded" \
    "$workspace/domain-by-application.preflight.json")"
record_status "$preflight_domain_status" "$workspace/domain-by-application.preflight.status"
require_status "$preflight_domain_status" "200" "domain.byApplicationId preflight"

if ! jq -e '. == []' "$workspace/domain-by-application.preflight.json" >/dev/null; then
    echo "Domain contract capture requires an application with no domains." >&2
    exit 1
fi

capture_traefik_manifest "$workspace/traefik.before.manifest"

resource_suffix="$(date -u +%s)-$(openssl rand -hex 6)"
created_host="iac-domain-contract-created-$resource_suffix.integration.test"
updated_host="iac-domain-contract-updated-$resource_suffix.integration.test"
create_body="$workspace/domain-create.request.json"

jq -n \
    --arg host "$created_host" \
    --arg applicationId "$application_id" \
    '{host:$host, applicationId:$applicationId, domainType:"application"}' \
    >"$create_body"

mutation_attempted=true
create_status="$(api_request POST "domain.create" "$workspace/domain-create.json" "$create_body")"
record_status "$create_status" "$workspace/domain-create.status"
require_status "$create_status" "200" "domain.create"

domain_id="$(jq -er '.domainId' "$workspace/domain-create.json")" || domain_id=""

created_collection_status="$(api_request \
    GET \
    "domain.byApplicationId?applicationId=$application_id_encoded" \
    "$workspace/domain-by-application.created.json")"
record_status "$created_collection_status" "$workspace/domain-by-application.created.status"
require_status "$created_collection_status" "200" "domain.byApplicationId after create"

if ! jq -e \
    --arg host "$created_host" \
    --arg applicationId "$application_id" \
    '. | length == 1
        and .[0].host == $host
        and .[0].applicationId == $applicationId
        and .[0].domainType == "application"' \
    "$workspace/domain-by-application.created.json" \
    >/dev/null
then
    echo "The created Domain collection did not contain exactly the owned application domain." >&2
    exit 1
fi

collection_domain_id="$(jq -er '.[0].domainId' "$workspace/domain-by-application.created.json")"

if [[ -n "$domain_id" && "$domain_id" != "$collection_domain_id" ]]; then
    echo "domain.create and domain.byApplicationId returned different Domain identifiers." >&2
    exit 1
fi

domain_id="$collection_domain_id"
domain_id_encoded="$(urlencode "$domain_id")"

created_one_status="$(api_request \
    GET \
    "domain.one?domainId=$domain_id_encoded" \
    "$workspace/domain-one.created.json")"
record_status "$created_one_status" "$workspace/domain-one.created.status"
require_status "$created_one_status" "200" "domain.one after create"

if ! jq -e \
    --arg domainId "$domain_id" \
    --arg host "$created_host" \
    --arg applicationId "$application_id" \
    '.domainId == $domainId
        and .host == $host
        and .applicationId == $applicationId
        and .domainType == "application"
        and .port == 3000
        and .https == false
        and .certificateType == "none"' \
    "$workspace/domain-one.created.json" \
    >/dev/null
then
    echo "The created Domain did not expose the expected default port and TLS settings." >&2
    exit 1
fi

created_application_status="$(api_request \
    GET \
    "application.one?applicationId=$application_id_encoded" \
    "$workspace/application-one.domain-created.json")"
record_status "$created_application_status" "$workspace/application-one.domain-created.status"
require_status "$created_application_status" "200" "application.one after Domain create"

if ! jq -e \
    --arg domainId "$domain_id" \
    --arg host "$created_host" \
    '.applicationStatus == "idle"
        and .serverId == null
        and .server == null
        and ([.domains[] | select(.domainId == $domainId and .host == $host)] | length == 1)' \
    "$workspace/application-one.domain-created.json" \
    >/dev/null
then
    echo "Domain creation changed the application deployment state or was not embedded." >&2
    exit 1
fi

capture_traefik_manifest "$workspace/traefik.created.manifest"

update_body="$workspace/domain-update.request.json"
jq -n \
    --arg domainId "$domain_id" \
    --arg host "$updated_host" \
    '{domainId:$domainId, host:$host, port:8080, https:true, certificateType:"none"}' \
    >"$update_body"

update_status="$(api_request POST "domain.update" "$workspace/domain-update.json" "$update_body")"
record_status "$update_status" "$workspace/domain-update.status"
require_status "$update_status" "200" "domain.update"

updated_one_status="$(api_request \
    GET \
    "domain.one?domainId=$domain_id_encoded" \
    "$workspace/domain-one.updated.json")"
record_status "$updated_one_status" "$workspace/domain-one.updated.status"
require_status "$updated_one_status" "200" "domain.one after update"

if ! jq -e \
    --arg domainId "$domain_id" \
    --arg host "$updated_host" \
    --arg applicationId "$application_id" \
    '.domainId == $domainId
        and .host == $host
        and .applicationId == $applicationId
        and .port == 8080
        and .https == true
        and .certificateType == "none"' \
    "$workspace/domain-one.updated.json" \
    >/dev/null
then
    echo "The updated Domain did not expose the requested host, port, and TLS settings." >&2
    exit 1
fi

updated_collection_status="$(api_request \
    GET \
    "domain.byApplicationId?applicationId=$application_id_encoded" \
    "$workspace/domain-by-application.updated.json")"
record_status "$updated_collection_status" "$workspace/domain-by-application.updated.status"
require_status "$updated_collection_status" "200" "domain.byApplicationId after update"

if ! jq -e \
    --arg domainId "$domain_id" \
    --arg host "$updated_host" \
    '. | length == 1 and .[0].domainId == $domainId and .[0].host == $host' \
    "$workspace/domain-by-application.updated.json" \
    >/dev/null
then
    echo "The updated Domain collection did not contain exactly the owned domain." >&2
    exit 1
fi

updated_application_status="$(api_request \
    GET \
    "application.one?applicationId=$application_id_encoded" \
    "$workspace/application-one.domain-updated.json")"
record_status "$updated_application_status" "$workspace/application-one.domain-updated.status"
require_status "$updated_application_status" "200" "application.one after Domain update"

if ! jq -e \
    --arg domainId "$domain_id" \
    --arg host "$updated_host" \
    '.applicationStatus == "idle"
        and .serverId == null
        and .server == null
        and ([.domains[] | select(.domainId == $domainId and .host == $host)] | length == 1)' \
    "$workspace/application-one.domain-updated.json" \
    >/dev/null
then
    echo "Domain update changed the application deployment state or was not embedded." >&2
    exit 1
fi

capture_traefik_manifest "$workspace/traefik.updated.manifest"

delete_body="$workspace/domain-delete.request.json"
jq -n --arg domainId "$domain_id" '{domainId:$domainId}' >"$delete_body"

delete_status="$(api_request POST "domain.delete" "$workspace/domain-delete.json" "$delete_body")"
record_status "$delete_status" "$workspace/domain-delete.status"
require_status "$delete_status" "200" "domain.delete"

deleted_one_status="$(api_request \
    GET \
    "domain.one?domainId=$domain_id_encoded" \
    "$workspace/domain-one.deleted.json")"
record_status "$deleted_one_status" "$workspace/domain-one.deleted.status"
require_status "$deleted_one_status" "404" "domain.one after delete"

deleted_collection_status="$(api_request \
    GET \
    "domain.byApplicationId?applicationId=$application_id_encoded" \
    "$workspace/domain-by-application.deleted.json")"
record_status "$deleted_collection_status" "$workspace/domain-by-application.deleted.status"
require_status "$deleted_collection_status" "200" "domain.byApplicationId after delete"

if ! jq -e \
    --arg domainId "$domain_id" \
    --arg createdHost "$created_host" \
    --arg updatedHost "$updated_host" \
    '[.[] | select(
        .domainId == $domainId
        or .host == $createdHost
        or .host == $updatedHost
    )] | length == 0' \
    "$workspace/domain-by-application.deleted.json" \
    >/dev/null
then
    echo "The Domain collection still contains an owned identifier or host after deletion." >&2
    exit 1
fi

deleted_application_status="$(api_request \
    GET \
    "application.one?applicationId=$application_id_encoded" \
    "$workspace/application-one.domain-deleted.json")"
record_status "$deleted_application_status" "$workspace/application-one.domain-deleted.status"
require_status "$deleted_application_status" "200" "application.one after Domain delete"

if ! jq -e \
    --arg domainId "$domain_id" \
    --arg createdHost "$created_host" \
    --arg updatedHost "$updated_host" \
    '.applicationStatus == "idle"
        and .serverId == null
        and .server == null
        and ([.domains[] | select(
            .domainId == $domainId
            or .host == $createdHost
            or .host == $updatedHost
        )] | length == 0)' \
    "$workspace/application-one.domain-deleted.json" \
    >/dev/null
then
    echo "The application retained the Domain or changed deployment state after deletion." >&2
    exit 1
fi

capture_traefik_manifest "$workspace/traefik.after.manifest"

if ! cmp -s "$workspace/traefik.before.manifest" "$workspace/traefik.after.manifest"; then
    echo "Traefik files did not return to their exact pre-capture content after Domain deletion." >&2
    exit 1
fi

if docker exec "$dokploy_container_id" \
    grep -R -F -q -- "$created_host" /etc/dokploy/traefik; then
    echo "Traefik configuration still contains the created Domain host." >&2
    exit 1
fi

if docker exec "$dokploy_container_id" \
    grep -R -F -q -- "$updated_host" /etc/dokploy/traefik; then
    echo "Traefik configuration still contains the updated Domain host." >&2
    exit 1
fi

cleanup_confirmed=true

candidate_fixture_root="$workspace/candidate/api/live"
candidate_versioned_fixture_directory="$candidate_fixture_root/$dokploy_version"
mkdir -p "$candidate_versioned_fixture_directory"
cp -R "$fixture_directory/." "$candidate_versioned_fixture_directory/"

fixture_mappings=(
    "domain-create.owner.json:domain-create.json"
    "domain-one.created.owner.json:domain-one.created.json"
    "domain-by-application.created.owner.json:domain-by-application.created.json"
    "application-one.domain-created.owner.json:application-one.domain-created.json"
    "domain-update.owner.json:domain-update.json"
    "domain-one.updated.owner.json:domain-one.updated.json"
    "domain-by-application.updated.owner.json:domain-by-application.updated.json"
    "application-one.domain-updated.owner.json:application-one.domain-updated.json"
    "domain-delete.owner.json:domain-delete.json"
    "domain-one.deleted.owner.json:domain-one.deleted.json"
    "domain-by-application.deleted.owner.json:domain-by-application.deleted.json"
    "application-one.domain-deleted.owner.json:application-one.domain-deleted.json"
)

for fixture_mapping in "${fixture_mappings[@]}"; do
    fixture_name="${fixture_mapping%%:*}"
    fixture_source="${fixture_mapping#*:}"
    jq --sort-keys --indent 2 \
        --from-file "$sanitizer" \
        "$workspace/$fixture_source" \
        >"$candidate_versioned_fixture_directory/$fixture_name"
done

traefik_changed_during_capture=false

if ! cmp -s "$workspace/traefik.before.manifest" "$workspace/traefik.created.manifest" \
    || ! cmp -s "$workspace/traefik.before.manifest" "$workspace/traefik.updated.manifest"
then
    traefik_changed_during_capture=true
fi

jq -n --sort-keys --indent 2 \
    --arg capturedAt "$captured_at" \
    --arg version "$runtime_version" \
    --arg image "$runtime_image" \
    --argjson traefikChangedDuringCapture "$traefik_changed_during_capture" \
    '{
        capturedAt:$capturedAt,
        cleanupEvidence:{
            applicationDomainAbsent:true,
            applicationIdle:true,
            collectionEmpty:true,
            oneStatus:404,
            traefikRestored:true
        },
        deployed:false,
        endpoints:[
            "domain.create",
            "domain.one",
            "domain.byApplicationId",
            "domain.update",
            "domain.delete",
            "application.one"
        ],
        image:$image,
        mutations:{create:1, update:1, delete:1},
        role:"owner",
        sanitized:true,
        traefikChangedDuringCapture:$traefikChangedDuringCapture,
        version:$version
    }' \
    >"$candidate_versioned_fixture_directory/domain-contract.metadata.json"

find "$candidate_fixture_root" -type d -exec chmod 755 {} +
find "$candidate_fixture_root" -type f -exec chmod 644 {} +

if grep -R -F -q -f "$api_key_file" "$candidate_fixture_root"; then
    echo "Sanitized Domain fixtures contain the local API key." >&2
    exit 1
fi

DOKPLOY_FIXTURE_DIRECTORY="$candidate_fixture_root" \
    "$script_directory/check-fixtures.sh"

publication_started=true
mv "$fixture_directory" "$published_fixture_backup"
mv "$candidate_versioned_fixture_directory" "$fixture_directory"
publication_complete=true

capture_succeeded=true

echo "Captured, updated, and removed a disposable Domain without deployment."
echo "Verified private raw responses will now be discarded."
