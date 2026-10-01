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

## Disposable Port contract capture

Capture the typed application Port contract against the pinned local instance
with:

```bash
scripts/integration/capture-port-contract.sh
```

The command creates a disposable project and undeployed application, then
exercises the exact `port.create`, `port.one`, `port.update`, and `port.delete`
operations. It verifies the direct record and the authoritative `ports`
relation from `application.one` after create and after an update that changes
both port numbers, publish mode, and protocol. The captured request and response
shapes preserve port numbers as JSON integers.

Dokploy `v0.30.6` returns HTTP 400 from `port.one` after successful deletion.
The capture therefore proves deletion through an empty parent collection, then
proves the application stayed idle with no deployments and verifies project
absence after removing the disposable hierarchy. The cleanup trap can recover
the run's uniquely named project after an interrupted create and removes only
that owned hierarchy.

Exercise the public SDK adapter through the same undeployed lifecycle with:

```bash
scripts/integration/test-port-sdk.sh
```

The wrapper supplies the owner-only local credential only to the test process.
It verifies create, direct and parent reads, an all-field update, delete, parent
absence, and cleanup without deploying the application.

## Disposable Redirect contract capture

Capture the application Redirect contract against the pinned local instance
with:

```bash
scripts/integration/capture-redirect-contract.sh
```

The command creates a disposable project and undeployed application. It proves
the boolean response from `redirects.create`, discovers exactly one new
identity through the before/after `application.one.redirects` set difference,
and requires `redirects.one` to agree with that authoritative parent entry. It
then updates the regular expression, replacement, and permanent flag together,
verifies the direct and parent views, and deletes the Redirect.

Cleanup requires a 404 from `redirects.one`, an empty parent collection, an
idle application with no deployments, and a 404 after removing the disposable
project. The cleanup trap can recover the run's uniquely named project after an
interrupted create and removes only that owned hierarchy.

Exercise the public SDK adapter through the same undeployed lifecycle with:

```bash
scripts/integration/test-redirect-sdk.sh
```

The wrapper supplies the owner-only local credential only to the test process.
It verifies create discovery, direct/parent agreement, all-field update,
authoritative deletion, and complete project cleanup.

## Disposable Security contract capture

Capture the application basic-auth contract against the pinned local instance
with:

```bash
scripts/integration/capture-security-contract.sh
```

The command creates a disposable project and undeployed application. It proves
the boolean response from `security.create`, discovers exactly one new identity
through the before/after `application.one.security` set difference, and
requires `security.one` to agree with that authoritative parent entry. The raw
capture privately verifies the created password and the complete username and
password update. The sanitizer publishes only `<redacted>` password values.

Cleanup requires a 404 from `security.one`, an empty parent collection, an idle
application with no deployments, and a 404 after removing the disposable
project. The cleanup trap recovers only the run's uniquely named project after
an interruption.

Exercise the public SDK adapter through the same undeployed lifecycle with:

```bash
scripts/integration/test-security-sdk.sh
```

The wrapper supplies the owner-only local credential only to the test process.
It verifies create discovery, secret-safe direct and parent reads, complete
credential update, authoritative deletion, and project cleanup.

## Disposable Schedule contract capture

Capture the Application and Compose Schedule contracts against the pinned
local instance with:

```bash
scripts/integration/capture-schedule-contract.sh
```

The command creates a disposable project with an undeployed application and an
undeployed Compose service. It keeps both Schedules disabled and privately
verifies the created and updated command and script text. Each lifecycle proves
the identity returned by `schedule.create`, agreement between `schedule.one`
and the authoritative target-scoped `schedule.list`, complete safe-field
update, delete, and collection absence. Host and Dokploy-server Schedules are
deliberately unsupported.

Cleanup requires empty Application and Compose Schedule collections, idle
targets with no deployment history, and a 404 after removing the disposable
project. The capture validates a complete candidate fixture tree, including
API-key and executable-text canary scans, before replacing the tracked fixture
directory. A failed validation keeps its private evidence and leaves the
tracked tree unchanged.

Exercise the public SDK adapter through the same undeployed lifecycles with:

```bash
scripts/integration/test-schedule-sdk.sh
```

The wrapper supplies the owner-only local credential only to the test process.
It verifies safe reads, collision preflight, all-field disabled updates,
authoritative deletion, absence of executions or deployments, and complete
project cleanup.

