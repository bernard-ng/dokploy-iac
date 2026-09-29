# ADR 0009: Authoritative project remote projection

## Status

Accepted for the project-only Phase 5 checkpoint. ADR 0011 extends the public
discovery seam to mixed project and environment state and replaces the
non-project rejection with kind-specific filtering.

## Context

The pure planner cannot infer remote absence from a missing observation. It
requires an explicit `Missing` result for creates, move targets, and resources
removed from state. Dokploy's `project.all` response can support those probes,
but only when the caller has established that the returned topology is a
complete, authoritative view. A role-filtered response must not be interpreted
as proof that an unseen project does not exist.

Managed projects also have two identities. Their logical address is stable in
configuration, while Dokploy assigns an opaque physical ID. Matching a managed
project by name after it has been renamed can target the wrong remote object.
Conversely, an unmanaged desired project has no physical ID and must use its
exact logical name only as a collision probe.

## Decision

The CLI crate owns a project-only remote adapter at the composition seam. Its
single async interface accepts the configured SDK client, compiled desired
state, durable state, and an explicit topology-authority assertion. Every
invocation performs one fresh `project.all` operation. The adapter is read-only
and keeps no persistent cache.

The SDK exposes its normalized, credential-free API base URL without depending
on the state crate. Before transport, the adapter projects that URL into the
state domain's instance identity and compares it with the state lineage. A
mismatch fails with a stable error that contains neither URL. Remote
observations can therefore never be tagged with an unrelated state instance.

The adapter rejects every relevant non-project address before transport.
Relevant addresses include desired and stored resources plus both ends of
moves and every removal. Managed addresses bind to their stored Dokploy ID;
unmanaged desired addresses and move targets probe the exact logical project
name. When a managed ID has disappeared but its exact logical name is occupied
by another ID, the name lookup is an explicit collision probe: the replacement
is projected with its physical ID so the planner emits an identity mismatch and
never adopts it. Moves receive distinct source and target observations.
Removals remain observable after leaving desired state.

An absent match becomes `Missing` only for an authoritative topology. Under a
partial topology, absence becomes an unavailable observation with the closed
`InvalidResponse` reason. The SDK preserves response-field presence for project
descriptions: an omitted field maps to `Unknown(NotReturned)`, explicit JSON
null maps to `KnownAbsent`, and a string maps to `Known`. Unowned or ignored
descriptions are not projected. Physical IDs remain opaque and are passed to
the core remote snapshot without entering plan output.

Duplicate project names, duplicate project IDs, invalid IDs, and mappings that
assign one physical project to multiple logical addresses fail closed. SDK
authentication failures map to `Unauthorized`; exhausted transport and server
failures map to `Unavailable`. Exhausted transient HTTP `408`, `425`, and `429`
responses use the same unavailable classification as `5xx` responses.
Decoding, contract, and other unexpected responses map to `InvalidResponse`.
Transport values and response contents are never included in adapter errors.

## Consequences

- Callers must assert authoritative topology only after verifying that the
  active Dokploy credentials provide a complete project listing.
- A client configured for another instance is rejected before any request.
- The adapter can safely drive project creates, updates, moves, retains, and
  destroys without exposing a mutation interface.
- SDK-owned bounded GET retries remain inside one logical `project.all`
  operation; repeated adapter invocations always start a fresh operation.
- Environment and child-resource discovery require separate later adapters.
- Adding the `plan` command and applying mutations remain later checkpoints.
