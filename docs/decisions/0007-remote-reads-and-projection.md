# ADR 0007: Remote reads and tolerant projection

## Status

Proposed. Keeps the first engine's invariants 1 to 4 and its ADR 0035 (bounded
response bodies) and 0037 (sanitised error bodies).

## Context

Dokploy's OpenAPI describes every response as an empty object, so generated
response types are useless for reconciliation, and `getWebServerSettings` and the
`tag.*` reads have no typed response at all. Collections can be role-filtered.
Fields appear and disappear between versions. A create often returns the new
object, sometimes only a boolean, sometimes nothing useful.

## Decision

### Projection

A **projection** maps a raw JSON response to a property map using the spec's
field table: each field names a JSON pointer in the response (defaulting to its
API name). Reading is tolerant and presence-aware:

- unknown response fields are ignored (and flagged by the ledger in CI, ADR 0002);
- a missing field is `NotReturned`, not `null`; `null` is a value;
- a value outside an `enum` reads as *unknown* for that property, which blocks
  planning for it without failing the whole resource;
- secret fields are never retained: the projector drops `secret` and `write_only`
  values at the boundary, and the transport's sanitising still applies to errors.

Projections are generic. A hook may post-process a projected resource.

### Authority

For each kind the spec states how absence is proven:

- `list` (authoritative collection) with an optional `scope` (by environment, by
  project, by service, by server);
- `one` (direct read);
- `agree: true` requires the direct read and the collection item to agree on
  identity and on every shared field, as before. Disagreement is an unavailable
  observation, not a guess.

A collection that may be filtered by role is flagged `authority: partial`;
absence from a partial collection is *unknown*, not *missing* (as the tag
collection already is).

### Discovery

Discovery is driven by the union of desired addresses, state addresses, and
removal directives. For each kind present it issues the minimum reads: one list
per (kind, scope) shared by all addresses of that kind, then direct reads only for
resources that matched. Nothing is read for a kind the document and state do not
mention. A failed read yields `Unavailable` for exactly the addresses it covers,
which blocks their planning and nothing else.

### Fixtures

Every read operation has a recorded live fixture per supported Dokploy version,
captured by a capture tool (the successor of `scripts/integration/capture-*-contract.sh`), sanitised by a fixed filter, and
checked for leaked secrets. Fixtures are never hand-edited. They feed the ledger,
the projection tests, and the simulator's response shapes (ADR 0015).

## Consequences

- No hand-written response models for the IaC path; the 6,000-line SDK model file
  and the CLI's 6,000-line remote module are retired (ADR 0016).
- Adding a field to a kind updates a spec and a fixture, not a struct.
- A new Dokploy minor that changes a response shape fails fixture checks rather
  than silently misreading.