## Disposable Backup contract capture

Capture the database Backup contract against the pinned local instance with:

```bash
scripts/integration/capture-backup-contract.sh
```

The command creates a disposable undeployed PostgreSQL target and two inert
backup destinations. It never calls `destination.testConnection`. Both
destinations point to a loopback tripwire, and the Backup stays disabled for
the entire create, direct and authoritative read, all-field update, delete,
and absence lifecycle. The run fails if the target deploys, a Backup execution
appears, or either destination receives a connection.

The capture publishes only allowlisted Backup fields. Nested target and
destination records are excluded rather than merely redacted. The complete
candidate fixture tree must pass the contract checker, local API-key scan, and
generated database and destination credential canary scan before it replaces
the tracked fixture directory. Interrupted publication restores the previous
tree. Cleanup removes the disposable project, both destinations, and the
tripwire through their exact run-scoped identities.

Exercise the public SDK adapter through the same disabled lifecycle with:

```bash
scripts/integration/test-backup-sdk.sh
```

The wrapper verifies create identity discovery, direct and parent agreement,
destination replacement and every other mutable field, delete, authoritative
absence, zero tripwire contacts, and complete cleanup. On failure, raw headers,
requests, responses, and logs are erased; only an allowlisted boolean/count
evidence projection remains.

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

## Live MySQL declarative apply test

Verify MySQL reconciliation independently against the pinned local instance:

```bash
scripts/integration/test-mysql-apply.sh
```

The check creates a disposable project, environment, and undeployed MySQL
record from two environment-backed one-shot secrets. It requires a fresh no-op
plan after creation, then rotates the user-password source and proves that the
read-only plan is blocked as an unsupported mutation without contacting a
mutation endpoint. After restoring the source, it requires another no-op plan,
updates the database and username in place, and proves convergence again.

The final apply removes the MySQL record. The check confirms absence through
both `mysql.one` and the environment-scoped `mysql.search`, removes the durable
state entry, and requires a final no-op plan. Before cleanup, it scans the
configuration, plans, diagnostics, state, and journals for the original user
password, root password, and rejected rotation value. The script verifies the
exact Dokploy `v0.30.6` image digest, never deploys the database, and removes
only its collision-resistant project and owner-only temporary workspace.

## Live MariaDB declarative apply test

Verify MariaDB reconciliation independently against the pinned local instance:

```bash
scripts/integration/test-mariadb-apply.sh
```

The check creates a disposable project, environment, and undeployed MariaDB
record from one environment-backed user password, deliberately omitting the
optional root password. It requires a fresh no-op plan, then proves that both a
changed user-password source and a newly supplied root-password source are
blocked during read-only planning without contacting a mutation endpoint.
After restoring the original configuration, it updates the database and
username in place and proves convergence again.

The final apply removes the MariaDB record. The check confirms absence through
both `mariadb.one` and the environment-scoped `mariadb.search`, removes the
durable state entry, and requires a final no-op plan. Before cleanup, it scans
configuration, plans, diagnostics, state, and journals for the original user
password and both rejected secret values. The script verifies the exact
Dokploy `v0.30.6` image digest, never deploys the database, and removes only its
collision-resistant project and owner-only temporary workspace.

## Live MongoDB declarative apply test

Verify MongoDB reconciliation independently against the pinned local instance:

```bash
scripts/integration/test-mongo-apply.sh
```

The check creates a disposable project, environment, and undeployed MongoDB
record from an environment-backed one-shot password. It requires a fresh no-op
plan, changes the password source, and proves that read-only planning blocks
the unsupported mutation without contacting a mutation endpoint. After
restoring the source, it updates username and replica-set mode in place and
proves convergence again.

The final apply removes the MongoDB record. The check confirms absence through
both `mongo.one` and the environment-scoped `mongo.search`, removes the durable
state entry, and requires a final no-op plan. It scans all captured command
streams, configuration, state, and journals for the original and rejected
passwords. Cleanup verifies project and database absence, retains owner-only
evidence if any proof fails, and never deploys the database.

## Live LibSQL declarative apply test

Verify LibSQL reconciliation independently against the pinned local instance:

```bash
scripts/integration/test-libsql-apply.sh
```

