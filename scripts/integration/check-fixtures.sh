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

required_libsql_fixtures=(
    "libsql-create.owner.json"
    "project-one.libsql-created.owner.json"
    "libsql-one.created.owner.json"
    "libsql-update.owner.json"
    "libsql-one.updated.owner.json"
    "libsql-change-password.owner.json"
    "libsql-one.password-updated.owner.json"
    "libsql-remove.owner.json"
    "libsql-one.removed.owner.json"
    "project-one.libsql-removed.owner.json"
    "libsql-contract.metadata.json"
)

required_compose_fixtures=(
    "compose-create.owner.json"
    "compose-one.created.owner.json"
    "compose-search.created.owner.json"
    "compose-update.owner.json"
    "compose-one.updated.owner.json"
    "compose-delete.owner.json"
    "compose-one.deleted.owner.json"
    "compose-search.deleted.owner.json"
    "project-one.compose-deleted.owner.json"
    "compose-contract.metadata.json"
)

required_mount_fixtures=(
    "mount-create.owner.json"
    "mount-one.created.owner.json"
    "mount-list.created.owner.json"
    "mount-update.owner.json"
    "mount-one.updated.owner.json"
    "mount-remove.owner.json"
    "mount-one.removed.owner.json"
    "mount-list.removed.owner.json"
    "application-one.mount-removed.owner.json"
    "project-one.mount-removed.owner.json"
    "mount-contract.metadata.json"
)

required_port_fixtures=(
    "port-create.owner.json"
    "port-one.created.owner.json"
    "application-one.port-created.owner.json"
    "port-update.owner.json"
    "port-one.updated.owner.json"
    "application-one.port-updated.owner.json"
    "port-delete.owner.json"
    "port-one.deleted.owner.json"
    "application-one.port-deleted.owner.json"
    "project-one.port-deleted.owner.json"
    "port-contract.metadata.json"
)

required_redirect_fixtures=(
    "redirect-create.owner.json"
    "application-one.redirect-created.owner.json"
    "redirect-one.created.owner.json"
    "redirect-update.owner.json"
    "redirect-one.updated.owner.json"
    "application-one.redirect-updated.owner.json"
    "redirect-delete.owner.json"
    "redirect-one.deleted.owner.json"
    "application-one.redirect-deleted.owner.json"
    "project-one.redirect-deleted.owner.json"
    "redirect-contract.metadata.json"
)

required_security_fixtures=(
    "security-create.owner.json"
    "application-one.security-created.owner.json"
    "security-one.created.owner.json"
    "security-one.updated.owner.json"
    "application-one.security-updated.owner.json"
    "security-delete.owner.json"
    "security-one.deleted.owner.json"
    "application-one.security-deleted.owner.json"
    "project-one.security-deleted.owner.json"
    "security-contract.metadata.json"
)

