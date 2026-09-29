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
parent-scoped application and Postgres search, and normalization of Dokploy's
sparse authentication error response.

## Live CLI redaction test

Verify that imperative command output does not expose the database password or
other secret-bearing fields returned by the live API:

```bash
scripts/integration/test-cli-redaction.sh
```

The wrapper resolves the ignored local API key and a disposable Postgres
fixture, invokes the public CLI, and fails if the known fixture secret appears
in either standard output or diagnostics.
