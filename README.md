# Dokploy Rust CLI and IaC engine

This repository is the implementation workspace for a native Rust `dokploy`
CLI. It will expose Dokploy's imperative API and a declarative infrastructure
workflow backed by the same SDK.

The implementation is intentionally phased. Phase 0 validates the external
contracts before the production workspace is shaped around an OpenAPI
generator.

## Current phase

Phase 0 is complete:

- the upstream Dokploy OpenAPI 3.1 document is vendored at an immutable commit;
- Rust generator candidates are tested against the same document;
- incomplete response schemas are inventoried;
- critical runtime responses are represented by handwritten tolerant models;
- live authentication and response fixtures are verified against local Dokploy
  `v0.30.6`.

Phase 1 is complete. The workspace now includes the CLI skeleton, contexts,
credential storage, diagnostics, tracing, and CI foundation.

Phase 2 is complete. The workspace now includes reproducible bindings and
imperative commands for all 604 upstream operations, an owned SDK for critical
project, application, and Postgres reads, bounded GET retries, explicit
outcome-unknown errors for interrupted mutations, and recursive secret
redaction for CLI responses. Fixture tests and live tests run against the
pinned local Dokploy `v0.30.6` environment. Phase 3 adds the durable state
engine.

Phase 3 is complete. The state subsystem now includes typed resource identity,
instance binding, lineage and serial revisions, fail-fast writer locking,
strict loading, atomic primary/backup checkpoints, durable operation journals,
and fail-closed recovery detection.

Phase 4 is complete. The configuration subsystem provides a versioned,
strictly bounded `dokploy.yaml` language with ownership-aware fields, typed
references, deferred secret descriptors, parent relationships, lifecycle
rules, moves, removals, JSON Schema, and redaction-safe semantic validation.
The offline `init`, `schema`, and `validate` commands do not require a Dokploy
context, credentials, or network access.

Phase 5 is underway. Its first checkpoint adds a pure planner seam over
explicit desired, stored, and remote property snapshots, with ownership-aware
path-level three-way diffs, value-free sensitive intent, fail-closed partial
observations, protection checks, immutable state-checkpoint targets, and
deterministic redaction-safe JSON output. Dependency graphs now reject cycles
and deterministically order desired actions dependency-first before removals
run dependent-first. Explicit moves now preserve managed identity through an
atomic source-to-target checkpoint, while removal directives choose retain or
destroy semantics without weakening identity or protection checks. Ignored
changes preserve stored ownership baselines for existing resources and expose
explicit write exclusions for future executors. The CLI composition layer now
projects one fresh, explicitly authoritative project topology into planner
observations, binding the client to the state instance, preserving omitted
versus null response fields, matching managed projects by physical ID, and
probing unmanaged names without exposing any mutation path.

See [the Phase 0 report](docs/phase-0-generator-bakeoff.md) for the evidence and
decision record.

The disposable runtime used to capture live API contracts is documented in
[the integration testing guide](docs/integration-testing.md).

The ordered delivery plan is tracked in
[the implementation phases](docs/implementation-phases.md).

## Product boundary

The CLI manages resources through an existing Dokploy API. It does not install
or provision the server that runs Dokploy.

Every reconciliation command reads fresh remote state. The project will not
contain a persistent API cache.
