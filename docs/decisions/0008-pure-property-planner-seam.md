# ADR 0008: Pure property planner seam

## Status

Accepted for the first Phase 5 checkpoint.

## Context

Resource-wide JSON equality cannot preserve field ownership. An omitted field
must relinquish management without restoring or reporting remote drift, while
an explicit null remains an owned instruction to clear the remote property.
Remote reads can also be partial or intentionally hide sensitive values.

The planner must remain independent from configuration parsing, Dokploy API
types, network access, and mutation code.

## Decision

`dokploy-core` owns a finite, path-capable MVP property vocabulary and receives
three typed, in-memory snapshots. Stable path names match configuration paths,
including `source.repository`, `source.branch`, and independently validated
`environment.NAME` entries. Desired and stored snapshots represent ownership
by path presence and distinguish omission, explicit null, an owned-empty
environment, opaque non-sensitive values, and value-free sensitive intent.
Collection roots are reserved for source clears and environment clear or empty
intent; a root cannot coexist with one of its child paths.
An owned source is either the root clear or a non-null repository with an
optional null or non-null branch. Desired and stored snapshots reject a branch
without that repository. Remote snapshots reject contradictory known child
observations while retaining explicit unknown observations for partial reads.

Remote resources contain a separate observation for each path: known value,
known absence, or a closed unknown reason. Write-only password and environment
entry paths reject known values and accept only known absence or the sensitive
unknown reason. Desired, stored, and remote constructors validate paths against
the resource kind. Desired construction also rejects self-dependencies and
dependencies absent from the desired snapshot.

Planning is a pure three-way comparison. Missing or unknown observations for
desired properties make the plan incomplete. Property values and remote IDs
remain opaque and never enter plan JSON, debug output, drift metadata, or
diagnostics. Omitting a previously owned property creates a state-only
checkpoint and neither mutates nor reports drift for that property.

Every planned change carries an immutable checkpoint target computed during
planning. A present target contains effective protection, canonical
dependencies, exact non-sensitive owned properties, and only value-free intent
for sensitive properties. Delete and forget changes target absence. Targets
are available through a sensitivity-aware accessor but are excluded from plan
JSON and redacted from debug output; a future executor must not reconstruct
them from field-change metadata.

Stored protection is an effective boolean, not a separate ownership record.
Desired unmanaged protection preserves that value. Explicit true or false
replaces it when the effective value changes. Explicit false over an already
false stored value needs no checkpoint because the persisted outcome is
identical. Deletion always consults the stored effective value.

Physical identity is the pair of resource kind and Dokploy remote ID. Stored
and remote projections reject duplicate identities within a kind, while the
same raw identifier may appear in different resource kinds.

Moves require a managed source and desired target of the same resource kind.
A pending move also requires a conclusive present source observation with the
stored remote identity and a separate explicit missing observation for the
target address. The target probe is part of the remote adapter contract; a
missing probe never means absence. A move is addressed to the target, exposes
its previous address and whether remote property work is required, and carries
an atomic checkpoint that removes the source while writing the exact target.
If the target is already stored and the source is absent, the declaration is
idempotently satisfied and the target plans normally. Pending move sources do
not enter the stored-removal graph.

Removal declarations are also idempotent once their address is absent from
stored state. Retain removes a conclusively identified present resource from
management without reading its properties or consulting protection. Destroy
uses the normal identity and stored-protection checks; property observations
are best-effort drift evidence and cannot prevent an otherwise safe delete.
An already missing managed resource is forgotten for either policy. Invalid,
conflicting, chained, or ambiguous directives block the whole plan rather than
degrading into default create or delete actions.

Ignored changes and replacement metadata remain retained in desired input but
produce typed blocking diagnostics until their dedicated planner slices exist.

Dependency ordering uses `petgraph` behind the planner seam. Desired-resource
edges order create, recreate, update, and no-op checkpoint actions
dependency-first. Stored dependencies among resources leaving desired state
order delete and forget actions dependent-first. Independent ready resources
use lexical address order.

Mixed plans use two stable phases: every desired-resource action completes in
dependency order before removal actions begin in reverse stored-dependency
order. This conservative policy avoids destroying old resources while desired
resources are still converging. No later lexical sort replaces the graph
order. A cycle in either relevant graph emits typed diagnostics for its members,
blocks the plan, and suppresses all change ordering rather than presenting a
misleading partial execution sequence.

## Consequences

- Remote adapters must provide explicit observations for every desired
  property path they claim to support.
- Durable managed-input keys outside the finite MVP vocabulary fail closed
  through a redaction-safe projection error; this avoids guessing how legacy
  or future state should be interpreted.
- State-only protection, dependency, and ownership changes are represented as
  no-op remote actions that a later executor must checkpoint.
- Executing a move requires future atomic `StateFile` and journal support for
  removing the source and writing the target as one recoverable transition.
  This checkpoint defines that target but adds no executor behavior.
- Dependency cycles are planning diagnostics, not desired-state construction
  errors; missing and self-dependencies still fail at the desired-state seam.
- Adding a property path requires an explicit planner vocabulary, kind
  validation, state projection, and adapter update.
- Ignored changes and replacement behavior remain later Phase 5 checkpoints.
