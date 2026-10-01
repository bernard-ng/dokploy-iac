# ADR 0036: External selector SDK contract

## Status

Accepted. Implements the read boundary anticipated by ADR 0026.

## Context

Dokploy servers, container registries, and backup destinations are external
infrastructure. The IaC workspace must resolve stable human selectors against
fresh remote state without owning these records or importing their physical
identities into desired configuration.

The pinned `v0.30.6` OpenAPI document declares generic success bodies for
`server.all`, `registry.all`, and `destination.all`. Runtime rows can contain
server commands and metrics tokens, registry credentials, and destination
access or secret keys. Deserializing whole rows into public models or remote
errors would allow those values to reach snapshots, diagnostics, and debug
output. Unbounded or internally ambiguous collections would also make selector
resolution unsafe.

## Decision

The handwritten SDK exposes three read-only services backed by the exact
`server.all`, `registry.all`, and `destination.all` operations. Their public
rows contain only the fields needed for later selector resolution:

- servers expose `server_id`, `name`, and `server_type`;
- registries expose `registry_id` and `registry_name`;
- destinations expose `destination_id` and `name`.

Every physical identity has its own strong SDK type. Response models ignore
unknown fields and therefore never retain server command or metrics-token
fields, registry credentials, or destination access and secret keys. The
shared 16 MiB zeroizing response reader remains the byte-level limit. These
secret-bearing collections use status-only errors for non-success responses so
an echoed credential cannot enter `DokployError` or its debug representation.

Each collection accepts at most 10,000 rows. Empty required values and
duplicate physical IDs fail closed as an unexpected response. Duplicate names
remain intact because name ambiguity is a declarative resolver concern: a
later exact-name selector must reject zero or multiple matches rather than let
the SDK silently choose one.

Application observations expose the primary server, build server, runtime
registry, build registry, and rollback registry as minimal typed identities.
Compose, PostgreSQL, MySQL, MariaDB, MongoDB, LibSQL, and Redis observations
expose their primary server where the pinned runtime response proves the
field. Every association uses `ResponseField` so an omitted field remains
distinguishable from an explicit local or cleared `null`. Nested server and
registry records are ignored and cannot expand the public secret surface.

Supported create inputs keep server placement unset by default, which omits
`serverId` and leaves placement unmanaged. An explicit `ServerPlacement::Local`
sends JSON `null`; `ServerPlacement::Server(id)` sends the resolved physical
identity. The SDK does not infer deploy or build roles and does not couple a
registry to a server without separate live evidence. A successful create whose
managed placement is missing or contradictory in the response is
`OutcomeUnknown`, because the server may already have accepted the mutation.
The application update contract exposes only the four nullable associations
declared by the pinned endpoint: build server, runtime registry, build registry,
and rollback registry. Primary server reassignment remains unsupported.

This presence-aware observation contract intentionally changes the public SDK
source shape during the pre-release `0.1.0` series. Callers migrating from
`Option<ServerId>` must map the former `None` case to either
`ResponseField::NotReturned` or `ResponseField::Null`, rather than collapsing
those states, and map `Some(id)` to `ResponseField::Value(id)`. A lossy
compatibility accessor is not provided because it would undermine fail-closed
planning.

Live contract capture is inert. It verifies the pinned version, HTTP status,
array envelope, required field types, item limit, and unique physical IDs,
then publishes only normalized projections of the public fields. The complete
candidate fixture tree is checked before rollback-safe replacement. Raw
responses stay in owner-only ignored storage and successful captures remove
that private evidence.

## Consequences

- Declarative code can resolve external infrastructure without depending on
  generated `serde_json::Value` responses.
- Future unrelated upstream fields remain compatible but cannot expand the
  public secret surface accidentally.
- A malformed, duplicate-ID, or excessive collection blocks planning before
  any selector is chosen.
- Same-name records are observable, enabling a resolver to report ambiguity
  instead of selecting by response order.
- Association reads preserve omitted, null, and concrete identities without
  retaining nested server or registry records.
- Managed create placement requires exact post-acceptance proof; missing or
  conflicting proof requires recovery instead of authorizing a retry.
- External lifecycle mutation remains outside the SDK services and Phase 8
  ownership boundary.
