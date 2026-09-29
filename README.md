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

Phase 4 is in progress. Its configuration core now provides a versioned,
strictly bounded `dokploy.yaml` language with ownership-aware fields, typed
references, deferred secret descriptors, parent relationships, lifecycle
rules, moves, removals, JSON Schema, and redaction-safe semantic validation.
The `init`, `schema`, and `validate` CLI commands are the next checkpoint.

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
