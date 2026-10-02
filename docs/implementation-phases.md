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

## Phase 5: planner — complete

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
  non-serializable execution sidecar that preserves logical containment and
  domain references without resolving remote IDs. The instance-bound seam
  preflights unsupported references, resolves literal, environment, and
  bounded file sources once, calculates keyed intent receipts, and derives an
  effective digest from source syntax plus canonical receipt identities.
- Project fresh project topology into explicit remote observations with
  authoritative absence, physical-ID matching for managed projects, exact-name
  collision probes, and fail-closed transport and topology diagnostics.
- Persist non-null sensitive intent only as strict, versioned HMAC-SHA-256
  receipts and direct containment in state format version 3. Compare receipts opaquely in the
  pure planner, treating write-only remote observations as conclusive for
  presence but never as comparable secret values.
- Store one random fingerprint key per normalized Dokploy instance in the OS
  credential store and calculate domain-separated, path-bound HMAC-SHA-256
  receipts without exposing key or MAC bytes through the CLI module seam.
- Project fresh environment state into the same snapshot as projects, using
  managed physical IDs, parent-scoped name probes, separate collection
  authority, containment validation, and secret-safe description reads.
- Project fresh application state into the combined snapshot, using bounded
  exhaustive parent-scoped search, managed physical IDs, current-containment
  validation, reparent collision probes, presence-aware owned fields, and a
  value-free environment shape.
- Project fresh Postgres state into the combined snapshot, using bounded
  exhaustive parent-scoped search, managed physical IDs, current-containment
  validation, presence-aware database fields, write-only password
  observations, and fail-closed physical reparenting.
- Project fresh Redis state into the combined snapshot, using bounded
  exhaustive parent-scoped search, managed physical IDs, current-containment
  validation, write-only password observations, and fail-closed physical
  reparenting.
- Project fresh Domain state into the combined snapshot, using managed
  physical IDs, application-scoped exact-host probes, explicit collection
  authority, and logical application references.
- Separate direct containment from general dependencies across desired,
  stored, checkpoint, ordering, and remote-discovery seams. Reject older state
  rather than infer parents from dependencies.
- Build the remaining fresh remote state adapters.
- Add adapter-projected mutability and ordered replacement behavior.
- Keep mutation code unreachable from the planner.
- Expose read-only `dokploy plan` with canonical JSON, human summaries,
  authoritative fresh discovery, state/recovery revalidation, and detailed
  exit status.

## Phase 6: executor MVP — complete

- Reconcile projects, environments, applications, Postgres, Redis, and domains.
- Add bounded execution, checkpoints, partial-failure behavior, protection,
  ignored changes, multi-step configuration, and deploy-on-change.
- Require `apply` followed by `plan` to converge to no changes.
- Expose a fresh-plan `dokploy apply` workflow with exact interactive approval
  and a validated parallelism bound.
- Journal concurrent independent database mutations before dispatch, stop
  scheduling after the first failure, allow already-running work to finish,
  and checkpoint every successful sibling without automatic rollback.
- Verify all six resource types and apply-then-plan convergence against the
  digest-pinned local Dokploy `v0.30.6` environment.

## Phase 7: recovery and destructive operations — complete

- Recover interrupted journaled mutations from durable evidence and fresh
  remote observations without blindly retrying uncertain creates.
- Destroy every tracked resource in dependent-first order while honoring
  durable protection.
- Inspect, move, forget, protect, and unprotect state through explicit commands.
- Exercise crash, partial-failure, recovery, and destructive-operation safety
  in the automated test suite.

## Phase 8: resource breadth — in progress

- Typed SDK contracts and live create-update-delete evidence cover MySQL,
  MariaDB, MongoDB, LibSQL, and Compose.
- MySQL is declarative end to end: strict configuration, durable state, fresh
  discovery, mutation contracts, journaled create-update-delete execution,
  recovery, protected import, and live apply-then-plan convergence. Database
  and username changes update in place; post-create credential changes remain
  blocked until Dokploy can persist them safely without requiring deployment.
- MariaDB is declarative end to end with authoritative bounded discovery,
  collision and endpoint-consistency checks, an optional create-only root
  password, journaled batched mutations, outcome-unknown recovery, protected
  import, and disposable live acceptance. Database and username changes update
  in place; post-create user or root password changes remain blocked.
- MongoDB is declarative end to end with authoritative bounded discovery,
  collision and endpoint-consistency checks, journaled batched mutations,
  outcome-unknown recovery, protected import, and disposable live acceptance.
  Username and replica-set mode update in place; post-create password changes
  remain blocked.
- LibSQL is declarative end to end with authoritative `project.one` topology,
  direct-read agreement, collision detection, safe identity proof for the
  no-ID create response, journaled updates, explicit recovery, and protected
  import. Description, username, and password update in place. Changing the
  atomic primary-or-replica node replaces the undeployed record in
  delete-before-create order.
