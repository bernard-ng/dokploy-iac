# ADR 0016: Replace the engine, keep the kernel

## Status

Accepted (2026-10-02). Depends on the pre-release compatibility policy in ADR 0001.

## Context

The per-kind layers of the first engine (typed config, SDK adapters, remote
projection, desired compilation, executor, importer) are about 30,000 lines that
the spec-driven design makes redundant. Nothing has been released, so there are
no users to migrate and no reason to maintain two engines.

## Decision

**Replace, do not coexist.** There is no `--engine` switch, no document-version
dispatch, and no dual state support.

1. **Freeze.** Commit `96cab73` is the reference for the first engine's behavior and
   the source of test scenarios. It is deliberately **not tagged**: a tag would
   promise a stable release, and nothing is stable before the beta. The commit id
   in history is enough.
2. **Extract goldens.** Before deleting a layer, its tests are mined for scenarios
   (inputs, expected requests, expected plans, expected state). They become
   conformance fixtures that the spec engine must satisfy for the same kind. This is
   the *parity oracle*: the old tests define done for a ported kind.
3. **Build the new layers** (specs, model, engine, simulator) beside the kernel and
   switch the CLI once the beta surface passes its acceptance gates.
4. **Delete as you replace.** When a layer's replacement passes the extracted
   goldens, its old code and tests are deleted in the same change. No dead code is
   kept "for reference"; commit `96cab73` is the reference.
5. **Order.** Foundations and flat settings kinds first, to prove the cost model;
   then project kinds from the leaves up; then the cross-cutting features (secrets,
   import all, deploy); then the live round-trip gate.

**Cost-model gate.** After the first three kinds (a flat settings kind, a leaf
kind, a union-bearing kind) the cost per kind is measured against the targets of
ADR 0002 (a flat kind in under a day, no per-kind test code). If the targets are
missed, the design is revisited before more kinds are ported.

**What carries over unchanged.** The generated `dokploy-api` and its xtask
pipeline, the SDK transport and sanitising, `dokploy-core`'s planner, `dokploy-state`'s
storage and journal, credential storage, fingerprinting, shell completions, the
imperative command surface, release and CI tooling.

**What is deleted when replaced.** `dokploy-config`; `dokploy-sdk`'s per-kind
models, services, and client methods; `dokploy-cli`'s `remote`, `executor`,
`import`, `desired`, and per-kind tests; the first engine's documentation (already
removed).

## Consequences

- The tree has one engine; CI always builds one path.
- The supported CLI surface is generated and tested from the current specs.
- Commit `96cab73` preserves the old behavior for comparison at zero maintenance cost.
