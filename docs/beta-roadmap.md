# Road to a functional beta: gap review and plan

Goal, in the project's own words: **describe a Dokploy instance in YAML so that a
fresh install can be brought to the same state with minimal manual input**, instead
of clicking through the dashboard.

Status: proposal, for review. Baseline: `master` at `96cab73`, Dokploy contract
`v0.30.6` (the screenshots that prompted this review are from `v0.30.7`).

## 1. Verdict

The engine is sound; the **coverage is not**. What exists is a safe reconciliation
core (three-way planner, journaled apply, recovery, saved plans, state scopes) with
a thin slice of Dokploy on top of it. Measured against the goal:

| Area | API surface | Modeled today | Rebuilds a fresh instance? |
|------|-------------|---------------|----------------------------|
| Project-side resource fields | ~345 manageable fields across 17 kinds | ~80 (about 25%) | No |
| Instance settings | ~20 manageable kinds (see section 3) | 1 (`tag`) | No |
| Environment variables | every service kind, project, environment | applications only, and import never writes them | No |
| Deploying what was created | `*.deploy`, `*.redeploy` | none (apply never deploys, by design) | No |
| Proof that "export, apply to empty, export again" is identical | n/a | none | n/a |

Per kind, modeled fields against the API's update body: Application 12 of ~84,
Compose 3 of ~36, PostgreSQL 4 of ~28, MySQL/MariaDB 5 of ~29, MongoDB 4 of ~28,
Redis 2 of ~26, LibSQL 5 of ~30, Domain 2 of ~14, Mount 3 of ~7. Ports, Redirects,
Security entries, Schedules, and Backups are essentially complete.

## 2. What is blocking the goal

Ranked by how much each one prevents, with evidence from your screenshots.

### B1. Import and config drop most of what a service is

Checked against the five screenshots and the code:

| You see in the dashboard | What happens today |
|--------------------------|--------------------|
| Compose domains: `api`, `mercure`, `admin`, `main` with host, path, port, HTTPS, Let's Encrypt | **Not imported.** Import reads domains for applications only (`import/inventory.rs`), and the Domain kind has just `host` and `application`. No `composeId`, `serviceName`, `path`, `port`, `https`, `certificateType`, `stripPath`, `middlewares`, or `customEntrypoint`. |
| Environment tab (Compose, PostgreSQL, applications) | **Not imported.** Import deliberately leaves environment variables unmanaged. Compose and databases have no `environment` field in the config at all; only applications do. |
| Raw Compose file (the whole YAML) | **Not imported.** The document is "left unmanaged" so it can never be reproduced on a new instance. |
| Compose source: GitHub / GitLab / Git / Raw | **Only Raw is expressible**, and only through `document`. `sourceType`, repository, branch, compose path, `autoDeploy`, `pullImages`, isolated deployment, and `composeType` are absent. |
| PostgreSQL Advanced: pinned `ghcr.io/...@sha256` image, command, args | **Not modeled.** `dockerImage`, `command`, `args`, `externalPort`, CPU and memory limits, and all Swarm settings have no config field. |
| Display names (`Leganews Staging Search`) and descriptions | **Names are lost.** Create sends the logical name (`search`) as the remote `name`; there is no `name` field. A rebuilt instance would show different names, and import cannot round-trip a name with spaces. |
| Autodeploy toggle, server badge (`leganews-staging`) | Server is modeled; autodeploy is not. |
| Project tags filter | Modeled (tags landed in this branch series). |

### B2. The secrets policy contradicts the goal

Every rule so far says "never read a remote secret into configuration or state".
That is right for state and plans, but it was applied to the **configuration source
of truth** too, so import drops passwords, environment values, and Compose files,
and a fresh instance cannot be rebuilt from what import produces. There is no
supported way to keep secrets next to the config (encrypted file, Dokploy Vault, or
an `.env` file) and have import write them there. This is a design decision, not a
bug (section 4, D1).

### B3. One field costs about ten edits in six crates

