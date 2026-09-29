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
path-level three-way diffs, opaque sensitive intent, fail-closed partial
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

The same composition seam now returns one combined project-and-environment
snapshot. Environment discovery binds managed resources by physical ID,
validates their containing project, probes unmanaged names only inside a
proved-present parent, distinguishes authoritative from partial collections,
and preserves description omission versus null without admitting Dokploy's
environment variables or nested resource payloads into planner state.

Application discovery now extends that snapshot with fully paginated search
inside proved physical environments and direct managed-ID reads. It validates
current containment, probes move and reparent targets without trusting
replacement parents, preserves owned field presence, and reduces the
secret-bearing runtime environment document to a value-free shape before it
crosses the SDK seam.

Postgres discovery uses the same combined snapshot with its own collection
authority. It exhausts bounded parent-scoped search, validates managed IDs and
current containment, preserves database and username field presence, and
represents every requested password as a write-only sensitive observation.
Logical moves remain safe inside one physical environment, while physical
reparenting fails closed until an explicit remote action exists.

The durable sensitive-state foundation now stores non-null password and
application environment intent only as opaque, versioned HMAC-SHA-256
receipts. State format version 2 rejects raw sensitive values, ambiguous
clear-and-receipt ownership, noncanonical receipts, and version 1 state. The
planner compares those receipts without exposing their MACs or key identifiers
and treats write-only remote observations without inventing drift. A private
CLI module keeps one random fingerprint key per normalized Dokploy instance in
the OS credential store.

The instance-bound compiler now preflights unsupported references before any
external access, resolves literal, environment, and bounded workspace-relative
file sources exactly once, derives path-bound receipts, and returns exact bytes
only through a redacted one-shot execution sidecar. Effective configuration
digests include canonical receipt identities, so content and key rotation plan
updates while unchanged intent converges. The offline compiler still rejects
concrete sensitive inputs and keeps clear or unmanaged fields I/O-free.

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
