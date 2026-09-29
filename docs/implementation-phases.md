# Implementation phases

Work proceeds in order so later mutation logic depends on proven contracts and
durable state semantics.

## Phase 0: external contracts — complete

- Vendor the OpenAPI 3.1 document at an immutable upstream commit.
- Compare Rust generator candidates against the same input.
- Audit the critical application, project, and database operations.
- Prove `x-api-key` authentication against Dokploy `v0.30.6`.
- Capture sanitized live fixtures.
- Prototype tolerant `ApplicationDetails` and `ProjectTopology` models.

## Phase 1: foundation — complete

- Establish the Cargo workspace and toolchain policy.
- Add the `dokploy` CLI skeleton.
- Add structured diagnostics and tracing.
- Add context selection and credential storage.
- Add CI for formatting, linting, tests, dependencies, OpenAPI audit, and
  compose validation.

## Phase 2: API and SDK — complete

- Normalize the vendored OpenAPI document without rewriting its response
  contract.
- Generate private request and endpoint bindings with pinned `oas3-gen`.
- Add the owned HTTP adapter and explicit `x-api-key` authentication.
- Expand handwritten critical read models and fixture tests.
- Expose the first ergonomic SDK services and generated imperative commands.
- Redact secret-bearing fields before imperative responses reach the terminal.
- Distinguish safe read failures from mutations whose remote outcome is
  unknown.
- Verify the owned SDK and CLI redaction against local Dokploy `v0.30.6`.

## Phase 3: state — complete

- Add logical resource addresses, lineage, serials, instance binding, locking,
  atomic writes, backups, operation journals, and recovery detection.
- Bind journaled actions to their exact state transitions and refuse new
  mutations while recovery evidence is unresolved.

## Phase 4: configuration — complete

- Add the versioned `dokploy.yaml` model, ownership-aware fields,
  typed references, descriptor-only secrets, dependencies, lifecycle rules,
  moves, removals, schema generation, containment, and semantic validation.
- Expose offline `dokploy init`, `dokploy schema`, and `dokploy validate`
  commands without requiring a connection context or contacting Dokploy.
- Create configuration files without overwriting existing paths and load only
  bounded regular files.

## Phase 5: planner

- Complete the first pure planner checkpoint: typed path-level ownership across
  desired, stored, and remote snapshots; nested source and environment paths;
  value-free sensitive intent; three-way diffs and drift origins; fail-closed
  partial observations; immutable state-checkpoint targets; protected deletion;
  and deterministic redaction-safe JSON.
- Order desired-resource actions dependency-first and removals dependent-first,
  with stable lexical ties, conservative mixed-action phases, and typed cycle
  diagnostics backed by `petgraph`.
- Plan explicit identity-preserving moves and idempotent retain or destroy
  removals with atomic checkpoint targets and fail-closed collision probes.
- Preserve stored ownership baselines for ignored existing-resource paths and
  expose deterministic write exclusions without treating them as drift.
- Compile validated configuration into pure desired state plus a redacted,
  non-serializable execution sidecar that preserves logical containment,
  and domain references without resolving remote IDs. Reject concrete
  sensitive inputs until composition can resolve secrets, calculate intent
  fingerprints, and load their key safely.
- Project fresh project topology into explicit remote observations with
  authoritative absence, physical-ID matching for managed projects, exact-name
  collision probes, and fail-closed transport and topology diagnostics.
- Persist non-null sensitive intent only as strict, versioned HMAC-SHA-256
  receipts in state format version 2. Compare those receipts opaquely in the
  pure planner, treating write-only remote observations as conclusive for
  presence but never as comparable secret values.
- Build the remaining fresh remote state adapters.
- Add adapter-projected mutability and ordered replacement behavior.
- Keep mutation code unreachable from the planner.

## Phase 6: executor MVP

- Reconcile projects, environments, applications, Postgres, Redis, and domains.
- Add bounded execution, checkpoints, partial-failure behavior, protection,
  ignored changes, multi-step configuration, and deploy-on-change.
- Require `apply` followed by `plan` to converge to no changes.

## Later phases

- Phase 7: recovery, destroy, and state commands.
- Phase 8: additional Dokploy resource adapters.
- Phase 9: import, moves, removals, and refactoring workflows.
- Phase 10: saved plans, CI-oriented output, completions, and release tooling.
