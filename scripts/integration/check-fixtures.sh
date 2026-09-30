#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture_directory="${DOKPLOY_FIXTURE_DIRECTORY:-$repository_root/fixtures/api/live}"
versioned_fixture_directory="$fixture_directory/v0.30.6"

if [[ ! -d "$fixture_directory" ]]; then
    echo "No live fixtures found." >&2
    exit 1
fi

required_redis_fixtures=(
    "redis-create.owner.json"
    "redis-one.owner.json"
    "redis-search.populated.owner.json"
    "project-one.redis-populated.owner.json"
    "redis-remove.owner.json"
    "redis-one.removed.owner.json"
    "redis-search.removed.owner.json"
    "project-one.redis-removed.owner.json"
    "redis-contract.metadata.json"
)

required_domain_fixtures=(
    "domain-create.owner.json"
    "domain-one.created.owner.json"
    "domain-by-application.created.owner.json"
    "application-one.domain-created.owner.json"
    "domain-update.owner.json"
    "domain-one.updated.owner.json"
    "domain-by-application.updated.owner.json"
    "application-one.domain-updated.owner.json"
    "domain-delete.owner.json"
    "domain-one.deleted.owner.json"
    "domain-by-application.deleted.owner.json"
    "application-one.domain-deleted.owner.json"
    "domain-contract.metadata.json"
)

required_mysql_fixtures=(
    "mysql-create.owner.json"
    "mysql-one.created.owner.json"
    "mysql-search.created.owner.json"
    "mysql-update.owner.json"
    "mysql-change-user-password.idle.owner.json"
    "mysql-change-root-password.idle.owner.json"
    "mysql-one.updated.owner.json"
    "mysql-remove.owner.json"
    "mysql-one.removed.owner.json"
    "mysql-search.removed.owner.json"
    "project-one.mysql-removed.owner.json"
    "mysql-contract.metadata.json"
)

required_mariadb_fixtures=(
    "mariadb-create.owner.json"
    "mariadb-one.created.owner.json"
    "mariadb-search.created.owner.json"
    "mariadb-update.owner.json"
    "mariadb-change-user-password.idle.owner.json"
    "mariadb-change-root-password.idle.owner.json"
    "mariadb-one.updated.owner.json"
    "mariadb-remove.owner.json"
    "mariadb-one.removed.owner.json"
    "mariadb-search.removed.owner.json"
    "project-one.mariadb-removed.owner.json"
    "mariadb-contract.metadata.json"
)

required_mongo_fixtures=(
    "mongo-create.owner.json"
    "mongo-one.created.owner.json"
    "mongo-search.created.owner.json"
    "mongo-update.owner.json"
    "mongo-change-password.idle.owner.json"
    "mongo-one.updated.owner.json"
    "mongo-remove.owner.json"
    "mongo-one.removed.owner.json"
    "mongo-search.removed.owner.json"
    "project-one.mongo-removed.owner.json"
    "mongo-contract.metadata.json"
)

for fixture_name in "${required_redis_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing Redis contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_domain_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing Domain contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_mysql_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing MySQL contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_mariadb_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing MariaDB contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_mongo_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing MongoDB contract fixture: $fixture_name" >&2
        exit 1
    fi
done

if ! find "$fixture_directory" -type f -name '*.json' -print0 \
    | xargs -0 jq --exit-status '.' >/dev/null
then
    echo "Live fixtures must all contain valid JSON." >&2
    exit 1
fi

if ! jq --exit-status '
    .redisId == "redis-1"
    and .environmentId == "environment-1"
    and .name == "Redis Contract Test"
    and .appName == "redis-contract-test"
    and .databasePassword == "<redacted>"
    and .applicationStatus == "idle"
    and .serverId == null
    and .server == null
    and (.mounts | length) == 1
    and .mounts[0].redisId == "redis-1"
    and .mounts[0].mountId == "mount-1"
    and .mounts[0].volumeName == "volume-1"
    and .mounts[0].applicationId == null
    and .mounts[0].postgresId == null
' "$versioned_fixture_directory/redis-one.owner.json" >/dev/null; then
    echo "Redis detail fixture does not preserve the sanitized live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    [.environments[]? | select(.environmentId == "environment-1") | .redis[]?
        | select(
            .redisId == "redis-1"
            and .name == "Redis Contract Test"
            and .description == "Disposable Redis contract capture"
        )]
    | length == 1