Adding `replicas` touched `property.rs`, `plan.rs`, `model.rs`, `parser.rs`,
`writer.rs`, `sdk/models.rs`, `remote.rs`, `desired.rs`, `executor.rs`,
`import/*`, and their tests. At ~265 unmodeled project fields plus ~20 settings
kinds, hand-writing each is months of work and the main reason progress feels slow.
The pipeline is the right shape for the **hard** kinds, but there is no cheap path
for the **flat** ones (a registry, a destination, a Swarm health-check blob, an
image name).

### B4. Settings are 1 of ~20 kinds, and bootstrap is undefined

Only `tag` exists. The previous plan (Track S, S1 to S7) estimates 41 to 53 days of
hand-written kinds. Separately, nothing says how a brand-new instance gets its
first admin and API key. The API has `user.createApiKey` but it needs a session,
so creating the first admin and key is the irreducible manual step. Also,
`github.create` does not exist: a GitHub App is installed through an OAuth flow, so
it can be adopted but never created from YAML.

### B5. Apply never deploys

Services are created undeployed, and tests assert no `deploy` call is ever made.
That was a deliberate safety choice, but it means "same state as before" yields
stopped services. The model needs an explicit, opt-in way to deploy (D3).

### B6. No proof that the goal is met

There is no test of the actual promise: import instance A, apply that YAML to an
empty instance B, import B, and get an identical document. The live suite proves
per-kind create, update, and plan-clean, never a whole-instance rebuild. Without it
"100% functional" cannot be claimed.

### B7. Process overhead

Each kind follows ADR plus three commits plus live acceptance, and PRs stacked on
each other caused two merge-conflict cycles. Fine for the core; too heavy for ~300
fields and ~20 settings kinds.

### B8. Version drift

The contract is vendored for `v0.30.6`; the instance in the screenshots is
`v0.30.7` with an update pending. There is no compatibility check or matrix.

## 3. Scope of the beta

"Everything the dashboard can manage" includes things that are not infrastructure.
Proposed split:

**In the beta (needed to rebuild a deployable instance):**
`ssh_key`, `server` (register; `setup` stays an action), `registry`, `destination`,
`certificate`, `dns_provider`, `git_provider` (adopt and update, no create),
`notification` (all providers), `vault_provider`, `network`, `web_server`
(domain, Traefik, cleanup, log cleanup, build concurrency), `organization` (name,
logo), `tag`; and on the project side all fields of every service kind, project
and environment `env`, Compose domains and sources, volume backups, and
`deploy`.

**After the beta:** users, invitations, roles, SSO, SCIM, licence, billing,
whitelabeling, AI providers, passkeys and sessions (identity and commercial
features, not infrastructure), preview-deployment settings, patches, and rollbacks.

## 4. Decisions needed (recommendation first)

**D1. Where secrets live.** Recommended: three tiers, all optional.
(a) **Dokploy Vault references** (`${{vault.provider.secret}}`): plain, non-secret
strings, safe in YAML, and the preferred route.
(b) **A secrets file beside the config** (`.env` format, git-ignored, or
sops/age-encrypted by the user): `import --secrets-out secrets/` writes secret
values there and writes only references in the YAML.
(c) Process environment variables (works today).
State, plans, and logs still never contain a secret value. Import classifies
variables as secret by name and by value shape (overridable with `--env=inline|file`).

**D2. Remote names.** Recommended: add `name:` to every named kind, defaulting to
the logical name, so display names round-trip and the logical address stays a
stable, short key.

**D3. Deploying.** Recommended: keep `apply` non-deploying by default; add
`dokploy deploy` (dependency-ordered, plan-first, `--auto-approve`) and a per-
resource `lifecycle.deploy: on_create | on_change | never`, so a rebuild is
`apply` then `deploy`.

**D4. Cost per field.** Recommended: introduce a **declarative field table** for flat
kinds and long-tail service fields: one entry per field (API name, config name,
type, mutability: in-place / create-only / write-only, sensitive?) drives parsing,
writing, schema, projection, update bodies, import, and docs. The existing hand
written engines stay for the complex kinds. This is the single change that makes
the rest of the plan affordable.

**D5. Versions.** Recommended: support the two newest patch releases, fail closed
with a clear message on an unknown minor, and run the live suite against both.

