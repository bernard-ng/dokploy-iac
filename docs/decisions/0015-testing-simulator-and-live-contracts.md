# ADR 0015: Testing: simulator, conformance, and live contracts

## Status

Proposed. Keeps the first engine's practice of path-routed doubles and live
acceptance, and its canary-secret scans.

## Context

About 70% of the first engine's per-kind cost was tests, written by hand per kind
against a hand-built fake server, so tests covered what the author thought of and
nothing proved the fakes matched Dokploy.

## Decision

### Layers

1. **Kernel tests.** Planner, state, journal, fingerprints: unit and property tests
   as today; model-based tests for plan determinism and journal recovery.
2. **Spec lint.** The coverage ledger (ADR 0002) and spec self-consistency, run by
   `cargo xtask specs --check` in CI.
3. **Document tests.** Property tests: random valid documents round-trip through
   render and parse; the generated JSON Schema accepts what the parser accepts.
4. **Conformance suite.** Generated per kind from its spec, run against the
   simulator. Scenarios:
   create; no-op after create; in-place update of each write group; replacement for
   each `create_only` field; drift on each field; delete and protected delete;
   move; outcome-unknown at create, update, delete, and deploy; recovery from each;
   dependency ordering; selector missing and ambiguous; import then plan is empty;
   and a canary scan (below). A new kind inherits all of it from its spec.
5. **Simulator.** `dokploy-sim` is an in-memory Dokploy: it implements the operations
   the specs reference by storing objects per kind, validating request bodies against
   the OpenAPI request schemas, returning response shapes copied from the recorded
   fixtures, enforcing natural-key uniqueness, and supporting fault injection (drop
   the connection before or after the request, return 5xx, filter collections by
   role, delay). It is reached through a `Transport` trait, so the engine runs
   against it in-process without HTTP.
6. **Live contract suite.** The same conformance scenarios run against a real,
   digest-pinned Dokploy in Docker for each supported version. Any scenario whose
   result differs between simulator and live fails and is fixed in the simulator or
   the spec. This is what keeps the simulator honest; the simulator never replaces
   live acceptance for a kind.
7. **Round-trip gate.** For each supported version: seed a live instance through
   the API with a rich fixture project and settings, `import all`, tear the
   instance down to a fresh one, `apply` and `deploy`, `import all` again, and
   require the documents to be equal under ADR 0011's normalisation. The same test
   runs against the simulator on every pull request.

### Canary secrets

Every scenario sets distinctive canary values for every secret and content field,
then scans stdout, stderr, plans, saved plans, state, journals, backups, and logs for
them. Any appearance fails the test.

### Cost target

A flat kind adds a spec file and a fixture capture; no Rust test code. A kind with
a hook adds that hook's tests only.

## Consequences

- Test code is written once and applies to every kind.
- The simulator is a real piece of software with its own tests; it is a
  development dependency, never shipped.
- Live suites are slower and run in CI for pull requests that touch specs, the
  engine, or the simulator, and nightly otherwise.
