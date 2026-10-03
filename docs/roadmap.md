# Roadmap to the beta

Plan for building the v2 engine specified in [`docs/decisions/`](decisions/).
Status of every ADR is **Proposed** until reviewed; this plan changes with them.
Effort figures are my estimates in working days, **not validated**: the cost gate
at the end of M2 replaces them with measurements and re-plans.

The beta is defined by [ADR 0001](decisions/0001-product-goal-and-scope.md): the
round-trip test passes on two supported Dokploy versions in CI, every beta-list kind
has import/plan/apply/recovery/destroy with live acceptance, and every API field of
those kinds is classified.

## Milestones

| # | Milestone | Content | Exit criterion | Days |
|---|-----------|---------|----------------|------|
| M0 | Foundations | Commit `96cab73` is the first-engine reference (no tag; nothing is stable yet). Move the generated commands under `dokploy api` (small, independent). Prototype the spec grammar on three kinds: `registry` (flat), `redirect` (leaf with fresh-read update), application `source` (union). New `dokploy-spec` crate: parse, validate, registry. `cargo xtask specs --check` ledger for the request side against the OpenAPI. Tool to mine legacy tests into golden scenarios. Capture-tool skeleton. | Ledger runs in CI on the three specs; grammar v0 frozen in `docs/design/spec-format.md` | 4–6 |
| M1 | Kernel and model | `PropertyPath` becomes spec-validated paths; `MutationContract` from specs. State format 5: hierarchical addresses, document id, per-document directories. New `dokploy-model`: parse, validate with spans, canonical render, JSON Schema. `Transport` trait in the SDK. Engine skeleton: compile and plan offline. | `plan` of a settings document containing a registry runs against a canned remote | 8–10 |
| M2 | Engine core, simulator, conformance | Discovery and projection; executor with write groups, verification, journal, recovery; `dokploy-sim`; conformance generator. Port `tag`, `registry`, `redirect` using extracted goldens; delete the legacy code they replace. | Three kinds pass the full conformance suite; **cost gate** measured (below) | 10–12 |
| M3 | Project kinds | Leaves first (port, security, mount, schedule, backup, volume backup, patch, domain), then databases (six kinds), Compose (all source types), application (sources, build, resources, Swarm, preview), environment, project. Hooks: Compose multi-service schedules, tag membership. | Every project kind green in conformance against the simulator; request-side ledger green | 16–22 |
| M4 | Secrets, environment, content | Sources (`env`, `file`, `vault`), `env` type and `env_file`, content digests, fingerprints for paths, secrets-out writer and `.gitignore` handling, canary scans in the suite. Overlaps the end of M3. | Canary suite passes for every kind | 6–8 |
| M5 | Settings kinds | SSH key, server, network, registry, destination, certificate, DNS provider, four git providers, twelve notification providers, vault provider, AI provider, organization, web server (with the disruptive rule). Mostly specs plus live captures. | Every settings kind green in conformance; fixtures captured | 12–16 |
| M6 | Import all | Generic importer, classification, manifest, unsupported ledger, convergence proof, `adopt`. | `import all` then `plan` is empty for the rich fixture | 7–9 |
| M7 | Deploy | Policy, `deploy` command, `apply --deploy`, waiting, ordering, journal. | A rebuilt fixture stack reaches `running` on the simulator and live | 5–7 |
| M8 | Live suites and beta | Docker live suite for 0.30.6 and 0.30.7; differential simulator-versus-live; round-trip gate; upstream watch; `doctor`; reference and usage docs generated; delete any remaining legacy code; cargo-dist beta tag. | Round trip green in CI on both versions | 7–10 |

Total **75 to 100 days** (the earlier gap review estimated 50 to 68 for a smaller
design; the difference is hierarchical addresses and state, the simulator and
conformance generator, deploy, and the live and round-trip suites).

Critical path: M0 → M1 → M2 → M3 → M6 → M8. M4, M5, and M7 can proceed in parallel
once M2 is done.

## M0 status

