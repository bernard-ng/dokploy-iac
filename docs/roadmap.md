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

Done. Every project kind has a spec and no per-kind Rust: `project`, `environment`, `application`,
`compose`, the six databases (`postgres`, `mysql`, `mariadb`, `mongo`, `redis`, `libsql`), and the leaves
`mount`, `domain`, `port`, `redirect`, `security`, `schedule`, `patch`, `backup`, and `volume_backup`,
with the settings kinds the project kinds select (`tag`, `registry`, `destination`, `network`). All of
them are `full` coverage, so the generic conformance suite runs them under every parent they have, and
the application's `source` and `build`, the Compose `source`, and the LibSQL `node` are also run through
each arm. The hooks the roadmap named are specs: a Compose schedule names its `service_name`, and tag
membership is a relation ([ADR 0019](decisions/0019-sets-of-selectors-relations-and-per-parent-operations.md)).

Exceptions and what the exit leaves to later:

- The `project` root stays `partial`: the generic suite cannot yet write a project root document, so
  its lifecycle (create with variables and tags, update, destroy) is covered by engine tests.
- Verified against a real Dokploy after the merge: `destination`, `network`, `patch`, `volume_backup`,
  and the tags relation of a project were captured on 0.30.6 and 0.30.7 (`capture-destination`,
  `-network`, `-patch`, `-volume-backup`, `-project-tag`), and the live engine test runs `network`
  and `destination` end to end. The captures found three facts the simulator does not model yet:
  `patch.update` adds a trailing newline to `content` (M4, with content digests), removing a
  destination deletes the volume backups that name it, and a second patch of one file path is an
  HTTP 500. The other project kinds (`application`, `compose`, the databases, and the leaves
  captured before M2) still lean on the earlier fixtures; the full live engine suite is M8's.
- The vision ratchet (`docs/vision/gaps/`) counted 2 lines for the project document (`env_file` of the
  application and of the Compose, closed by M4) and counts the settings document's remaining kinds (M5).
- Not modeled, on purpose: database schedules (Dokploy runs schedules in applications and Compose
  only), Compose backups (ADR 0039), Compose `pullImages` (never returned), and the per-service
  `detachDokployNetwork` flag (kept as Dokploy holds it).

## M4 status

Done. `env_file` for the application and the Compose (`file_key` in the spec grammar, the dotenv
expansion in `compile`), which closes the two project-document lines of the vision ratchet; a
variable the document stops naming is removed; a `vault` source sends Dokploy's
`${{vault.provider.secret}}` reference; content is compared with what Dokploy holds through the
keyed receipt of both (`trim` for the `patch`'s newline), so an edit made behind the engine's back
is a drift; `SecretsWriter` writes secrets out below the workspace with `.gitignore` handling; and
the conformance suite supplies every secret from a file and every environment variable from a vault
reference besides the environment, scanning for the canaries as ever.

The value of each environment variable the document or state names is read back from Dokploy's text
and compared as a receipt too, so a variable edited at Dokploy is a drift and is restored, while one
the document never named is left alone. A live engine test (`an_environment_value_and_a_content_edited_at_dokploy_are_drift_the_engine_repairs`,
0.30.6 and 0.30.7) applies a project with an application, an environment, a secret, and a patch to a
real Dokploy, edits the variable and the patch behind the engine's back, and checks the plan sees both,
the repair is exact, and the newline Dokploy adds to a patch is not a difference.

Left to later: the vault provider check (M5, with the `vault_provider` kind) and the importer that
calls `SecretsWriter` (M6).

The live test also found two M3 gaps the simulator hid. `project.create` makes a default `production`
environment, which `environment.create` then refuses to duplicate, `environment.update` refuses to
rename (any body with a `name`), and `environment.remove` refuses to delete: the `made_with_parent`
spec attribute now takes it, never writes its name, and forgets it instead of deleting it, and the
simulator reproduces all three refusals. And an environment made without a description reads `null`
while `environment.update` takes only text, so `description` is nullable (written as an empty text to
clear).
The fingerprint already binds the instance, the address, and the property path, so no separate
fingerprint of paths is needed.

## M5 status

Complete. Every beta settings kind has a spec, sanitized captures on Dokploy 0.30.6 and 0.30.7,
and live engine coverage. The adopt-only `web_server` singleton reads its split settings endpoints,
writes ordinary groups before disruptive groups, manages named Traefik dynamic files, and requires
explicit permission for unattended disruptive applies. `destination`, `registry`, and `network`
omit their `server` field because Dokploy does not store or list it. `custom_roles`, `members`,
`sso_providers`, `profile`, `cluster`, `license`, whitelabeling, and the licensed web-server
`enforce_sso` and `remote_servers_only` controls remain outside the beta.

## M6 status

Complete. `dokploy import settings`, `import project <id>`, and `import all` inventory resources
through the kind specs, allocate deterministic keys, render canonical version 2 documents, and
create adopted state. Secret and content values are written as git-ignored files. Environment
classification supports `auto`, `inline`, `file`, and `skip`; skipped, unsupported, and
unrepresentable values are reported by category. SSO and licensing remain outside the beta.

Before writing, import renders, parses, compiles, and proves that each document has an empty first
plan, then repeats the inventory to detect topology changes. Persistence preflights every destination,
refuses an existing document or state directory, and rolls back files created by a failed write.
Every import writes `dokploy.workspace.yaml` as the public entry point and keeps its documents,
artifacts, secrets, and state below `.dokploy/`. `dokploy plan --all` plans the manifest documents in
order.
The simulator acceptance test imports settings and a nested project, loads the manifest, and proves
that every plan is empty without sending a mutation. The top-level `adopt` directive enrolls one
matching managed resource into an existing workspace while preserving its remote identity.

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
3. Secrets: git-ignored `.dokploy/secrets/` dotenv files plus `env` and `vault` sources (ADR 0010).
4. Beta kinds: members, custom roles, SSO, and the other identity features are out
   for now; AI providers stay in (ADR 0001).
5. Deploy is its own verb with a per-resource policy (ADR 0012).
6. The engine is replaced, not kept alongside; `96cab73` is the reference, untagged because nothing is stable yet (ADR 0016).
7. Live captures and live suites run in CI, and locally in Docker for debugging
   (ADR 0015).
