# ADR 0011: Parent-scoped environment remote projection

## Status

Accepted for the project-and-environment Phase 5 checkpoint.

## Context

Environment identity is scoped by a containing project. A stored environment
has an opaque Dokploy ID, while a new desired environment can only be checked
for a collision by its exact name inside its parent project. Dokploy's pinned
OpenAPI document declares empty success objects for the environment reads, so
generated response types cannot establish identity, containment, description
presence, or safe omission of the environment-level `env` field.

The `environment.byProjectId` runtime operation returns an array, but an
unknown project also returns an empty array. Its absence evidence is therefore
valid only when fresh project discovery has already proved the parent present
and the caller has asserted that the environment collection is authoritative.
The `environment.search` operation is paginated and performs non-exact name
matching, so it cannot serve as a collision oracle.

## Decision

The CLI composition layer exposes one combined, read-only discovery interface
for projects and environments. It accepts separate project and environment
authority assertions and returns one core `RemoteState`. Kind-specific
projection stays behind that interface so callers never merge independently
built snapshots or reproduce parent-resolution rules.

Every invocation performs a fresh `project.all` read. For every relevant,
proved-present parent, it reads `environment.byProjectId` once. Stored
environments are also read through `environment.one` by physical ID. Relevant
addresses include desired and stored environments, both ends of moves, and
state-backed removals. Project and environment addresses are filtered by their
own adapters so a mixed desired configuration remains valid input.

Managed environments bind only to their stored physical ID. A direct response
must return the requested ID and the physical ID of the expected parent. A
different parent, an invalid ID, duplicate IDs, or duplicate names within one
parent fails closed. If a managed ID returns `404`, the parent collection is
used as a same-name replacement probe. A replacement is projected with its own
ID so the planner emits an identity mismatch rather than adopting it.

Unmanaged environments and move targets use an exact name lookup inside the
resolved physical parent. Parent project moves resolve through their stored
source identity, which lets project and environment moves be planned in the
same snapshot. A same-name replacement project never qualifies as that parent,
and its environment collection is not queried. Absence becomes `Missing` only
when the parent is proved present by its stored identity and its collection is
authoritative. Partial or unavailable collections yield a closed unavailable
observation. A proved-missing parent also proves an unmanaged child missing.

The SDK owns narrow handwritten `EnvironmentDetails`, `EnvironmentSummary`,
and `EnvironmentCollection` models. They include only identity, name,
containment where returned, and a presence-aware description. Environment
variables, child collections, relation objects, status fields, and timestamps
are ignored. An owned description maps omitted to `Unknown(NotReturned)`, JSON
null to `KnownAbsent`, and a string to `Known`. Unowned and ignored descriptions
are not projected.

Authentication failures map to `Unauthorized`. Exhausted transport, `408`,
`425`, `429`, and server failures map to `Unavailable`. Decode failures and
other unexpected responses map to `InvalidResponse`. Direct environment `404`
is handled as missing physical identity, subject to the replacement probe.

## Consequences

- Project and environment planning share one instance-bound, fresh snapshot.
- The adapter can safely plan environment creates, updates, moves, retains, and
  destroys without exposing mutations.
- Callers must establish collection authority separately for projects and
  environments.
- Environment display-name drift remains unmanaged because the current planner
  vocabulary uses names for lookup identity, not as an owned property.
- Stored containment temporarily relies on the canonical project dependency in
  state. A future multi-project state format should persist containment as a
  dedicated field.
- Dokploy `v0.30.6` does not prove that `environment.update` accepts a null
  description, so execution-time description clearing remains fail-closed work
  for Phase 6.
