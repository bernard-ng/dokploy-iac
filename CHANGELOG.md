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