' "$versioned_fixture_directory/project-one.redis-populated.owner.json" >/dev/null; then
    echo "Redis populated project fixture does not prove unique recovery." >&2
    exit 1
fi

if ! jq --exit-status '
    .total == 1
    and (.items | length) == 1
    and .items[0].redisId == "redis-1"
    and .items[0].environmentId == "environment-1"
' "$versioned_fixture_directory/redis-search.populated.owner.json" >/dev/null; then
    echo "Redis populated search fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '.items == [] and .total == 0' \
    "$versioned_fixture_directory/redis-search.removed.owner.json" >/dev/null
then
    echo "Redis cleanup search fixture is not empty." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "redis.one"
' "$versioned_fixture_directory/redis-one.removed.owner.json" >/dev/null; then
    echo "Redis cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.searchEmpty == true
    and .cleanupEvidence.projectOneAbsent == true
' "$versioned_fixture_directory/redis-contract.metadata.json" >/dev/null; then
    echo "Redis contract metadata does not prove capture and cleanup." >&2
    exit 1
fi

if ! jq --exit-status \
    '[.environments[]?.redis[]?] | length == 0' \
    "$versioned_fixture_directory/project-one.redis-removed.owner.json" >/dev/null
then
    echo "Redis cleanup project fixture still contains a Redis record." >&2
    exit 1
fi

if ! jq --exit-status '
    .domainId == "domain-1"
    and .applicationId == "application-1"
    and .host == "created.domain.example.test"
    and .domainType == "application"
    and .port == 3000
    and .https == false
    and .certificateType == "none"
    and .uniqueConfigKey == 1
' "$versioned_fixture_directory/domain-create.owner.json" >/dev/null; then
    echo "Domain create response fixture does not preserve the defaulted live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    .domainId == "domain-1"
    and .applicationId == "application-1"
    and .host == "created.domain.example.test"
    and .domainType == "application"
    and .port == 3000
    and .https == false
    and .certificateType == "none"
    and .uniqueConfigKey == 1
' "$versioned_fixture_directory/domain-one.created.owner.json" >/dev/null; then
    echo "Created Domain detail fixture does not preserve the defaulted live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    length == 1
    and .[0].domainId == "domain-1"
    and .[0].applicationId == "application-1"
    and .[0].host == "created.domain.example.test"
' "$versioned_fixture_directory/domain-by-application.created.owner.json" >/dev/null; then
    echo "Created Domain collection fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationId == "application-1"
    and .applicationStatus == "idle"
    and .serverId == null
    and .server == null
    and ([.domains[] | select(
        .domainId == "domain-1"
        and .host == "created.domain.example.test"
    )] | length == 1)
' "$versioned_fixture_directory/application-one.domain-created.owner.json" >/dev/null; then
    echo "Created Domain is not embedded in the idle application fixture." >&2
    exit 1
fi

if ! jq --exit-status '
    .domainId == "domain-1"
    and .applicationId == "application-1"
    and .host == "updated.domain.example.test"
    and .port == 8080
    and .https == true
    and .certificateType == "none"
    and .uniqueConfigKey == 1
' "$versioned_fixture_directory/domain-update.owner.json" >/dev/null; then
    echo "Domain update response fixture does not preserve the requested live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    .domainId == "domain-1"
    and .applicationId == "application-1"
    and .host == "updated.domain.example.test"
    and .port == 8080
    and .https == true
    and .certificateType == "none"
    and .uniqueConfigKey == 1
' "$versioned_fixture_directory/domain-one.updated.owner.json" >/dev/null; then
    echo "Updated Domain detail fixture does not preserve the requested live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    length == 1
    and .[0].domainId == "domain-1"
    and .[0].applicationId == "application-1"
    and .[0].host == "updated.domain.example.test"
' "$versioned_fixture_directory/domain-by-application.updated.owner.json" >/dev/null; then
    echo "Updated Domain collection fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationId == "application-1"
    and .applicationStatus == "idle"
    and .serverId == null
    and .server == null
    and ([.domains[] | select(
        .domainId == "domain-1"
        and .host == "updated.domain.example.test"
    )] | length == 1)
