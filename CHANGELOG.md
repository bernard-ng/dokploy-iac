# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

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
