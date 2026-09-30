# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Changed

- Separate command results on standard output from plans, prompts, warnings,
  and diagnostics on standard error, and reject non-interactive apply,
  recovery, and destroy approval unless `--auto-approve` is explicit.
- Reframe the README as a project presentation focused on the product,
  capabilities, architecture, maturity, and contributor entry points.

### Added

- Add read-only interactive and non-interactive resource import with canonical
  configuration generation, durable identity adoption, unmanaged secrets,
  protected databases, and immediate plan convergence.
- Add the declarative MySQL substrate with strict environment containment,
  canonical configuration writing, and independently fingerprinted user and
  root-password intent; remote reconciliation remains fail-closed until its
  adapter mutation contract is implemented.
- Enforce independent generated-SDK compilation, explicit RustSec auditing,
  cargo-dist workflow drift checks, release-plan validation, and a disposable
  live Dokploy apply-and-converge job in pull-request CI.
- Add `dokploy plan --out` and `dokploy apply PLAN` with a strict, owner-only
  saved-plan envelope that is revalidated under the writer lock against
  configuration, state lineage and serial, and keyed fresh-remote evidence.
- Add `dokploy recover` with durable checkpoint reconstruction, fresh remote
  verification, exact approval, safe create adoption, and fail-closed handling
  for unreadable or ambiguous interrupted outcomes.
- Add `dokploy destroy` with a fresh dependent-first deletion plan, protection
  enforcement, exact approval, journaled checkpoints, and an absent-state no-op.
- Add atomic state address moves, idempotent protection toggles, and guarded
  state-only forgetting with dependent-reference validation.
- Add `dokploy apply --auto-approve` for explicitly non-interactive execution
  while preserving fresh plan rendering and validation.
- Add offline shell completion generation for Bash, Zsh, Fish, PowerShell, and
  Elvish from the public CLI command tree, with deterministic command coverage
  and installation guidance for every supported shell.
- Add an explicit `DOKPLOY_FINGERPRINT_KEY` source for deterministic,
  keyring-free sensitive-intent receipts in headless CI environments.
- Add typed, single-attempt delete operations for all six MVP SDK resources,
  preserving structured remote rejections and outcome-unknown transport errors.
- Add read-only `dokploy state list` and `dokploy state show` commands with
  workspace-instance validation and value-free managed-field rendering.
- Add owned, read-only Domain SDK operations with strong identifiers and
  fixture-backed models that expose only host and application linkage.
- Add application-scoped Domain discovery with managed-ID matching, exact-host
  collision probes, explicit collection authority, and fail-closed duplicate
  and endpoint-consistency checks.
- Add adapter-projected mutation contracts, explicit reparent actions, stable
  unsupported-transition diagnostics, and protected ordered replacements.
- Add the public read-only `dokploy plan` workflow with single-read source
  digests, deterministic absent-state planning, canonical JSON, human summaries,
  recovery/state revalidation, and detailed exit status.
- Add the first typed mutation SDK path for project creation, preserving both
  the project and Dokploy-created default-environment identities.
- Add planner-selected checkpoint materialization into validated durable
  resource state without exposing non-null sensitive input receipts.
- Add the first journaled executor slice for fresh project creation, including
  exclusive locking, initial lineage creation, per-step checkpoints, and
  apply-then-plan convergence coverage.
- Add typed environment creation and executor handling for both explicit
  environment mutations and safe adoption of Dokploy's project-created default
  environment without a duplicate create.
- Add typed application, Postgres, Redis, and Domain creation with redacted
  secret-bearing inputs, required-create contracts, dependency-ordered
  execution, and a durable checkpoint after every successful resource.
- Add typed in-place updates across all MVP resources, multi-step application
  configuration, deploy-on-runtime-change, state-only lifecycle checkpoints,
  and redacted environment merging that preserves unowned remote variables.
- Add the public `dokploy apply` workflow with fresh plan rendering, exact
  interactive confirmation, a validated execution-bound option, and a concise
  applied-change summary.
- Add durable multi-step execution for all six MVP resource types, bounded
  overlap for independent Postgres and Redis mutations, and partial-failure
  journaling that checkpoints successful in-flight siblings without rollback.
- Add a disposable live apply-and-converge check against Dokploy `v0.30.6`.

- Add first-class containment across desired, stored, checkpoint, ordering, and
  remote-discovery seams. State format version 3 rejects missing or invalid
  containment and older formats instead of inferring parents from general
  dependencies.

- Add the pinned Dokploy OpenAPI 3.1 contract and a reproducible contract audit.
- Add a disposable local Dokploy integration environment.
- Add Phase 0 architecture notes, fixture requirements, and tolerant SDK model prototypes.
- Add the initial `dokploy` CLI with local context selection, safe connection
  resolution, OS credential storage, diagnostics, and tracing.
- Add a pinned Rust toolchain and CI checks for formatting, linting, tests,
  dependency policy, API contracts, and the integration Compose definition.