required_schedule_fixtures=(
    "schedule-create.application.owner.json"
    "schedule-one.application-created.owner.json"
    "schedule-list.application-created.owner.json"
    "schedule-update.application.owner.json"
    "schedule-one.application-updated.owner.json"
    "schedule-list.application-updated.owner.json"
    "schedule-delete.application.owner.json"
    "schedule-one.application-deleted.owner.json"
    "schedule-list.application-deleted.owner.json"
    "schedule-create.compose.owner.json"
    "schedule-one.compose-created.owner.json"
    "schedule-list.compose-created.owner.json"
    "schedule-update.compose.owner.json"
    "schedule-one.compose-updated.owner.json"
    "schedule-list.compose-updated.owner.json"
    "schedule-delete.compose.owner.json"
    "schedule-one.compose-deleted.owner.json"
    "schedule-list.compose-deleted.owner.json"
    "application-one.schedule-deleted.owner.json"
    "compose-one.schedule-deleted.owner.json"
    "project-one.schedule-deleted.owner.json"
    "schedule-contract.metadata.json"
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

for fixture_name in "${required_libsql_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing LibSQL contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_compose_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing Compose contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_mount_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing Mount contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_port_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing Port contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_redirect_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing Redirect contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_security_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing Security contract fixture: $fixture_name" >&2
        exit 1
    fi
done

for fixture_name in "${required_schedule_fixtures[@]}"; do
    if [[ ! -s "$versioned_fixture_directory/$fixture_name" ]]; then
        echo "Missing Schedule contract fixture: $fixture_name" >&2
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

if ! jq --exit-status '. == true' \
    "$versioned_fixture_directory/libsql-create.owner.json" >/dev/null
then
    echo "LibSQL create fixture does not preserve the boolean response contract." >&2
    exit 1
fi

if ! jq --exit-status '
    [.environments[]? | select(.environmentId == "environment-1") | .libsql[]?
        | select(
            .libsqlId == "libsql-1"
            and .name == "LibSQL Contract Test"
            and .appName == "libsql-contract-test"
        )]
    | length == 1
' "$versioned_fixture_directory/project-one.libsql-created.owner.json" >/dev/null; then
    echo "LibSQL project topology fixture does not prove unique identity recovery." >&2
    exit 1
fi

if ! jq --exit-status '
    .libsqlId == "libsql-1"
    and .environmentId == "environment-1"
    and .name == "LibSQL Contract Test"
    and .appName == "libsql-contract-test"
    and .dockerImage == "ghcr.io/tursodatabase/libsql-server:v0.24.32"
    and .databaseUser == "contract"
    and .databasePassword == "<redacted>"
    and .sqldNode == "primary"
    and .sqldPrimaryUrl == null
    and .enableNamespaces == false
    and .applicationStatus == "idle"
    and .serverId == null
    and .server == null
' "$versioned_fixture_directory/libsql-one.created.owner.json" >/dev/null; then
    echo "LibSQL detail fixture does not preserve the sanitized live contract." >&2
    exit 1
fi

if ! jq --exit-status '
    .databaseUser == "contract_next"
    and .databasePassword == "<redacted>"
    and .applicationStatus == "idle"
' "$versioned_fixture_directory/libsql-one.updated.owner.json" >/dev/null; then
    echo "LibSQL updated detail fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .databaseUser == "contract_next"
    and .databasePassword == "<redacted>"
' "$versioned_fixture_directory/libsql-one.password-updated.owner.json" >/dev/null; then
    echo "LibSQL credential update fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "libsql.one"
' "$versioned_fixture_directory/libsql-one.removed.owner.json" >/dev/null; then
    echo "LibSQL cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '[.environments[]?.libsql[]?] | length == 0' \
    "$versioned_fixture_directory/project-one.libsql-removed.owner.json" >/dev/null
then
    echo "LibSQL cleanup project fixture still contains a LibSQL record." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .discovery.source == "project.one"
    and .discovery.createResponse == "boolean"
    and .discovery.exactEnvironment == true
    and .discovery.preflightAbsenceRequired == true
    and .discovery.uniqueNameRequired == true
    and .discovery.createStatus == 200
    and .credentialUpdateStatus == 200
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.projectOneAbsent == true
' "$versioned_fixture_directory/libsql-contract.metadata.json" >/dev/null; then
    echo "LibSQL metadata does not prove discovery, non-deployment, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'libsql-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable LibSQL project name." >&2
    exit 1
fi

if ! jq --exit-status '
    .composeId == "compose-1"
    and .environmentId == "environment-1"
    and .name == "Compose Contract Test"
    and .appName == "compose-contract-test"
    and .composeFile == "<redacted>"
    and .refreshToken == "<redacted>"
    and .sourceType == "raw"
    and .composeType == "docker-compose"
    and .composeStatus == "idle"
    and .serverId == null
    and (.deployments | length) == 0
' "$versioned_fixture_directory/compose-one.created.owner.json" >/dev/null; then
    echo "Compose detail fixture does not preserve the sanitized undeployed contract." >&2
    exit 1
fi

if ! jq --exit-status '
    .total == 1
    and (.items | length) == 1
    and .items[0].composeId == "compose-1"
    and .items[0].environmentId == "environment-1"
' "$versioned_fixture_directory/compose-search.created.owner.json" >/dev/null; then
    echo "Compose populated search fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .name == "Compose Contract Updated"
    and .description == "Updated Compose SDK contract"
    and .composeFile == "<redacted>"
    and .composeStatus == "idle"
    and (.deployments | length) == 0
' "$versioned_fixture_directory/compose-one.updated.owner.json" >/dev/null; then
    echo "Compose updated detail fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "compose.one"
' "$versioned_fixture_directory/compose-one.deleted.owner.json" >/dev/null; then
    echo "Compose cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '.items == [] and .total == 0' \
    "$versioned_fixture_directory/compose-search.deleted.owner.json" >/dev/null
then
    echo "Compose cleanup search fixture is not empty." >&2
    exit 1
fi

if ! jq --exit-status '[.environments[]?.compose[]?] | length == 0' \
    "$versioned_fixture_directory/project-one.compose-deleted.owner.json" >/dev/null
then
    echo "Compose cleanup project fixture still contains a Compose record." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .createIdentity.direct == true
    and .createIdentity.parentVerified == true
    and .update.composeFilePersisted == true
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.searchEmpty == true
    and .cleanupEvidence.projectOneAbsent == true
' "$versioned_fixture_directory/compose-contract.metadata.json" >/dev/null; then
    echo "Compose metadata does not prove identity, non-deployment, update, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'compose-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable Compose project name." >&2
    exit 1
fi

if ! jq --exit-status '
    .mountId == "mount-1"
    and .type == "volume"
    and .volumeName == "volume-1"
    and .mountPath == "/data"
    and .serviceType == "application"
    and .applicationId == "application-1"
    and .content == null
    and .application.appName == "application-contract-test"
' "$versioned_fixture_directory/mount-one.created.owner.json" >/dev/null; then
    echo "Mount detail fixture does not preserve the sanitized typed target contract." >&2
    exit 1
fi

if ! jq --exit-status '
    length == 1
    and .[0].mountId == "mount-1"
    and .[0].serviceType == "application"
    and .[0].applicationId == "application-1"
' "$versioned_fixture_directory/mount-list.created.owner.json" >/dev/null; then
    echo "Mount target list fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .mountId == "mount-1"
    and .mountPath == "/updated"
' "$versioned_fixture_directory/mount-one.updated.owner.json" >/dev/null; then
    echo "Mount updated detail fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "mounts.one"
' "$versioned_fixture_directory/mount-one.removed.owner.json" >/dev/null; then
    echo "Mount cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '. == []' \
    "$versioned_fixture_directory/mount-list.removed.owner.json" >/dev/null
then
    echo "Mount cleanup target list is not empty." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.mounts | length) == 0
