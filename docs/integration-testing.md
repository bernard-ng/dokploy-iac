# Local Dokploy integration environment

The integration environment runs Dokploy `v0.30.6` and PostgreSQL 16.10 from
digest-pinned multi-platform images. It binds the Dokploy UI and API only to
`127.0.0.1:33000` by default. PostgreSQL is isolated on an internal network and
is not published to the host.

Dokploy requires access to the Docker socket to exercise resource operations.
The compose file therefore mounts `/var/run/docker.sock` into the Dokploy
container. Use this environment only as a disposable local test fixture.

The Phase 0 contract suite creates resource records but does not deploy them,
so Docker Swarm is not required. Later executor tests that deploy workloads
must run on a dedicated Docker engine initialized as a Swarm manager; the
scripts do not silently change the host engine's Swarm state.

Start it with:

```bash
scripts/integration/up.sh
```

Stop it while retaining data with:

```bash
scripts/integration/down.sh
```

Delete its containers, volumes, generated secrets, and captured local state
with:

```bash
scripts/integration/reset.sh
```

Generated credentials live under `.integration/`, are mode `0600`, and are
ignored by Git. No integration credential belongs in a tracked environment
file or fixture.

`up.sh` creates the disposable administrator and an API key with rate limiting
disabled so a fixture run cannot lock itself out halfway through. Capture the
Phase 0 contracts on a fresh instance with:

```bash
scripts/integration/capture-fixtures.sh
```

The capture process keeps raw responses under ignored `.integration/` storage,
normalizes unstable identifiers and timestamps, redacts secret-like fields, and
writes only sanitized responses under `fixtures/api/live/`.

## Disposable Redis contract capture

Capture the currently undocumented Redis response shapes separately from the
long-lived Phase 0 topology:

```bash
scripts/integration/capture-redis-contract.sh
```

The command requires the populated `IaC Contract Test` project and an empty
Redis collection in its `production` environment. It creates one
collision-resistant Redis record, reads it through `redis.one` and
`redis.search`, and removes it without deploying it. A cleanup trap is
installed before the create request. If creation returns an incomplete or
interrupted response, cleanup recovers only a unique record carrying the run's
name and ownership description from `project.one`; ambiguous matches are never
deleted.

API keys and generated passwords are passed to the HTTP client through
owner-only files rather than command arguments. Raw requests and responses are
captured in a mode-`0700` run directory under `.integration/state/`, with files
set to mode `0600`. A successful, fully verified run deletes this private
workspace; a failed run retains it for cleanup diagnosis. Retained raw evidence
contains secrets and must not be copied into tracked paths. The tracked fixtures
normalize IDs, timestamps, generated application names, and volume names, and
redact every secret-bearing field. The complete candidate fixture tree is
validated before a rollback-safe directory replacement publishes it.

A successful capture first proves that `project.one` exposes exactly one record
matching the collision-resistant name and ownership description used by
failure recovery. It then proves cleanup three ways: `redis.one` returns 404,
the environment-scoped `redis.search` result is empty, and `project.one` no
longer contains the owned record. The proof and exact Dokploy image provenance
are recorded in `redis-contract.metadata.json`. `check-fixtures.sh` validates
these invariants and scans tracked fixtures against every available local
integration secret. The capture also verifies the running container's exact
image reference and requires the Redis record to remain idle with no assigned
server before recording that no deployment occurred.

## Disposable MySQL contract capture

Capture the MySQL mutation contract against the pinned local instance with:

```bash
scripts/integration/capture-mysql-contract.sh
```

The command creates a disposable project, environment, and MySQL record. It
captures create, read, environment-scoped search, metadata update, user and
root password change attempts, and delete without deploying the database. On
Dokploy `v0.30.6`, both password-change endpoints reject an idle database with
a structured HTTP 400 response because no database container is running. The
SDK transport tests independently verify the exact successful request bodies
for the explicit user and root password variants.

The capture requires both the database user password and MySQL root password,
keeps raw secret-bearing evidence only under ignored owner-only storage, and
publishes sanitized fixtures after proving authoritative absence through
`mysql.one`, `mysql.search`, and `project.one`. A cleanup trap uses the unique
ownership marker to recover the exact disposable record after interruption.
The script never deploys the database.

## Disposable MariaDB contract capture

Capture the MariaDB adapter contract against the pinned local instance with:

```bash
scripts/integration/capture-mariadb-contract.sh
```

The command creates a disposable project, environment, and MariaDB record. It
supplies both user and root passwords during capture, then records create,
read, environment-scoped search, metadata update, both password-change
variants, and delete. The database remains idle and is never deployed. Dokploy
`v0.30.6` therefore returns a structured HTTP 400 response for user and root
password changes because no database container is running; SDK transport tests
verify both successful request contracts independently.

Raw passwords and generated identifiers remain in ignored owner-only storage.
Published fixtures redact both passwords and normalize the generated MariaDB
application name, including the name embedded in idle password-change errors.
Before publishing, the command proves cleanup through `mariadb.one`,
`mariadb.search`, and `project.one`. An ownership-scoped cleanup trap removes
the disposable database and project after interruption.

