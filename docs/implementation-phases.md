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

## Phase 3: state

- Add logical resource addresses, lineage, serials, instance binding, locking,
  atomic writes, backups, operation journals, and recovery detection.
- Completed: typed state identities and mutations, instance-bound storage,
  strict decoding, stale-write detection, and atomic primary/backup files.
- Remaining: durable operation journals and recovery detection.

## Phase 4: configuration

- Add the versioned `dokploy.yaml` model, ownership-aware fields, references,
  secrets, dependencies, lifecycle rules, schema generation, and semantic
  validation.

## Phase 5: planner

- Build desired, stored, and fresh remote state models.
- Add drift detection, deterministic diffs, dependency ordering, replacement,
  moves, and removals.
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