' "$versioned_fixture_directory/application-one.mount-removed.owner.json" >/dev/null; then
    echo "Mount cleanup application fixture retained runtime effects." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "project.one"
' "$versioned_fixture_directory/project-one.mount-removed.owner.json" >/dev/null; then
    echo "Mount disposable project cleanup is not proven." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .mountType == "volume"
    and .createIdentity.direct == true
    and .createIdentity.targetVerified == true
    and .update.mountPathPersisted == true
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.listEmpty == true
    and .cleanupEvidence.applicationMountsEmpty == true
    and .cleanupEvidence.projectOneStatus == 404
' "$versioned_fixture_directory/mount-contract.metadata.json" >/dev/null; then
    echo "Mount metadata does not prove identity, non-deployment, update, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'mount-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable Mount project name." >&2
    exit 1
fi

if ! jq --exit-status '
    .portId == "port-1"
    and .applicationId == "application-1"
    and .publishedPort == 18080
    and .targetPort == 8080
    and .publishMode == "ingress"
    and .protocol == "tcp"
' "$versioned_fixture_directory/port-one.created.owner.json" >/dev/null; then
    echo "Port detail fixture does not preserve the typed creation contract." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationId == "application-1"
    and (.ports | length) == 1
    and .ports[0].portId == "port-1"
    and .ports[0].applicationId == "application-1"
' "$versioned_fixture_directory/application-one.port-created.owner.json" >/dev/null; then
    echo "Port parent fixture does not prove one exact created identity." >&2
    exit 1
fi

if ! jq --exit-status '
    .portId == "port-1"
    and .applicationId == "application-1"
    and .publishedPort == 19090
    and .targetPort == 9090
    and .publishMode == "host"
    and .protocol == "udp"
' "$versioned_fixture_directory/port-one.updated.owner.json" >/dev/null; then
    echo "Port updated detail fixture does not preserve every mutable field." >&2
    exit 1