' "$versioned_fixture_directory/application-one.domain-updated.owner.json" >/dev/null; then
    echo "Updated Domain is not embedded in the idle application fixture." >&2
    exit 1
fi

if ! jq --exit-status '
    .domainId == "domain-1"
    and .applicationId == "application-1"
    and .host == "updated.domain.example.test"
    and .port == 8080
    and .https == true
    and .certificateType == "none"
    and .uniqueConfigKey == 1
' "$versioned_fixture_directory/domain-delete.owner.json" >/dev/null; then
    echo "Domain delete response fixture does not preserve the deleted live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "domain.one"
' "$versioned_fixture_directory/domain-one.deleted.owner.json" >/dev/null; then
    echo "Deleted Domain lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '. == []' \
    "$versioned_fixture_directory/domain-by-application.deleted.owner.json" >/dev/null
then
    echo "Domain collection is not empty after deletion." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationId == "application-1"
    and .applicationStatus == "idle"
    and .serverId == null
    and .server == null
    and ([.domains[] | select(
        .domainId == "domain-1"
        or .host == "created.domain.example.test"
        or .host == "updated.domain.example.test"
    )] | length == 0)
' "$versioned_fixture_directory/application-one.domain-deleted.owner.json" >/dev/null; then
    echo "Deleted Domain remains embedded in the application fixture." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .mutations == {create:1, update:1, delete:1}
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.collectionEmpty == true
    and .cleanupEvidence.applicationDomainAbsent == true
    and .cleanupEvidence.applicationIdle == true
    and .cleanupEvidence.traefikRestored == true
    and ([.endpoints[] | select(test("(?i)deploy"))] | length) == 0
' "$versioned_fixture_directory/domain-contract.metadata.json" >/dev/null; then
    echo "Domain contract metadata does not prove capture, non-deployment, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'iac-domain-contract-(created|updated)-' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable Domain host." >&2
    exit 1
fi

if ! jq --exit-status '
    .mysqlId == "mysql-1"
    and .environmentId == "environment-1"
    and .name == "MySQL Contract Test"
    and .appName == "mysql-contract-test"
    and .databaseName == "contract"
    and .databaseUser == "contract"
    and .databasePassword == "<redacted>"
    and .databaseRootPassword == "<redacted>"
    and .applicationStatus == "idle"
    and .serverId == null
    and .server == null
    and (.mounts | length) == 1
    and .mounts[0].mysqlId == "mysql-1"
    and .mounts[0].mountId == "mount-1"
    and .mounts[0].volumeName == "volume-1"
' "$versioned_fixture_directory/mysql-one.created.owner.json" >/dev/null; then
    echo "MySQL detail fixture does not preserve the sanitized live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    .total == 1
    and (.items | length) == 1
    and .items[0].mysqlId == "mysql-1"
    and .items[0].environmentId == "environment-1"
' "$versioned_fixture_directory/mysql-search.created.owner.json" >/dev/null; then
    echo "MySQL populated search fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .databaseName == "contract_next"
    and .databaseUser == "contract_next"
    and .databasePassword == "<redacted>"
    and .databaseRootPassword == "<redacted>"
    and .applicationStatus == "idle"
' "$versioned_fixture_directory/mysql-one.updated.owner.json" >/dev/null; then
    echo "MySQL updated detail fixture is incomplete." >&2
    exit 1
fi

for password_fixture in \
    mysql-change-user-password.idle.owner.json \
    mysql-change-root-password.idle.owner.json
do
    if ! jq --exit-status '
        .code == "BAD_REQUEST"
        and .data.httpStatus == 400
        and .data.path == "mysql.changePassword"
    ' "$versioned_fixture_directory/$password_fixture" >/dev/null; then
        echo "Idle MySQL password-change fixture is incomplete: $password_fixture" >&2
        exit 1
    fi
done

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "mysql.one"
' "$versioned_fixture_directory/mysql-one.removed.owner.json" >/dev/null; then
    echo "MySQL cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '.items == [] and .total == 0' \
    "$versioned_fixture_directory/mysql-search.removed.owner.json" >/dev/null
then
    echo "MySQL cleanup search fixture is not empty." >&2
    exit 1
fi

