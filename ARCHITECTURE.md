# Architecture

How the v2 engine is built and the rules it must not break. Decisions are in
[`docs/decisions/`](docs/decisions/); the field-level target is in
[`docs/vision/`](docs/vision/); the grammar of kind specs is in
[`docs/design/spec-format.md`](docs/design/spec-format.md).

## Shape

```
                       specs/*.yaml  +  openapi/dokploy.json  +  fixtures/api/live
                              │                │                       │
                      ┌───────▼────────┐       └──────► coverage ledger (CI) ◄──┘
                      │  dokploy-spec  │  kinds, fields, write groups, hooks
                      └───┬────────┬───┘
                          │        │
              ┌───────────▼──┐   ┌─▼───────────────────────────────────────────┐
 YAML ───────►│ dokploy-model│   │ dokploy-engine                               │
              │ parse, render│   │ discover ─ compile ─ plan ─ execute ─ deploy │
              │ validate     │   │ import ─ prove ─ persist                     │
              └──────┬───────┘   └───┬───────────┬────────────┬─────────────────┘
                     │               │           │            │
                ┌────▼────┐   ┌──────▼────┐ ┌────▼─────┐ ┌────▼────────────────┐
                │  core   │   │   state   │ │   sdk    │ │ dokploy-sim (tests) │
                │ planner │   │ journal,  │ │ transport│ └─────────────────────┘
                │ (pure)  │   │ lock,     │ │ + generic│
                └─────────┘   │ fingerprint│ │ operation│          dokploy-cli
                              └───────────┘ └──────────┘          (thin shell)
```

`dokploy-engine` is the only crate that talks to Dokploy. `dokploy-core` has no
transport, runtime, or CLI dependency; it reads `dokploy-spec` for property paths and
mutation contracts. See [ADR 0003](docs/decisions/0003-layered-architecture-and-kept-kernel.md).

## Data flow

**Plan.** Parse the document with the specs → compile desired state (resolve
sources, selectors, refs; fingerprint secrets) → discover fresh remote state with
the minimum reads → project it to properties → plan (pure) → render.

**Apply.** Build the plan under the writer lock → ask for approval → for each step:
journal, send one request per write group, verify by re-reading, checkpoint state.
Deploy is a separate phase ([ADR 0012](docs/decisions/0012-deployment-model.md)).

**Import.** Inventory → validate → build document and state from the same field
mappings → render, re-parse, compile → prove the first plan is empty → persist →
re-read ([ADR 0011](docs/decisions/0011-import-and-round-trip-contract.md)).

## Invariants

1. Every reconciliation reads fresh Dokploy state.
2. There is no persistent API cache.
3. Generated API files are never edited manually.
4. IaC-critical read models do not trust OpenAPI responses; they are projected from
   recorded live captures (all response schemas are empty objects).
5. The planner cannot mutate remote infrastructure.
6. The executor follows a plan and does not invent changes.
7. Only resources recorded as managed can be destroyed.
8. Omitted configuration fields are unmanaged.
9. A `null` field and an omitted field have different meanings.
10. Remote ids establish physical identity after import or creation.
11. Addresses establish configuration identity; they are hierarchical.
12. Moves preserve remote identity.
13. State lineage and serial protect against stale state.
14. State is bound to one Dokploy instance and one document.
15. Every successful mutation is journaled and checkpointed.
16. An uncertain non-idempotent mutation is never blindly retried.
17. Protected resources cannot be destroyed or replaced.
18. Secrets never appear in state, plans, logs, diagnostics, or snapshots.
19. Partial failure does not trigger automatic rollback.
20. Plan output is deterministic.
21. Unmanaged Dokploy resources are untouchable.
22. A successful apply followed immediately by a plan converges to no changes.

New in v2:

23. **Every field is classified.** Each request and response field of each operation a
    spec uses is mapped, read-only, an action, or ignored with a reason; CI fails on
    anything else.
24. **Import and apply share one mapping.** Import never silently drops a manageable
    field; what it cannot write is reported.
25. **Kind behavior lives in specs.** Engine code is kind-agnostic; a hook needs a
    written reason and tests.
26. **Apply never deploys on its own.** Deploys happen only by policy or request.
27. **Disruptive changes are explicit.** Changes that restart Traefik or the server
    need explicit approval and are applied last.
28. **Unknown remote fields are tolerated on read; unknown configuration fields are
    rejected.**
29. **Applying settings before projects is the workspace order.**