fi

if ! jq --exit-status '
    (.ports | length) == 1
    and .ports[0].portId == "port-1"
    and .ports[0].publishedPort == 19090
    and .ports[0].targetPort == 9090
    and .ports[0].publishMode == "host"
    and .ports[0].protocol == "udp"
' "$versioned_fixture_directory/application-one.port-updated.owner.json" >/dev/null; then
    echo "Port updated parent fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "BAD_REQUEST"
    and .data.httpStatus == 400
    and .data.path == "port.one"
' "$versioned_fixture_directory/port-one.deleted.owner.json" >/dev/null; then
    echo "Port cleanup lookup fixture does not preserve the runtime 400 response." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.ports | length) == 0
' "$versioned_fixture_directory/application-one.port-deleted.owner.json" >/dev/null; then
    echo "Port cleanup parent fixture retained runtime effects." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "project.one"
' "$versioned_fixture_directory/project-one.port-deleted.owner.json" >/dev/null; then
    echo "Port disposable project cleanup is not proven." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .createIdentity.direct == true
    and .createIdentity.parentVerified == true
    and .update.allFieldsPersisted == true
    and .update.parentVerified == true
    and .cleanupEvidence.oneStatus == 400
    and .cleanupEvidence.applicationPortsEmpty == true
    and .cleanupEvidence.projectOneStatus == 404
' "$versioned_fixture_directory/port-contract.metadata.json" >/dev/null; then
    echo "Port metadata does not prove identity, update, non-deployment, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'port-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable Port project name." >&2
    exit 1
fi

if ! jq --exit-status '. == true' \
    "$versioned_fixture_directory/redirect-create.owner.json" >/dev/null
then
    echo "Redirect create fixture does not preserve boolean acceptance." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationId == "application-1"
    and (.redirects | length) == 1
    and .redirects[0].redirectId == "redirect-1"
    and .redirects[0].applicationId == "application-1"
    and .redirects[0].regex == "^/legacy/(.*)$"
    and .redirects[0].replacement == "/current/$1"
    and .redirects[0].permanent == false
' "$versioned_fixture_directory/application-one.redirect-created.owner.json" >/dev/null; then
    echo "Redirect parent fixture does not prove one exact created identity." >&2
    exit 1
fi

if ! jq --exit-status '
    .redirectId == "redirect-1"
    and .applicationId == "application-1"
    and .regex == "^/legacy/(.*)$"
    and .replacement == "/current/$1"
    and .permanent == false
' "$versioned_fixture_directory/redirect-one.created.owner.json" >/dev/null; then
    echo "Redirect detail fixture does not agree with its created parent entry." >&2
    exit 1
fi

if ! jq --exit-status '
    .redirectId == "redirect-1"
    and .applicationId == "application-1"
    and .regex == "^/old/(.*)$"
    and .replacement == "/new/$1"
    and .permanent == true
' "$versioned_fixture_directory/redirect-one.updated.owner.json" >/dev/null; then
    echo "Redirect updated detail fixture does not preserve every mutable field." >&2
    exit 1
fi

if ! jq --exit-status '
    (.redirects | length) == 1
    and .redirects[0].redirectId == "redirect-1"
    and .redirects[0].regex == "^/old/(.*)$"
    and .redirects[0].replacement == "/new/$1"
    and .redirects[0].permanent == true
' "$versioned_fixture_directory/application-one.redirect-updated.owner.json" >/dev/null; then
    echo "Redirect updated parent fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "redirects.one"
' "$versioned_fixture_directory/redirect-one.deleted.owner.json" >/dev/null; then
    echo "Redirect cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.redirects | length) == 0
' "$versioned_fixture_directory/application-one.redirect-deleted.owner.json" >/dev/null; then
    echo "Redirect cleanup parent fixture retained runtime effects." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "project.one"
' "$versioned_fixture_directory/project-one.redirect-deleted.owner.json" >/dev/null; then
    echo "Redirect disposable project cleanup is not proven." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .collisionKey == "application+regex"
    and .createIdentity.setDifference == true
    and .createIdentity.parentVerified == true
    and .update.allFieldsPersisted == true
    and .update.parentVerified == true
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.applicationRedirectsEmpty == true
    and .cleanupEvidence.projectOneStatus == 404