The check creates a disposable project, environment, and undeployed primary
LibSQL record from an environment-backed one-shot password. It requires a
fresh no-op plan, updates description and username in place, rotates the
password through a separate journaled mutation, and proves convergence after
each step.

It then changes the atomic node to a replica, verifies delete-before-create
replacement with a new physical identity, and proves that the old identity is
absent. The final apply removes the replica. The check confirms absence through
both `libsql.one` and the exact environment in `project.one`, removes the
durable state entry, and requires a final no-op plan. It scans every captured
command stream, configuration, state file, and journal for both passwords and
the ephemeral fingerprint key. Because `libsql.one` returns the database
password, direct proof responses are reduced atomically to the allowlisted
identity, node, and status fields before they can become retained evidence.
Cleanup verifies project and database absence, retains owner-only evidence if
any proof fails, and never deploys the database.

## Live Compose declarative apply test

Verify Compose reconciliation independently against the pinned local instance:

```bash
scripts/integration/test-compose-apply.sh
```

The check creates a disposable project, environment, and undeployed raw
Compose record from an environment-backed one-shot document. It requires an
immediate no-op plan, then updates the nullable description and opaque document
in one journaled apply and proves convergence again. No provider, refresh
token, deployment, environment values, or server selector are owned.

The final apply removes the Compose record with the declarative
preserve-volume policy. The check confirms absence through both `compose.one`
and the bounded environment-scoped `compose.search`, removes the durable state
entry, and requires a final no-op plan. Direct and collection responses can
echo the raw document, so each private response is validated and atomically
replaced with an allowlisted identity, containment, metadata, and deployment
status projection. Every command stream, configuration, state file, journal,
and retained response is scanned for both document canaries and the ephemeral
fingerprint key, while cleanup also excludes the real integration API key.
Cleanup proves project and Compose identity absence and keeps only owner-safe
evidence whenever a proof fails.

## Live Port declarative apply test

Verify Port reconciliation independently against the pinned local instance:

```bash
scripts/integration/test-port-apply.sh
```

The check creates a disposable project, environment, and two undeployed
applications with one Port under the first application. It requires an
immediate no-op plan, replaces all four owned fields in one apply while proving
the physical identity is unchanged, and then moves the Port under the second
application to prove delete-before-create replacement, a new identity, absence
of the old identity, and an empty former parent collection.

The next apply removes the Port declaratively. The check confirms absence
through `port.one` (which reports HTTP 400 for a missing Port) and the
authoritative `application.one` collection, removes the durable state entry,
and requires a no-op plan. It then creates a Port out of band, adopts it through
protected import into a separate workspace, and requires an immediate no-op
plan. Every touched application must report zero deployments. Raw responses stay
in a private directory that cleanup discards, retained evidence is reduced to an
allowlist and scanned for the integration API key, and cleanup proves project
and every observed Port identity absent.

## Live Redirect declarative apply test

Verify Redirect reconciliation independently against the pinned local instance:

```bash
scripts/integration/test-redirect-apply.sh
```

The check creates a disposable project, environment, and two undeployed
applications with one Redirect under the first application. It requires an
immediate no-op plan, replaces all three owned fields in one apply while proving
the physical identity is unchanged, and then moves the Redirect under the second
application to prove delete-before-create replacement, a new identity, absence
of the old identity, and an empty former parent collection.

The next apply removes the Redirect declaratively. The check confirms absence
through `redirects.one` (HTTP 404) and the authoritative `application.one`
collection, removes the durable state entry, and requires a no-op plan. It then
creates a Redirect out of band, adopts it through protected import into a
separate workspace, and requires an immediate no-op plan. Every touched
application must report zero deployments. Raw responses stay in a private
directory that cleanup discards, retained evidence is reduced to an allowlist
and scanned for the integration API key, and cleanup proves project and every
observed Redirect identity absent.

## Live Security declarative apply test

Verify basic-auth reconciliation independently against the pinned local
instance:

```bash
scripts/integration/test-security-apply.sh
scripts/integration/test-security-evidence-sanitization.sh
```

