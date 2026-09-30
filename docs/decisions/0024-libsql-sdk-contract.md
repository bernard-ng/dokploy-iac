# ADR 0024: LibSQL SDK identity and mutation contract

## Status

Accepted. Extends Phase 8 resource breadth.

## Context

Dokploy `v0.30.6` has no `libsql.search` operation. `libsql.create` returns the
JSON boolean `true` instead of the created identity, while `libsql.one` requires
that identity. The project detail response is the only pinned operation that
lists LibSQL records: each environment embeds a `libsql` collection.

The create request accepts an application name, but Dokploy appends a generated
suffix before storing it. The caller-provided resource name remains stable.
Detail responses also expose `databasePassword`, which is the self-hosted
LibSQL authentication credential, alongside environment and runtime data.

## Decision

The SDK treats `project.one` as the authoritative LibSQL discovery boundary.
Every collection read requires both `ProjectId` and `EnvironmentId`, verifies
the returned project identity, requires exactly one matching environment, and
rejects collections above the local safety bound.

Create retains the project identity as discovery context without serializing it
to `libsql.create`. It first proves that no record has the requested name in the
exact environment. After the boolean create response, it reads the same
topology again and returns a strong `LibSqlId` only when exactly one record has
that name. Existing collisions and missing or ambiguous post-create identities
fail closed. The generated application name is not used for recovery because
Dokploy changes it.

Safe reads omit the password/authentication token, raw environment document,
mounts, and other unowned runtime data. `UpdateLibSql` owns only the username
and description fields proven to persist on the pinned live instance. A
separate write-only `ChangeLibSqlPassword` operation sends `databasePassword`
through `libsql.update`; the pinned API exposes no separate token endpoint.
Create supports the required primary/replica node distinction, namespace
selection, and optional server association.

## Consequences

- LibSQL creation costs two additional project topology reads but never guesses
  an identity from a broad or stale collection.
- Callers must provide a project identity when listing or creating LibSQL.
- A concurrent same-name create can make post-create discovery ambiguous; the
  adapter reports an unrecognized result instead of selecting either record.
- The live capture proves credential persistence through private raw evidence,
  then publishes only sanitized fixtures. It never deploys the database.