## Disposable MongoDB contract capture

Capture the MongoDB adapter contract against the pinned local instance with:

```bash
scripts/integration/capture-mongo-contract.sh
```

The command creates a disposable project, environment, and MongoDB record. It
captures create, read, environment-scoped search, username and replica-set
update, password change, and delete. The database remains idle and is never
deployed. Dokploy `v0.30.6` therefore returns a structured HTTP 400 response
for the password change because no database container is running; the SDK
transport test verifies the successful request contract independently.

Raw passwords and generated identifiers remain in ignored owner-only storage.
Published fixtures redact the password and normalize the generated MongoDB
application name, including the name embedded in the idle password-change
error. Before publishing, the command proves cleanup through `mongo.one`,
`mongo.search`, and `project.one`. An ownership-scoped cleanup trap removes the
disposable database and project after interruption.

## Disposable LibSQL contract capture

Capture the LibSQL adapter contract against the pinned local instance with:

```bash
scripts/integration/capture-libsql-contract.sh
```

The command creates an undeployed LibSQL record, proves its identity through
the exact environment nested in `project.one`, reads it through `libsql.one`,
updates its username and authentication credential, and removes it. The
capture fails closed unless the requested name is unique in that environment.
This is required because `libsql.create` returns only a boolean and the pinned
API has no LibSQL search operation. Dokploy also suffixes the requested
application name, so it cannot serve as a stable recovery key.

The raw password/authentication token remains only in ignored owner-only
storage. The command privately verifies that credential rotation persisted,
then publishes a redacted detail fixture. Cleanup is proven through both the
404 from `libsql.one` and absence from the exact project environment. The
database is never deployed.

## Disposable Compose contract capture

Capture the raw Compose adapter contract against the pinned local instance
with:

```bash
scripts/integration/capture-compose-contract.sh
```

The command creates a disposable project and raw Compose record, verifies the
direct create identity against its requested environment and name, reads the
record through `compose.one` and bounded `compose.search`, updates its metadata
and opaque document, and deletes it while preserving volumes. The record must
remain idle with an empty deployment history throughout; the script never
calls a deploy operation.

Raw Compose content and Dokploy's generated refresh token remain only in the
ignored owner-only capture workspace. Published fixtures redact both fields.
Cleanup is proven through the 404 from `compose.one`, an empty scoped search,
and absence from `project.one`; the disposable project is then removed. An
interruption trap recovers only a unique name match in the dedicated
environment and refuses ambiguous deletion.

## Disposable Mount contract capture

Capture the typed Mount adapter contract against the pinned local instance
with:

```bash
scripts/integration/capture-mount-contract.sh
```

The command creates a disposable project and application, then exercises the
exact `mounts.create`, `mounts.one`, `mounts.listByServiceId`, `mounts.update`,
and `mounts.remove` transport using a volume Mount. The application remains
idle and undeployed. Cleanup is proven through the Mount 404, an empty target
list, an application with no Mounts or deployments, and a project 404 after
the disposable hierarchy is removed.

An interruption trap recovers only one volume matching the run-specific name,
typed application target, and known Mount paths. Ambiguous candidates are
never removed. Opaque file content is covered by SDK transport tests instead
of tracked live fixtures; the shared sanitizer and fixture checker treat every
non-empty `content` field as secret-bearing data.

Exercise the public SDK adapter itself through the same lifecycle with:

```bash
scripts/integration/test-mount-sdk.sh
```

This wrapper owns a separate disposable project and application. Its cleanup
trap removes that hierarchy after interruption and verifies authoritative
project absence. A successful run additionally requires the application to
remain idle, undeployed, and free of Mounts before it removes the project.

## Live SDK contract tests

After the populated fixtures have been captured, exercise the public SDK
against the running instance with:

```bash
scripts/integration/test-sdk.sh
```

The wrapper verifies the pinned Dokploy version and required fixture topology,
then supplies the ignored local API key only to the test process. The tests are
ignored during ordinary `cargo test` runs and perform no remote mutations.
They cover fresh project topology reads, project, environment, application,
and Postgres lookup, parent-scoped environment collections, fully collected
parent-scoped application, Postgres, and empty Redis search, and normalization
of Dokploy's sparse authentication error response.

## Live CLI redaction test

Verify that imperative command output does not expose the database password or
other secret-bearing fields returned by the live API:

```bash
scripts/integration/test-cli-redaction.sh
```

The wrapper resolves the ignored local API key and a disposable Postgres
fixture, invokes the public CLI, and fails if the known fixture secret appears
in either standard output or diagnostics.

## Live declarative apply test

Verify the Phase 6 executor against a disposable project containing an
environment, application, domain, Postgres database, and Redis database:

```bash
scripts/integration/test-apply.sh
```

The check uses environment-backed one-shot secrets, confirms the rendered plan,
executes with a parallelism bound of two, and requires the next fresh plan to
contain zero changes. It does not deploy the empty application and therefore
does not require Docker Swarm. Cleanup removes only the collision-resistant
project created by that run and deletes its ignored temporary workspace.