The apply check follows the Redirect lifecycle with one entry whose password
comes from a private file descriptor: undeployed create, no-op plan, a username
and password rotation that proves the exact credentials privately, containment
replacement, declarative deletion with `security.one` (HTTP 404) and collection
absence, and protected import of an out-of-band entry that leaves the password
unmanaged. Every touched application must report zero deployments. Generated
password canaries and the integration API key are scanned across all retained
evidence, state, journals, and plan output; cleanup proves project and every
observed identity absent and discards the private directory. The sanitization
self-test proves the scanner fails on a seeded password canary and API key.

## Live Mount declarative apply test

Verify Mount reconciliation independently against the pinned local instance:

```bash
scripts/integration/test-mount-apply.sh
scripts/integration/test-mount-evidence-sanitization.sh
```

The apply check creates a disposable project, environment, two undeployed
applications, and an undeployed raw Compose service, then three Mounts: a
volume Mount on the first application, a volume Mount on the Compose service,
and a file Mount whose content comes from a private file descriptor. It
requires an immediate no-op plan and proves each exact record, the exact remote
file content, and each target's authoritative `mounts.listByServiceId`
collection.

One apply then edits the volume path in place and rotates the file content
while proving every physical identity is unchanged. Changing the target
application and then the storage type (volume to bind) each prove
delete-before-create replacement, a new identity, and absence of the old one
through `mounts.one` (HTTP 404). A declarative removal proves identity and
collection absence, removes the durable state entry, and requires a no-op plan.
The check then creates a Mount out of band, adopts it through protected import
into a separate workspace, and requires an immediate no-op plan. Every touched
application and the Compose service must report zero deployments. Raw responses
and the content files stay in a private directory that cleanup discards;
retained evidence is reduced to an allowlist, and generated content canaries and
the integration API key are scanned across all retained evidence, state,
journals, and plan output. Cleanup proves the project and every observed Mount
identity absent. The sanitization self-test proves the scanner fails on a
seeded content canary and API key.

## Live external selector resolution test

Verify selector resolution and saved-plan binding against the pinned local
instance:

```bash
scripts/integration/test-external-selectors-apply.sh
```

The Dokploy instance is shared, so serialize runs that share it with
`.integration/live.lock`. Set `DOKPLOY_CARGO_CONFIG` to a Cargo configuration
file to keep workspace artifacts private when several worktrees share one
`target` directory.

The check creates disposable servers (deploy and build), registries (including
two with one name), and a backup destination. Servers and the destination point
at a connection tripwire. `registry.create` runs `docker login` before it stores
a registry, so registries point at a loopback `/v2/` responder; the run asserts
that it saw only `GET /v2/` login probes and that the probe count never changes
after the records exist. It then resolves the local selector and the named
server, registry, and destination records, exact-name mismatches, and the
ambiguous name through the real selector seam, and drives the declarative
engine: an application created with a resolved server and build-server, registry
associations updated and cleared in place, unmatched and ambiguous selectors
blocking the plan and apply, a saved plan that applies while its resolution is
unchanged and is refused after the selected registry is re-created under the
same name, delete-before-create replacement on a changed server, the explicit
local selector, and import that writes name selectors and fails closed on an
ambiguous association. Every touched application must stay idle with zero
deployments and the tripwire must record no connection. Saved-plan envelopes
and imported workspaces must contain no external identity or selector name.
Raw responses stay in a private directory that cleanup discards; cleanup removes
the project and every record carrying the run prefix and proves their absence.
Retained evidence is scanned for the API key, the fingerprint key, and every
secret and identity canary.

## External selector contract capture

Capture the read-only server, registry, and backup-destination selector
contracts against the pinned local instance with:

```bash
scripts/integration/capture-external-selectors-contract.sh
```

The command performs only `server.all`, `registry.all`, and `destination.all`
reads. It validates the array envelopes, required public fields, 10,000-item
limit, and unique physical IDs. Raw responses can contain server commands,
metrics tokens, registry credentials, and destination access keys, so they
remain in owner-only ignored storage. Published fixtures contain only
normalized IDs, names, and server types; duplicate names remain duplicate so
the later declarative resolver can test ambiguity. The complete candidate
fixture tree passes the secret checker before rollback-safe publication.

Exercise the public SDK against the same three inert reads with:

```bash
scripts/integration/test-external-selectors-sdk.sh
```

The wrapper requires Dokploy `v0.30.6` and performs no creates, updates,
deletes, deployments, or executions.