- Add reproducible `oas3-gen` bindings, generated endpoint metadata, and
  imperative CLI commands for all 604 upstream operations.
- Add owned project, application, and Postgres SDK reads with bounded GET
  retries, tolerant error decoding, and explicit outcome-unknown mutation
  failures.
- Add recursive secret redaction for imperative CLI responses and a live
  regression check against Dokploy `v0.30.6`.
- Add fixture-backed and live SDK contract tests for project, application,
  Postgres, and authentication responses.
- Add the typed Phase 3 state model with canonical resource addresses,
  instance identity, non-sensitive managed inputs, lineage, revisions, and
  serial-checked mutation invariants.
- Add instance-bound local state persistence with fail-fast locking, strict
  decoding, stale-revision protection, owner-only artifacts, atomic writes,
  and previous-state backups.
- Add durable JSONL operation journals with action-bound state transitions,
  append-before-checkpoint ordering, constrained failure codes, strict recovery
  scanning, and mutation refusal while recovery evidence is unresolved.
- Add the strict Phase 4 `dokploy.yaml` model with ownership-aware fields,
  bounded parsing, typed references and containment, descriptor-only secrets,
  lifecycle rules, moves, removals, JSON Schema, and redaction-safe semantic
  diagnostics.
- Add offline `dokploy init`, `dokploy schema`, and `dokploy validate`
  commands with no-clobber initialization, bounded regular-file loading, and
  deterministic schema and diagnostic output.
- Add the first pure Phase 5 planner checkpoint with typed nested property
  paths, value-free sensitive ownership, three-way drift attribution, immutable
  state targets, protected deletion, deterministic redaction-safe plans, and
  fail-closed snapshot validation and partial remote observations.
- Add deterministic dependency-first desired action ordering, dependent-first
  removal ordering, conservative mixed-plan phases, and typed cycle diagnostics
  backed by `petgraph`, with a crate-scoped Zlib license allowance for its
  `foldhash` dependency.
- Add pure-planner move and removal semantics with idempotent declarations,
  explicit target collision probes, atomic move checkpoint targets, retain or
  destroy policies, and redaction-safe two-address diagnostics.
- Add stored-baseline `ignore_changes` planning for existing resources,
  including explicit write exclusions, create and recreate semantics, move
  support, lifecycle-only suppression, and fail-closed selector validation.
- Add a configuration-to-planner compiler for all MVP resource kinds with
  derived containment and reference dependencies, lifecycle directives, and a
  redacted deferred-execution sidecar for logical references. Concrete
  sensitive inputs fail closed until a convergent intent fingerprint exists.
- Add read-only project remote discovery with explicit topology authority,
  pre-transport instance binding, presence-aware descriptions, managed-ID and
  unmanaged-name matching, explicit replacement collisions, move and removal
  probes, fresh reads, duplicate-topology rejection, and redaction-safe failure
  classification.
- Add strict durable sensitive-intent receipts, canonical sensitive property
  paths, raw-value rejection, disjoint clear ownership, and value-free planner
  projection in state format version 2. This is a deliberate pre-release
  incompatibility with version 1 state.
- Add opaque sensitive-intent comparison to the pure planner so matching
  durable receipts converge across write-only remote observations, while new,
  changed, absent, cleared, and relinquished intents remain distinct and plan
  without exposing receipt material.
- Add per-instance sensitive-fingerprint keys in the OS credential store with
  strict versioned envelopes, fail-closed initialization, immediate readback,
  zeroized key buffers, and domain-separated HMAC-SHA-256 receipt calculation.
- Add combined project and environment remote discovery with separate
  authority assertions, parent-scoped exact-name probes, managed-ID and
  containment validation, presence-aware descriptions, move and removal
  support, fresh reads, and secret-safe SDK response models.
- Add instance-bound sensitive configuration compilation with reference
  preflight, one-pass zeroized literal and environment resolution, secure
  bounded workspace-relative file reads, opaque desired receipts, effective
  content-aware digests, and a redacted one-shot execution sidecar.
- Add combined application remote discovery with separately asserted search
  authority, bounded stable pagination, managed-ID and containment checks,
  exact parent-scoped collision probes, move and reparent support,
  presence-aware owned fields, and value-free secret environment projection.
- Add combined Postgres remote discovery with separately asserted search
  authority, bounded stable pagination, managed-ID and containment checks,
  exact parent-scoped collision probes, presence-aware database fields,
  write-only password observations, and fail-closed physical reparenting.
- Add a failure-safe disposable Redis contract capture with owner-only raw
  evidence, deterministic secret-safe fixtures, provenance metadata, and
  three-way post-removal verification without deploying the resource.
- Add combined Redis remote discovery with separately asserted search
  authority, bounded stable pagination, managed-ID and containment checks,
  exact parent-scoped collision probes, write-only password observations, and
  fail-closed physical reparenting.