Done: `dokploy-spec` (parser, lint, registry, request-side ledger), grammar v0 and five
working specs (`registry`, `redirect` full; `application`, `environment`, `project`
partial), `cargo xtask specs --check` in CI, the `dokploy api` command prefix, and the
golden-mining tool (`cargo xtask goldens`, [`docs/design/golden-ledger.md`](design/golden-ledger.md)):
891 legacy scenarios in `goldens/`, all `pending`; and the manually dispatched capture
workflow (`.github/workflows/capture.yaml`, [`docs/ci.md`](ci.md#capturing-fixtures)) with
`specs/versions.yaml`. A full capture ran against a real v0.30.7 and produced
`fixtures/api/live/v0.30.7/`; v0.30.7 stays `candidate` until its live suite passes.
M0 is complete once the capture workflow has run once in GitHub Actions.

## M1 status

Done: spec-validated property paths (`dokploy-spec` path catalog, `PropertyPath`,
`MutationContract::from_spec`; [`spec-format.md`](design/spec-format.md#property-paths)).
State format 5 with hierarchical addresses, document ids, per-document directories, open kinds,
and open sensitive paths ([`state-format.md`](design/state-format.md)); a `registry`, which has no
per-kind code, plans and checkpoints from its spec alone (kernel test).
The document model (`dokploy-model`: parse, validate with positions, canonical render, JSON
Schema; [`document-format.md`](design/document-format.md)).
The `Transport` trait (`dokploy-sdk`): the engine sends through it, over HTTP or in memory.
The engine skeleton (`dokploy-engine`, [`engine.md`](design/engine.md)): **a settings document
containing a registry is planned against a canned remote**, in memory and over HTTP.
**M1 is complete.**

## M2 status

Done: the generated request contracts; live captures of `registry` and `tag` on 0.30.6 and 0.30.7;
`dokploy-sim` ([`simulator.md`](design/simulator.md)); apply and recovery in `dokploy-engine`
([`engine.md`](design/engine.md)); nested kinds (scoped and embedded collections); the conformance
suite ([`conformance.md`](design/conformance.md)); a live engine test in CI for both versions; the
golden ledger classification (37 of the 55 scenarios of the three kinds covered); and the CLI cut over
to the v2 engine with the whole first engine deleted (no support for version 1 documents). `tag`,
`registry`, and `redirect` pass the full suite with no per-kind Rust. The **cost gate** is measured
([`cost-gate.md`](design/cost-gate.md)): met for the flat and leaf shapes, the union shape is the
first thing to try in M3.

Left from the plan's M2 row: nothing. `import`, saved plans, and `--parallelism` are removed until M6
and later.

**Kernel vocabulary (after M2).** `dokploy-core` and `dokploy-state` no longer carry a closed list of
properties or resource kinds: a `PropertyPath` is a handle on the spec's `PropertyInfo`, a
`ResourceKind` is a registered name, and containment is the address path. The planner tests run on
three synthetic kinds (`widget`, a nested `gadget`, `cache`) in `crates/dokploy-core/tests/support`,
so they do not move when a spec does.

## M3 status

Started. Done, all with specs and no per-kind Rust: the leaf kinds `redirect`, `port`, and `security`
(full conformance); the databases `postgres`, `mysql`, `mariadb`, `mongo`, and `redis`, and the
`environment` (conformance for the fields they carry); the `application` with its six-arm union
`source` (91 to 101 scenarios per arm). Engine work the kinds needed: follow-up writes after a create, selectors, struct
members and environment blocks, creation requirements that follow the create operation, and
`moves` and `removed`. The vision ratchet (`docs/vision/gaps/`) counts what the parser still
rejects of the two vision documents: 28 lines for the project document, 19 for the settings one.

Both decisions that blocked the rest are made ([ADR 0018](decisions/0018-several-parents-and-per-member-unions.md)):
a kind may have several parents (`mount`, `domain`), and a union is planned per member (the
application `source`).

Still open, and not blocked: the `libsql` `node` (can now be a union), `compose` (a union source),
`build` (a union), the swarm settings, `networks`, and tag membership (a hook); the Compose
multi-service schedules hook. Needs destinations (M5): backups and volume backups, which need a
`destination` spec. Needs a grammar addition: schedules and patches, whose list reads take two
parameters.

## Cost gate (end of M2)

After three kinds of different shapes (flat, leaf, union) the following are measured
and compared with the targets in ADR 0002 and ADR 0015:

- lines of Rust and of test code added per kind (target: zero test code, no Rust for
  a flat kind);
- time to add a field that needs a new write group;
- hooks needed (target: under a fifth of kinds);
- simulator-versus-live differences found.

If any target is badly missed, the grammar or the engine is revisited before M3
starts, and this plan is re-estimated.

## What works at each milestone

Legacy layers are deleted as v2 replaces them (ADR 0016), so the declarative
commands cover only the kinds ported so far. For the full first engine, use the
commit `96cab73`.

| After | Declarative commands cover |
|-------|----------------------------|
| M1 | `validate`, `schema`, `plan` for ported settings kinds against canned data |
| M2 | validate, schema, plan, apply, recover, destroy, state for `tag`, `registry`, and `redirect` (**done**) |
| M3 | every project kind (no deploy, no import) |
| M5 | every settings kind |
| M6 | `import` for everything |
| M7 | deploy |
| M8 | the whole beta surface, on two Dokploy versions |

## Constraints and risks

- **Live captures need Docker.** Cloud sessions cannot run it (see `CLAUDE.md`), so
  captures run in a manually dispatched CI workflow that uploads sanitised fixtures
  for review, and the live suite runs in CI (ADR 0015). About 40 kinds need captures;
  this is the largest external dependency of the plan.
- **Spec grammar too weak.** If more than a fifth of kinds need hooks, the grammar is
  extended by ADR before continuing.
- **Simulator drift.** Mitigated by running the same scenarios live (ADR 0015); a
  divergence is always a bug in the simulator or the spec.
- **Unreadable settings.** `getWebServerSettings` is untyped; web-server fields need a
  capture before design. Fields that cannot be read back are dropped from the schema.
- **Dokploy churn.** The upstream watch (ADR 0014) turns drift into a CI issue.
- **Scope creep.** The beta kind list in ADR 0001 is the boundary.

## Decisions recorded (2026-10-02)

1. Children nest under their service; addresses are hierarchical (ADRs 0005, 0006).
2. Key, remote name, and `app_name` are separate (ADR 0006).
3. Secrets: git-ignored `secrets/` dotenv files plus `env` and `vault` sources (ADR 0010).
4. Beta kinds: members, custom roles, SSO, and the other identity features are out
   for now; AI providers stay in (ADR 0001).
5. Deploy is its own verb with a per-resource policy (ADR 0012).
6. The engine is replaced, not kept alongside; `96cab73` is the reference, untagged because nothing is stable yet (ADR 0016).
7. Live captures and live suites run in CI, and locally in Docker for debugging
   (ADR 0015).
