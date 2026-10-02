# ADR 0003: Layered architecture and the kept kernel

## Status

Proposed.

## Context

The first engine's pure planner (`dokploy-core`, about 5,000 lines) and durable
state with journals (`dokploy-state`, about 5,000 lines) are small, well tested,
and already generic over a property map. The weight of the code base (about
145,000 lines including 45,000 generated) is the per-kind layers around them:
typed config, SDK adapters, remote projection, desired compilation, executor,
and import.

## Decision

Keep the kernel; replace the per-kind layers with generic ones driven by specs.

```
dokploy-api      generated OpenAPI types and operation table      kept, unchanged
dokploy-sdk      authenticated transport: bounded JSON, error
                 sanitising, generic `Imperative` operation call   kept, slimmed
dokploy-spec     spec model, registry, OpenAPI ledger, schema and
                 doc generation                                    new
dokploy-model    documents: parse, validate, render; values,
                 paths, spans, diagnostics                         new (replaces dokploy-config)
dokploy-core     pure planner and mutation contracts               kept, generalised
dokploy-state    state, journal, lock, fingerprints                kept, format 5
dokploy-engine   discovery, compile, execute, deploy, import       new (replaces cli/remote,
                                                                   executor, import, desired and
                                                                   sdk per-kind adapters)
dokploy-sim      in-memory Dokploy simulator for tests            new, dev-only
dokploy-cli      argument parsing, prompts, output                kept, thin
xtask            codegen, spec ledger, fixture checks             kept, extended
```

Dependency direction: `cli → engine → {model, core, state, spec, sdk}`;
`model → spec`; `core` and `state` depend on nothing above them. `core` stays free
of transport, runtime, and CLI code (invariants 5 and 6). `dokploy-engine` is
the only crate that talks to Dokploy.

**Generalisation of `core`.** `PropertyPath` changes from a closed enum with
per-kind allowed lists to a validated path string whose legality is decided by the
spec registry (ADR 0004). `MutationContract` is built from a spec instead of
hand-written per kind. The planner algorithm, plan types, checkpoints, drift
attribution, and diagnostics are unchanged.

**Imperative CLI.** The generated commands stay, now as
`dokploy api <resource> <operation>` (ADR 0017); they use the same transport.

## Consequences

- Roughly 30,000 lines of per-kind code (SDK models and client, CLI remote,
  executor, import, desired, config) are retired at the end of the migration
  (ADR 0016); the kernel's ~10,000 lines and their tests carry over.
- Public Rust API of `dokploy-sdk` shrinks to transport plus the generic call;
  the per-kind typed services are removed. The crate is pre-release, so this is
  not a compatibility event.
- `dokploy-model` replaces `dokploy-config`; the config format changes to version 2
  (ADR 0005).
