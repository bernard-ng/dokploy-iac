# ADR 0014: Parent-scoped application remote projection

## Status

Accepted for the application Phase 5 checkpoint.

## Context

Application identity is scoped by a containing environment. Durable state binds
a managed application to an opaque Dokploy ID, while an unmanaged application
can only be checked for a collision by exact name inside its intended physical
environment. The pinned OpenAPI document declares empty success objects for
`application.one` and `application.search`, so generated response types cannot
prove identity, containment, pagination, field presence, or safe treatment of
the secret-bearing `env` field.

The runtime search operation returns `{items,total}` pages and accepts an
environment filter. One page is not absence evidence. Totals can also change
between pages, or a malformed server can return an empty page before the
declared total. Application absence is therefore valid only after a bounded,
stable, exhaustive search below an environment whose physical identity was
already proved by combined project and environment discovery.

## Decision

The CLI composition layer extends its single read-only discovery interface to
projects, environments, and applications. Callers provide an independent
application-collection authority assertion and receive one core `RemoteState`.
Every invocation performs fresh reads and retains no cache.

The SDK exposes `applications().by_environment(id)` rather than raw paging.
That deep module requests pages of at most 100 entries, caps the result at
10,000 entries, requires a stable total, and rejects over-total, oversized, or
prematurely empty pages. The application adapter rejects duplicate global IDs,
duplicate exact names inside one environment, cross-environment search items,
invalid IDs, and direct containment mismatches.

Managed applications bind only to the stored physical ID and validate the
environment recorded in durable dependencies. This remains the current
containment contract during same-address reparenting and simultaneous logical
moves. A desired reparent target is separately probed when it resolves to a
different trusted physical environment; an exact-name collision is projected
with its own ID so the planner blocks adoption. Environment move targets
resolve through the stored source identity. Replacement environment IDs never
become trusted parents.

Unmanaged applications and move targets use exact-name lookup only inside a
proved, state-backed parent environment. A direct `404` on a managed ID uses
the current parent collection as a replacement probe and, during same-address
reparenting, also probes the desired trusted parent. Search returning the same
stored ID after the direct `404` is an endpoint inconsistency, not proof of
presence. A different-ID collision in either parent is conclusive under either
authority; without one, any unresolved probe fails closed. Exhaustive absence
becomes `Missing` only when every relevant parent is proved and its collection
is authoritative. A proved-missing parent proves an unmanaged child missing.

The handwritten `ApplicationDetails` model preserves omitted, null, and value
states for description, replicas, source type, repository, branch, and build
path. Build path remains deliberately unprojected because the current
configuration and planner have no path for it. An owned source-root clear is
known only when source type is explicitly null; omission remains unknown.
Owned GitHub repository and branch observations are accepted only when the
presence-aware source type is exactly `github`. Another type, null, or an
omitted selector fails closed rather than comparing stale provider fields.

Application environment bytes never deserialize into a string retained by the
model. A custom value-free shape records only null, empty, or opaque. Owned
environment roots project that shape without values. A null or empty root
conclusively proves each requested child absent, an opaque root projects each
child as `Unknown(Sensitive)`, and an omitted root fails closed as not returned.
Unrequested and ignored properties are not projected.

Authentication failures map to `Unauthorized`. Exhausted transport, `408`,
`425`, `429`, and server failures map to `Unavailable`. Decode errors,
pagination-contract failures, and other unexpected responses map to
`InvalidResponse`.

## Consequences

- Application create, update, move, reparent, retain, and destroy planning now
  use the same fresh instance-bound snapshot as their parents.
- Search pagination and runtime response drift remain hidden behind one narrow
  SDK interface.
- Secret-bearing environment bytes cannot enter SDK debug output, remote
  snapshots, plans, state, or diagnostics through application reads.
- The 10,000-entry per-environment cap is a deliberate fail-closed bound and
  can be raised only with new performance and pagination evidence.
- Application display names and build paths remain unmanaged in this MVP.
- Mutation contracts and execution remain Phase 6 work.