if ! jq --exit-status '[.environments[]?.mysql[]?] | length == 0' \
    "$versioned_fixture_directory/project-one.mysql-removed.owner.json" >/dev/null
then
    echo "MySQL cleanup project fixture still contains a MySQL record." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .idlePasswordChange == {userStatus:400, rootStatus:400}
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.searchEmpty == true
    and .cleanupEvidence.projectOneAbsent == true
' "$versioned_fixture_directory/mysql-contract.metadata.json" >/dev/null; then
    echo "MySQL contract metadata does not prove capture, non-deployment, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'mysql-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable MySQL project name." >&2
    exit 1
fi

if ! jq --exit-status '
    .mariadbId == "mariadb-1"
    and .environmentId == "environment-1"
    and .name == "MariaDB Contract Test"
    and .appName == "mariadb-contract-test"
    and .dockerImage == "mariadb:11"
    and .databaseName == "contract"
    and .databaseUser == "contract"
    and .databasePassword == "<redacted>"
    and .databaseRootPassword == "<redacted>"
    and .applicationStatus == "idle"
    and .serverId == null
    and .server == null
    and (.mounts | length) == 1
    and .mounts[0].mariadbId == "mariadb-1"
    and .mounts[0].mountId == "mount-1"
    and .mounts[0].volumeName == "volume-1"
' "$versioned_fixture_directory/mariadb-one.created.owner.json" >/dev/null; then
    echo "MariaDB detail fixture does not preserve the sanitized live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    .total == 1
    and (.items | length) == 1
    and .items[0].mariadbId == "mariadb-1"
    and .items[0].environmentId == "environment-1"
' "$versioned_fixture_directory/mariadb-search.created.owner.json" >/dev/null; then
    echo "MariaDB populated search fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .databaseName == "contract_next"
    and .databaseUser == "contract_next"
    and .databasePassword == "<redacted>"
    and .databaseRootPassword == "<redacted>"
    and .applicationStatus == "idle"
' "$versioned_fixture_directory/mariadb-one.updated.owner.json" >/dev/null; then
    echo "MariaDB updated detail fixture is incomplete." >&2
    exit 1
fi

for password_fixture in \
    mariadb-change-user-password.idle.owner.json \
    mariadb-change-root-password.idle.owner.json
do
    if ! jq --exit-status '
        .code == "BAD_REQUEST"
        and .data.httpStatus == 400
        and .data.path == "mariadb.changePassword"
        and .message == "No running container found for mariadb-contract-test"
    ' "$versioned_fixture_directory/$password_fixture" >/dev/null; then
        echo "Idle MariaDB password-change fixture is incomplete: $password_fixture" >&2
        exit 1
    fi
done

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "mariadb.one"
' "$versioned_fixture_directory/mariadb-one.removed.owner.json" >/dev/null; then
    echo "MariaDB cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '.items == [] and .total == 0' \
    "$versioned_fixture_directory/mariadb-search.removed.owner.json" >/dev/null
then
    echo "MariaDB cleanup search fixture is not empty." >&2
    exit 1
fi

if ! jq --exit-status '[.environments[]?.mariadb[]?] | length == 0' \
    "$versioned_fixture_directory/project-one.mariadb-removed.owner.json" >/dev/null
then
    echo "MariaDB cleanup project fixture still contains a MariaDB record." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .idlePasswordChange == {userStatus:400, rootStatus:400}
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.searchEmpty == true
    and .cleanupEvidence.projectOneAbsent == true
' "$versioned_fixture_directory/mariadb-contract.metadata.json" >/dev/null; then
    echo "MariaDB contract metadata does not prove capture, non-deployment, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'mariadb-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable MariaDB project name." >&2
    exit 1
fi

if grep -R -E 'No running container found for mariadb-' "$fixture_directory" \
    | grep -F -v -q 'mariadb-contract-test'
then
    echo "Live fixtures contain an unsanitized generated MariaDB application name." >&2
    exit 1
fi

if ! jq --exit-status '
    .mongoId == "mongo-1"
    and .environmentId == "environment-1"
    and .name == "MongoDB Contract Test"
    and .appName == "mongodb-contract-test"
    and .dockerImage == "mongo:8"
    and .databaseUser == "contract"
    and .databasePassword == "<redacted>"
    and .replicaSets == false
    and .applicationStatus == "idle"
    and .serverId == null
    and .server == null
    and (.mounts | length) == 1
    and .mounts[0].mongoId == "mongo-1"
    and .mounts[0].mountId == "mount-1"
    and .mounts[0].volumeName == "volume-1"
