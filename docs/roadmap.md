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
| M0 | Foundations | Tag `engine-v1`. Move the generated commands under `dokploy api` (small, independent). Prototype the spec grammar on three kinds: `registry` (flat), `redirect` (leaf with fresh-read update), application `source` (union). New `dokploy-spec` crate: parse, validate, registry. `cargo xtask specs --check` ledger for the request side against the OpenAPI. Tool to mine legacy tests into golden scenarios. Capture-tool skeleton. | Ledger runs in CI on the three specs; grammar v0 frozen in `docs/design/spec-format.md` | 4–6 |
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
`engine-v1` tag.

| After | Declarative commands cover |
|-------|----------------------------|
| M1 | `validate`, `schema`, `plan` for ported settings kinds against canned data |
| M2 | plan, apply, recover, destroy for `tag`, `registry`, and `redirect` |
| M3 | every project kind (no deploy, no import) |
| M5 | every settings kind |
| M6 | `import` for everything |
| M7 | deploy |
| M8 | the whole beta surface, on two Dokploy versions |

## Constraints and risks

- **Live captures need Docker.** Cloud sessions cannot run it (see `CLAUDE.md`). Live
  fixtures and the live suite run as a CI job (manual dispatch for captures, which
  upload sanitised fixtures for review), or on a machine with Docker. About 40 kinds
  need captures; this is the largest external dependency of the plan.
- **Spec grammar too weak.** If more than a fifth of kinds need hooks, the grammar is
  extended by ADR before continuing.
- **Simulator drift.** Mitigated by running the same scenarios live (ADR 0015); a
  divergence is always a bug in the simulator or the spec.
- **Unreadable settings.** `getWebServerSettings` is untyped; web-server fields need a
  capture before design. Fields that cannot be read back are dropped from the schema.
- **Dokploy churn.** The upstream watch (ADR 0014) turns drift into a CI issue.
- **Scope creep.** The beta kind list in ADR 0001 is the boundary.

## Decisions awaiting review

1. Nesting every child under its service and hierarchical addresses (ADRs 0005, 0006).
2. Splitting key, remote name, and `app_name` (ADR 0006).
3. Secrets default: git-ignored `secrets/` dotenv files plus `env` and `vault` sources
   (ADR 0010).
4. The beta kind list, in particular AI providers in and members/roles out (ADR 0001).
5. Deploy as a separate verb with a per-resource policy (ADR 0012).
6. Replacing the engine instead of keeping two (ADR 0016).
7. How live captures will be run (CI dispatch, or your own instance, read only).