' "$versioned_fixture_directory/redirect-contract.metadata.json" >/dev/null; then
    echo "Redirect metadata does not prove identity, update, non-deployment, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'redirect-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable Redirect project name." >&2
    exit 1
fi

if ! jq --exit-status '. == true' \
    "$versioned_fixture_directory/security-create.owner.json" >/dev/null
then
    echo "Security create fixture does not preserve boolean acceptance." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationId == "application-1"
    and (.security | length) == 1
    and .security[0].securityId == "security-1"
    and .security[0].applicationId == "application-1"
    and .security[0].username == "owner"
    and .security[0].password == "<redacted>"
' "$versioned_fixture_directory/application-one.security-created.owner.json" >/dev/null; then
    echo "Security parent fixture does not prove one exact created identity." >&2
    exit 1
fi

if ! jq --exit-status '
    .securityId == "security-1"
    and .applicationId == "application-1"
    and .username == "owner"
    and .password == "<redacted>"
' "$versioned_fixture_directory/security-one.created.owner.json" >/dev/null; then
    echo "Security detail fixture does not agree with its created parent entry." >&2
    exit 1
fi

if ! jq --exit-status '
    .securityId == "security-1"
    and .applicationId == "application-1"
    and .username == "operator"
    and .password == "<redacted>"
' "$versioned_fixture_directory/security-one.updated.owner.json" >/dev/null; then
    echo "Security updated detail fixture does not preserve complete credentials." >&2
    exit 1
fi

if ! jq --exit-status '
    (.security | length) == 1
    and .security[0].securityId == "security-1"
    and .security[0].applicationId == "application-1"
    and .security[0].username == "operator"
    and .security[0].password == "<redacted>"
' "$versioned_fixture_directory/application-one.security-updated.owner.json" >/dev/null; then
    echo "Security updated parent fixture is incomplete." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "security.one"
' "$versioned_fixture_directory/security-one.deleted.owner.json" >/dev/null; then
    echo "Security cleanup lookup fixture is not a 404 response." >&2
    exit 1
fi

if ! jq --exit-status '
    .applicationStatus == "idle"
    and (.deployments | length) == 0
    and (.security | length) == 0
' "$versioned_fixture_directory/application-one.security-deleted.owner.json" >/dev/null; then
    echo "Security cleanup parent fixture retained runtime effects." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND"
    and .data.httpStatus == 404
    and .data.path == "project.one"
' "$versioned_fixture_directory/project-one.security-deleted.owner.json" >/dev/null; then
    echo "Security disposable project cleanup is not proven." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .collisionKey == "application+username"
    and .credentialsVerifiedPrivately == true
    and .createIdentity.setDifference == true
    and .createIdentity.parentVerified == true
    and .update.completeFieldsPersisted == true
    and .update.parentVerified == true
    and .cleanupEvidence.oneStatus == 404
    and .cleanupEvidence.applicationSecurityEmpty == true
    and .cleanupEvidence.projectOneStatus == 404
' "$versioned_fixture_directory/security-contract.metadata.json" >/dev/null; then
    echo "Security metadata does not prove credentials, identity, non-deployment, and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'security-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable Security project name." >&2
    exit 1
fi