' "$versioned_fixture_directory/mongo-one.created.owner.json" >/dev/null; then
    echo "MongoDB detail fixture does not preserve the sanitized live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    .total == 1
    and (.items | length) == 1
    and .items[0].mongoId == "mongo-1"
    and .items[0].environmentId == "environment-1"
' "$versioned_fixture_directory/mongo-search.created.owner.json" >/dev/null; then
    echo "MongoDB populated search fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .databaseUser == "contract_next"
    and .databasePassword == "<redacted>"
    and .replicaSets == true
    and .applicationStatus == "idle"
' "$versioned_fixture_directory/mongo-one.updated.owner.json" >/dev/null; then
    echo "MongoDB updated detail fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "BAD_REQUEST"
    and .data.httpStatus == 400
    and .data.path == "mongo.changePassword"
    and .message == "No running container found for mongodb-contract-test"
' "$versioned_fixture_directory/mongo-change-password.idle.owner.json" >/dev/null; then
    echo "Idle MongoDB password-change fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "mongo.one"
' "$versioned_fixture_directory/mongo-one.removed.owner.json" >/dev/null; then
    echo "MongoDB cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '.items == [] and .total == 0' \
    "$versioned_fixture_directory/mongo-search.removed.owner.json" >/dev/null
then
    echo "MongoDB cleanup search fixture is not empty." >&2
    exit 1
fi

if ! jq --exit-status '[.environments[]?.mongo[]?] | length == 0' \
    "$versioned_fixture_directory/project-one.mongo-removed.owner.json" >/dev/null
then
    echo "MongoDB cleanup project fixture still contains a MongoDB record." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .idleCredentialChangeStatus == 400
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.searchEmpty == true
    and .cleanupEvidence.projectOneAbsent == true
' "$versioned_fixture_directory/mongo-contract.metadata.json" >/dev/null; then
    echo "MongoDB contract metadata does not prove capture, non-deployment, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'mongodb-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable MongoDB project name." >&2
    exit 1
fi

if grep -R -E 'No running container found for mongo-' "$fixture_directory" \
    | grep -F -v -q 'mongodb-contract-test'
then
    echo "Live fixtures contain an unsanitized generated MongoDB application name." >&2
    exit 1
fi

unsafe_values="$(
    find "$fixture_directory" -type f -name '*.json' -print0 \
    | xargs -0 jq -r '
        paths(scalars) as $path
        | ($path[-1] | tostring) as $key
        | getpath($path) as $value
        | select(
            $key == "env"
            or $key == "previewEnv"
            or $key == "buildArgs"
            or $key == "previewBuildArgs"
            or $key == "buildSecrets"
            or $key == "previewBuildSecrets"
            or ($key | test("(?i)(password|secret|token|privatekey|accesskey)"))
        )
        | select($value != null and $value != "" and $value != "<redacted>")
        | $path | map(tostring) | join(".")
    ' 2>/dev/null || true
)"

if [[ -n "$unsafe_values" ]]; then
    echo "Live fixtures contain unredacted secret-like values:" >&2
    echo "$unsafe_values" >&2
    exit 1
fi

runtime_secret_files=(
    "$repository_root/.integration/state/api-key"
    "$repository_root/.integration/state/admin-password"
    "$repository_root/.integration/secrets/auth_secret"
    "$repository_root/.integration/secrets/postgres_password"
)

while IFS= read -r -d '' generated_password_file; do
    runtime_secret_files+=("$generated_password_file")
done < <(find "$repository_root/.integration/state" \
    -mindepth 2 \
    -maxdepth 2 \
    -type f \
    -name database-password \
    -print0 2>/dev/null || true)

for secret_file in "${runtime_secret_files[@]}"; do
    if [[ -s "$secret_file" ]] && grep -R -F -q -f "$secret_file" "$fixture_directory"; then
        echo "Live fixtures contain a local integration secret." >&2
        exit 1
    fi
done

echo "Live fixture secret checks passed."
