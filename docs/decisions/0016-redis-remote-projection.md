# ADR 0016: Parent-scoped Redis remote projection

## Status

Accepted for the Redis Phase 5 checkpoint.

## Context

A managed Redis database has an opaque Dokploy identity and a containing
environment. A new desired database has no physical identity, so only an exact
name search inside its intended physical environment can detect a collision.
The pinned OpenAPI document does not describe the runtime response bodies for
`redis.one` or `redis.search`.

Sanitized Dokploy `v0.30.6` evidence proves that direct reads expose identity,
containment, and metadata together with a generated password and environment
data. Search returns `{items,total}` pages. One page is not absence evidence,
and an unstable total or incomplete page cannot safely prove that a name is
unused. Although the upstream operation catalog contains `redis.move`, its
mutation contract and recovery semantics are not yet proven for the executor.

## Decision

The CLI composition layer extends combined discovery with an independent Redis
collection-authority assertion. Every invocation performs fresh reads and
keeps no cache.

The SDK exposes `redis().get(id)` and `redis().by_environment(id)`. The latter
fully collects pages of at most 100 entries, caps the collection at 10,000,
requires a stable total, and rejects oversized, over-total, or prematurely
empty pages. Handwritten direct and search models retain only safe identity,
containment, name, and operational metadata. Password,
environment, mounts, and nested runtime documents never cross the SDK seam.

Managed databases bind only to their stored physical IDs. Direct reads must
return the requested identity and current stored environment. An authoritative
search and successful direct read must agree on identity and name. A direct
`404` is checked against every collected parent: the same stored identity is an
endpoint contradiction, while a different exact-name identity is projected as
a replacement collision for planner validation.

Unmanaged databases and move targets use exact names only inside proved,
state-backed environments. Search items must belong to the requested parent.
Duplicate global IDs, duplicate scoped names, invalid IDs, malformed paging,
and containment conflicts fail closed. Absence becomes `Missing` only when the
parent is proved present and the caller asserted authoritative Redis search.

The only Redis-owned MVP property is the password. Every requested,
nonignored password is projected as `Unknown(Sensitive)`. Discovery never
claims to read, compare, or clear the remote password; durable intent receipts
remain the comparison input.

Logical Redis moves are safe when source and target resolve to the same
physical environment. Simultaneous environment moves are safe because the
target resolves through the stored source identity. Any desired Redis
containment change that resolves to another physical environment fails before
Redis transport until an explicit mutation contract, journal sequence, and
recovery test exist.

## Consequences

- Project, environment, application, Postgres, and Redis observations share one
  instance-bound snapshot.
- Redis creates, same-parent logical moves, retains, removals, and persisted
  removals can be planned without admitting mutation code to the planner.
- Password and runtime environment bytes cannot enter snapshots, diagnostics,
  plans, state, or debug output through the Redis read model.
- Dokploy `v0.30.6` does not establish a safe password-clear contract. The
  executor must fail closed rather than infer one.
- Physical Redis reparenting remains deferred until Phase 6 proves the remote
  action and its recoverability.