for schedule_type in application compose; do
    if ! jq --exit-status --arg type "$schedule_type" '
        .scheduleId == "schedule-1"
        and .scheduleType == $type
        and .name == ($type + "-job")
        and .cronExpression == "7 4 * * 0"
        and .shellType == "bash"
        and .command == "<redacted>"
        and .script == "<redacted>"
        and .enabled == false
        and .timezone == "UTC"
        and (if $type == "application" then
            .applicationId == "application-1" and .composeId == null and .serviceName == null
        else
            .composeId == "compose-1" and .applicationId == null and .serviceName == "worker"
        end)
    ' "$versioned_fixture_directory/schedule-one.$schedule_type-created.owner.json" >/dev/null; then
        echo "Created $schedule_type Schedule fixture is incomplete." >&2
        exit 1
    fi

    if ! jq --exit-status --arg type "$schedule_type" '
        length == 1
        and .[0].scheduleId == "schedule-1"
        and .[0].scheduleType == $type
        and (.[0].deployments | length) == 0
        and .[0].command == "<redacted>"
        and .[0].script == "<redacted>"
    ' "$versioned_fixture_directory/schedule-list.$schedule_type-created.owner.json" >/dev/null; then
        echo "Created $schedule_type Schedule collection is not authoritative." >&2
        exit 1
    fi

    if ! jq --exit-status --arg type "$schedule_type" '
        .scheduleId == "schedule-1"
        and .scheduleType == $type
        and .name == ($type + "-job-updated")
        and .description == "updated disabled schedule"
        and .cronExpression == "13 5 * * 1"
        and .shellType == "sh"
        and .command == "<redacted>"
        and .script == "<redacted>"
        and .enabled == false
        and .timezone == "Africa/Lubumbashi"
    ' "$versioned_fixture_directory/schedule-one.$schedule_type-updated.owner.json" >/dev/null; then
        echo "Updated $schedule_type Schedule fixture is incomplete." >&2
        exit 1
    fi

    if ! jq --exit-status '
        length == 1
        and (.[0].deployments | length) == 0
        and (.[0].name | endswith("-updated"))
    ' "$versioned_fixture_directory/schedule-list.$schedule_type-updated.owner.json" >/dev/null; then
        echo "Updated $schedule_type Schedule collection retained execution evidence." >&2
        exit 1
    fi

    if ! jq --exit-status '
        .code == "NOT_FOUND"
        and .data.httpStatus == 404
        and .data.path == "schedule.one"
    ' "$versioned_fixture_directory/schedule-one.$schedule_type-deleted.owner.json" >/dev/null; then
        echo "Deleted $schedule_type Schedule lookup is not a 404." >&2
        exit 1
    fi

    if ! jq --exit-status 'length == 0' \
        "$versioned_fixture_directory/schedule-list.$schedule_type-deleted.owner.json" >/dev/null
    then
        echo "Deleted $schedule_type Schedule remains in its target collection." >&2
        exit 1
    fi
done

if ! jq --exit-status '.applicationStatus == "idle" and (.deployments | length) == 0' \
    "$versioned_fixture_directory/application-one.schedule-deleted.owner.json" >/dev/null
then
    echo "Schedule application target retained deployment effects." >&2
    exit 1
fi

if ! jq --exit-status '.composeStatus == "idle" and (.deployments | length) == 0' \
    "$versioned_fixture_directory/compose-one.schedule-deleted.owner.json" >/dev/null
then
    echo "Schedule Compose target retained deployment effects." >&2
    exit 1
fi

if ! jq --exit-status '
    .code == "NOT_FOUND" and .data.httpStatus == 404 and .data.path == "project.one"
' "$versioned_fixture_directory/project-one.schedule-deleted.owner.json" >/dev/null; then
    echo "Schedule disposable project cleanup is not proven." >&2
    exit 1
fi

if ! jq --exit-status '
    .version == "v0.30.6"
    and .image == "dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"
    and .sanitized == true
    and .deployed == false
    and .executed == false
    and .collisionKey == "target+name"
    and .supportedTargets == ["application", "compose"]
    and .privilegedTargetsSupported == false
    and .executableTextVerifiedPrivately == true
    and .createIdentity.direct == true
    and .createIdentity.parentVerified == true
    and .update.allSafeMutableFieldsPersisted == true
    and .update.parentVerified == true
    and .update.enabled == false
    and .cleanupEvidence.applicationSchedulesEmpty == true
    and .cleanupEvidence.composeSchedulesEmpty == true
    and .cleanupEvidence.projectOneStatus == 404
' "$versioned_fixture_directory/schedule-contract.metadata.json" >/dev/null; then
    echo "Schedule metadata does not prove safe lifecycle and cleanup." >&2
    exit 1
fi

if grep -R -E -q 'schedule-sdk-contract-[0-9]' "$fixture_directory"; then
    echo "Live fixtures contain an unsanitized disposable Schedule project name." >&2
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
            or $key == "composeFile"
            or $key == "buildArgs"
            or $key == "previewBuildArgs"
            or $key == "buildSecrets"
            or $key == "previewBuildSecrets"
            or $key == "content"
            or $key == "command"
            or $key == "script"
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
