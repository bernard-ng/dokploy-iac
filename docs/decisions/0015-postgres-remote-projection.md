# ADR 0015: Parent-scoped Postgres remote projection

## Status

Accepted for the Postgres Phase 5 checkpoint.

## Context

A managed Postgres database has an opaque Dokploy identity and a containing
environment. A new desired database has no physical identity, so only an exact
name search inside its intended physical environment can detect a collision.
The pinned OpenAPI document declares empty success objects for `postgres.one`
and `postgres.search`; it cannot prove identity, containment, field presence,
pagination, or omission of database credentials and runtime environment data.

The runtime search operation returns `{items,total}` pages. One page is not
absence evidence, and an unstable total or early empty page cannot safely prove
that a name is unused. The planner also has no Postgres reparent action. A
dependency-only checkpoint must not represent moving a database between two
physical environments.

## Decision

The CLI composition layer extends combined discovery with an independent
Postgres collection-authority assertion. Every invocation performs fresh reads
and keeps no cache.

The SDK exposes `postgres().by_environment(id)` as a fully collected read. The
module requests pages of at most 100 entries, caps a collection at 10,000
entries, requires a stable total, and rejects oversized, over-total, or
prematurely empty pages. Its handwritten search item retains only Postgres ID,
environment ID, and name. The direct model ignores database passwords,
environment documents, and unknown fields. Database name and database user are
presence-aware so omission, null, and a concrete string remain distinct.

Managed databases bind only to their stored physical IDs. Direct reads must
return the requested identity and the environment recorded in durable state.
An authoritative search and a successful direct read must agree on identity
and name. A direct `404` is checked against the current parent collection; the
same stored ID is an endpoint contradiction, while a different same-name ID is
projected as a replacement collision for planner identity validation.

Unmanaged databases and move targets use an exact name only inside a proved,
state-backed environment. Search results must belong to the requested parent.
Duplicate global IDs, duplicate scoped names, invalid IDs, malformed paging,
and containment conflicts fail closed. Absence becomes `Missing` only when the
parent is proved present and the caller asserted an authoritative Postgres
collection. A partial or failed collection remains unavailable.

Only requested, nonignored database and username paths are projected from a
managed direct read. Every requested password path is projected as
`Unknown(Sensitive)`; neither remote reads nor debug output retain password or
environment bytes. This supports receipt-based write-only planning without
claiming knowledge of the remote password.

A logical Postgres move is safe when both addresses resolve to the same
physical environment. A simultaneous logical environment move is also safe
because the target resolves through the stored source identity. Any desired
Postgres dependency change that resolves to another physical environment is
rejected with a dedicated discovery error before a Postgres request. It remains
unsupported until the planner and executor have an explicit, recoverable
Postgres reparent action.

## Consequences

- Project, environment, application, and Postgres observations share one
  instance-bound snapshot.
- Postgres creates, same-parent logical moves, updates, retains, removals, and
  idempotent persisted removals can be planned without exposing a mutation
  interface.
- Parent-scoped pagination and incomplete runtime schemas stay behind the SDK
  interface.
- Password drift is never inferred from a readable value; durable intent
  receipts remain the only comparison input.
- Dokploy `v0.30.6` does not establish a safe password-clear contract. A future
  executor must fail closed rather than infer clear support from the
  configuration or discovery model.
- Physical Postgres reparenting requires a later planner action, mutation
  contract, journal sequence, and convergence test.
