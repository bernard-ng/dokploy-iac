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
- External lifecycle mutation remains outside the SDK services and Phase 8
  ownership boundary.