- Compose is declarative end to end with strict environment containment,
  bounded authoritative search and direct-read agreement, descriptor-only
  document fingerprints, journaled mutations, explicit recovery, and
  protected document-free import. Description and the opaque raw document
  update in place. Deletion always preserves volumes, and the disposable live
  acceptance never deploys the Compose record.
- Ports are declarative end to end as application-contained resources with
  duplicate collision rejection, authoritative `application.one` discovery and
  direct-read agreement, fresh-read complete-replacement updates,
  delete-before-create containment replacement, explicit recovery, and
  protected import. The disposable live acceptance never deploys the
  application.
- Redirects are declarative end to end as application-contained resources
  keyed by regular expression with authoritative `application.one` discovery,
  direct-read agreement, fresh-read complete-replacement updates that keep
  unowned fields, delete-before-create containment replacement, explicit
  recovery, and protected import (ADR 0042).
- Security entries are declarative end to end as application-contained
  resources keyed by username with descriptor-only fingerprinted passwords,
  fail-closed updates that require the declared password, manual recovery for
  unprovable password rotation, and secret-free protected import (ADR 0043).
  Both disposable live acceptances never deploy the application.
- Mounts are declarative end to end as environment-contained resources whose
  typed target (application, Compose, or a supported database) is an owned
  property and inferred dependency, with authoritative per-target discovery,
  direct-read agreement, descriptor-only fingerprinted file content, fresh-read
  complete-replacement updates, delete-before-create replacement on target or
  storage-type change, explicit recovery, and protected import (ADR 0044). The
  disposable live acceptance never deploys a target.
- Schedules are declarative end to end as environment-contained resources whose
  typed target (an application, or a Compose service) is an owned property and
  inferred dependency, with an explicit target-scoped name, a required
  `enabled` flag, descriptor-only fingerprinted command and script,
  authoritative per-target `schedule.list` discovery with direct-read
  agreement, fresh-read complete-replacement updates, delete-before-create
  replacement on target or service change, explicit recovery that never guesses
  an executable rotation, and protected import (ADR 0046). The disposable live
  acceptance keeps every Schedule disabled and proves none ever executed or
  deployed a target.
- External selector resolution is implemented for application server,
  build-server, and registry associations (ADR 0045): typed local-or-named
  selectors, fresh minimal collection reads with exact-name matching that
  blocks zero or multiple matches, keyed saved-plan receipts that bind the
  resolved identities, create-time placement with in-place nullable
  associations and delete-before-create replacement on a changed placement, and
  name-selector import. The resolver seam is kind-agnostic; Backup destinations
  are wired onto it (ADR 0047). Schedule server and Dokploy-server scopes stay
  unsupported (ADR 0033).
- Database Backups are declarative end to end as environment-contained
  resources whose typed database target (PostgreSQL, MySQL, MariaDB, MongoDB,
  or LibSQL) is an owned property and inferred dependency and whose destination
  is an external name selector (ADR 0047): authoritative per-target `backups`
  discovery with direct-read agreement, fresh-read complete-replacement updates,
  in-place destination re-selection, delete-before-create replacement on a
  target change, saved-plan binding of the resolved destination identity,
  explicit recovery, and protected import. The disposable live acceptance keeps
  every Backup disabled and proves no backup ran, no destination was contacted,
  and no target was deployed. Compose and web-server Backups remain unsupported.
- Backups and schedules use the shared typed-target and external-selector
  model in ADR 0026.
- Database Backups now have a typed SDK contract for PostgreSQL, MySQL,
  MariaDB, MongoDB, and LibSQL targets. Reads use bounded authoritative
  `target.one.backups` collections, mutations prove identity and target
  agreement, and disabled live evidence covers all mutable fields without
  deployment, execution, or destination traffic. Compose and web-server
  Backups remain unsupported.
- Completion still requires the remaining server placement selectors for
  Compose and databases, plus any adapter that is not yet declarative, each with
  unit, sanitized fixture, and live create-update-delete evidence.

## Phase 9: import and refactoring workflows — complete

- Import existing MVP resources through non-interactive IDs or terminal-only
  discovery without reading remote secrets into configuration or state.
- Persist identity-preserving moves, state address moves, removals, and
  protection changes with collision and dependency checks.
- Keep import atomic and no-clobber, generate canonical configuration, and
  require the first fresh plan to converge.

## Phase 10: mature CI/CD workflow — complete

- Save strict owner-only plan envelopes and revalidate instance, state,
  configuration, full plan, and keyed fresh-remote evidence before apply.
- Provide canonical JSON, detailed plan exit status, explicit non-interactive
  approval, and independent result and diagnostic streams.
- Generate deterministic Bash, Zsh, Fish, PowerShell, and Elvish completions.
- Validate generated API code, dependencies, release workflow drift,
  cargo-dist plans, and disposable live apply convergence in CI.
- Ship reviewed-plan and protected-apply GitHub Actions examples that pin apply
  to the exact source revision used for planning.