## 5. Plan

Rough effort in working days, in the same units as the earlier design doc; the
first item pays for the rest.

| # | Milestone | Content | Exit criterion | Est. |
|---|-----------|---------|----------------|------|
| M0 | Foundations | D4 field table; D2 `name:`; D1 secrets file source and `--secrets-out`; D5 version check; live-suite harness that can stand up two Dokploy instances | A new flat kind (`registry`) is added in under a day using only the table | 6 to 8 |
| M1 | Service fidelity | All fields of Application, Compose, and the six databases: names, descriptions, `env`, image, command, args, external ports, CPU and memory, Swarm blobs, source types (GitHub, GitLab, Gitea, Bitbucket, Git, Docker image, Raw), build types, autodeploy, Compose document and options; project and environment `env` | Import then plan is empty for a service using every field | 12 to 16 |
| M2 | Domains and networking | Domains on applications and Compose (service name, path, port, HTTPS, certificate type, middlewares, strip path, entrypoint), networks, volume backups | Your four Compose domains import and re-apply | 4 to 6 |
| M3 | Settings, part 1 | `ssh_key`, `server`, `registry`, `destination`, `certificate` on the field table, with `import settings` | Settings round trip against a live instance | 6 to 8 |
| M4 | Settings, part 2 | `dns_provider`, `git_provider`, `notification` (all providers), `vault_provider`, `organization`, `web_server` (with the disruptive-change guard) | Same | 12 to 16 |
| M5 | Deploy | `dokploy deploy`, `lifecycle.deploy`, deploy ordering, status wait with timeout | Rebuilt stack is running | 4 to 6 |
| M6 | Whole-instance acceptance | `dokploy import all` (settings plus every project), `apply all`; live **round trip**: instance A to YAML to empty instance B to YAML, byte-identical; docs and examples for bootstrap; compatibility matrix; cargo-dist beta tag | Round trip green in CI on two Dokploy versions | 6 to 8 |

Total roughly **50 to 68 working days**, versus 41 to 53 for settings alone under the
old plan, because it also closes the project side and adds deploy and the round
trip. Milestones M1/M2 and M3/M4 are independent once M0 lands.

### Order of work and what you can use when

1. **M0 then M1 and M2** (about 5 weeks): your `leganews` project imports with its
   environment, Compose files, domains, images, and names, and re-applies cleanly.
   This is the most visible win and the main gap in your screenshots.
2. **M3 and M4**: servers, registries, destinations, certificates, notifications,
   and web server from YAML.
3. **M5 and M6**: deploy and the rebuild proof, then the beta.

### Beta definition ("100% functional")

- `dokploy import all` on a real instance produces YAML that `plan` reports as
  empty, with no secret in the YAML, state, plans, or logs.
- `apply` plus `deploy` of that YAML on a **fresh** instance, after only the
  documented manual steps, yields an instance whose own `import all` equals the
  first.
- Every kind in section 3's beta list is covered by import, plan, apply, recovery,
  and destroy, with live acceptance.
- Unsupported fields are listed by the importer, never silently dropped.

### Manual steps that remain (and are documented, not hidden)

1. Install Dokploy and create the first admin.
2. Create an API key (`user.createApiKey` needs a session).
3. Install any GitHub App (an OAuth flow; adopted afterwards).
4. Provide secrets through the chosen tier (D1), once.
5. Run server `setup`, which installs software on a host and stays an explicit
   action, never part of `apply`.

## 6. Risks

- **Unreadable settings.** `getWebServerSettings` has no typed response; web-server
  fields need live captures first. Mitigation: capture before designing (already in
  the old S6).
- **Secrets returned by the API.** If Dokploy returns plaintext secrets, `--secrets-out`
  must handle them without logging; ADR 0037 sanitising applies.
- **Disruptive settings.** Changing the server domain or Traefik can cut the API
  connection mid-apply; handled as outcome-unknown, with explicit approval.
- **Schema churn.** The field table must tolerate unknown fields in reads.
- **Scope creep.** The beta list in section 3 is the boundary; anything else waits.
