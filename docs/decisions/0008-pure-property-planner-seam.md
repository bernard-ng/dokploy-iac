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
environment, opaque non-sensitive values, and opaque sensitive intent receipts.
Collection roots are reserved for source clears and environment clear or empty
intent; a root cannot coexist with one of its child paths.
An owned source is either the root clear or a non-null repository with an
optional null or non-null branch. Desired and stored snapshots reject a branch
without that repository. Remote snapshots reject contradictory known child
observations while retaining explicit unknown observations for partial reads.

Remote resources contain a separate observation for each path: known value,
known absence, or a closed unknown reason. Write-only password and environment
entry paths reject known values and accept known absence or a closed unknown
reason. Only the sensitive unknown reason is conclusive for write-only planning;
omission and invalid-response reasons remain blocking. Desired, stored, and
remote constructors validate paths against the resource kind. Desired
construction also rejects self-dependencies and dependencies absent from the
desired snapshot.

Planning is a pure three-way comparison. Missing observations and unknown
non-sensitive observations for desired properties make the plan incomplete.
The sensitive unknown reason is a valid write-only observation: durable and
desired fingerprints determine configuration change without claiming remote
value equality. Property values, fingerprints, key identifiers, and remote IDs
remain opaque and never enter plan JSON, debug output, drift metadata, or
diagnostics. Omitting a previously owned property creates a state-only
checkpoint and neither mutates nor reports drift for that property.

Every planned change carries an immutable checkpoint target computed during
planning. A present target contains effective protection, canonical
dependencies, exact non-sensitive owned properties, and opaque receipt identity
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

Ignored paths use previous managed state as the ownership baseline for an
existing resource. A path owned in stored state keeps that exact stored value
in the checkpoint; a path absent from stored ownership stays unmanaged even
when desired input declares it. Ignored paths require no remote property
observation and do not contribute field changes, drift, origins, convergence,
or state-only checkpoints. Initial create still uses desired input because no
previous baseline exists. Recreate after remote deletion uses stored values for
previously owned ignored paths and omits newly desired ignored paths.

Existing-resource update, no-op, and move entries expose ignored paths as a
redaction-safe write-exclusion set. A future executor must omit those paths
from mutation payloads and must not reconstruct a whole-object write from the
checkpoint. Pending moves use the source's stored baseline. Satisfied moves
plan the target normally. `deployment.status` is a computed write exclusion
and never enters managed inputs. Sensitive and structural-root ignore paths,
ignore/replacement overlaps, and ancestor/descendant selector conflicts fail
at the desired-state seam. An ignored baseline that would make durable source
ownership invalid blocks with a typed diagnostic.

Replacement metadata remains typed-blocking. Safe replacement requires
adapter-projected mutability and an explicit `ReplacementOrder` rather than a
global create/delete default. It also requires protection enforcement,
explicit move interaction, recoverable journal steps, and a checkpoint that
accepts the new physical identity. Until those inputs and executor semantics
exist, replacement cannot degrade into create, update, or move.

The CLI crate is the composition seam from validated `dokploy.yaml` models to
core desired state. One compiler maps all MVP resource variants, lifecycle
metadata, containment and reference dependencies, moves, and removals without
performing I/O. Its configuration digest is supplied by the caller; digest
derivation is a separate checkpoint.

Values needed only when execution eventually occurs live in a non-serializable
sidecar with fully redacted debug output. At this checkpoint the sidecar
retains direct logical parents and domain-to-application logical references.
The desired domain application property deliberately contains the canonical
logical address, never a Dokploy remote ID. Remote ID resolution remains an
adapter responsibility after discovery.

The planner accepts sensitive desired properties only through opaque intent
fingerprints. The configuration compiler cannot calculate those fingerprints
at this checkpoint, so it rejects every concrete application environment value
and every set Postgres or Redis password with `DOKCMP004`. Explicit clear and
unmanaged sensitive fields remain supported. ADR 0013 later adds a separate
instance-bound compiler seam while preserving this offline behavior.

Durable state format version 2 completes the persisted part of that convergence
contract. A non-null sensitive input is stored only as a version-one
HMAC-SHA-256 receipt containing a
canonical, non-nil UUID key identifier and a 32-byte MAC encoded as exactly 64
lowercase hexadecimal characters. The receipt serializes as `version`,
`keyId`, and `mac`, with `version` fixed to `hmac-sha256-v1`. Its Rust interface
does not expose the MAC or implement display, and debug output is fully
redacted. The CLI now owns per-instance key generation, OS credential storage,
and HMAC computation. Bounded secret resolution and compiler integration are
completed by ADR 0013.

Sensitive receipts use a closed property vocabulary: `password` and uppercase
`environment.NAME` entries. Durable non-sensitive inputs may represent those
paths only as explicit null clears. The application `environment` root may be
null, empty, or contain canonical entries whose values are all null. Resource
construction and decoding reject overlap between a clear and a receipt. Core
state projection wraps each receipt in an opaque `SensitiveIntent` and exposes
it only as `OwnedValue::Sensitive`; it still rejects a sensitive path on the
wrong resource kind. The wrapper supports equality without exposing a MAC or
key identifier.

State format version 2 is a deliberate pre-release incompatibility. Version 1
state is rejected rather than migrated or interpreted without sensitive-input
invariants. No released state compatibility promise exists yet, and failing
closed avoids silently adopting a raw secret-bearing legacy shape.

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

- Remote adapters must provide explicit observations for every desired,
  nonignored property path they claim to support.
- Durable managed-input keys outside the finite MVP vocabulary fail closed
  through a redaction-safe projection error; this avoids guessing how legacy
  or future state should be interpreted.
- State-only protection, dependency, and ownership changes are represented as
  no-op remote actions that a later executor must checkpoint.
- Ignored paths are explicit write exclusions on existing-resource actions;
  checkpoint data is not permission to send a whole-object update.
- Executing a move requires future atomic `StateFile` and journal support for
  removing the source and writing the target as one recoverable transition.
  This checkpoint defines that target but adds no executor behavior.
- Dependency cycles are planning diagnostics, not desired-state construction
  errors; missing and self-dependencies still fail at the desired-state seam.
- Adding a property path requires an explicit planner vocabulary, kind
  validation, state projection, and adapter update.
- Replacement behavior remains a later Phase 5 checkpoint.
- Configuration compilation never reads secret bytes or resolves logical
  references to physical IDs.
- Sensitive fingerprints are compared only as opaque intent receipts. Matching
  desired and stored receipts converge when the remote value is write-only;
  changed receipts plan a write, known remote absence plans restoration,
  explicit null remains a distinct clear, and omission relinquishes ownership
  without a remote write. Concrete sensitive configuration inputs still fail
  closed until composition can compute receipts safely.
- State format version 1 cannot be opened by this pre-release implementation;
  callers must intentionally recreate state in version 2.
