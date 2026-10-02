# Design: hierarchical project import and instance settings

Status: proposed, not implemented. Baseline: `master` at `30f26b7` (Phase 8
complete, ADRs 0001–0048). New ADRs would start at 0049.

Goal: configure Dokploy entirely from code, never from the Dokploy UI, for two
kinds of state that today are not covered:

- **Track A**: importing a whole project (project → environments → every
  resource) in one recursive pass, with environments nested under the project in
  the YAML.
- **Track S**: instance-level settings (servers, SSH keys, tags, git providers,
  registries, DNS providers, destinations, certificates, notifications,
  license) as a second declarative document with its own state.

The two tracks are independent. They share only CLI conventions (`import`,
`--file`) and the ADR/slice process.

## 0. Non-goals and decisions already taken

Decided by the user:

- Import selects a **project only**. It is always recursive. Individual
  resource import is removed.
- The environment lives **under** the project in YAML, not as a sibling key.
- Name collisions are resolved with an **environment prefix**.
- The imported project keeps the **remote project name**.
- Settings state is **separate** from project state.

Out of scope:

- Environment-scoped addresses. Addresses stay global `kind.name` (invariant 11).
- Dokploy users, organizations, billing, passkeys, sessions.
- Creating a GitHub App from code. Dokploy exposes no create endpoint for it.
- Triggering deployments, server provisioning, backups or tests of a connection
  from `apply`. Reconciliation never runs these (consistent with ADR 0044–0047).

Corrections to my earlier estimate, found while reading the current tree:

- Backup (ADR 0047) and service server placement (ADR 0048) landed since the
  first analysis. A whole-project import must cover both: backups for the five
  database kinds, and `server` selectors on all seven service kinds, not just
  applications.
- Compose Schedules can be enumerated. The SDK's `schedules_by_target` filters
  to one service, but upstream `schedule.list` takes only the Compose id. A
  relaxed read that returns every service's Schedules removes the gap I flagged
  (see A2).

## 1. Architecture facts that shape the design

| Fact | Where | Consequence |
|---|---|---|
| The planner is kind-agnostic: it consumes `DesiredResource` property maps. | `dokploy-core/src/planner.rs`, `snapshot.rs` | New kinds do not need planner logic, only property vocabulary and mutation contracts. |
| `PropertyPath` is a closed enum with a per-kind allow list. | `dokploy-core/src/property.rs` (~L270) | Every new kind adds variants and an allow-list arm. This is the largest mechanical edit per kind. |
| `ResourceKind` is a closed enum used by state. | `dokploy-state/src/resource.rs` | New kinds are a state-format change. |
| State is one file per workspace directory: `<config dir>/.dokploy/state.json`, format version 3. | `dokploy-state/src/storage.rs:20`, `state.rs` | Two documents in one directory would share one lineage. Separate state needs an explicit scope (B2). |
| `DokployConfig` requires exactly one `project`. | `dokploy-config/src/parser.rs` `RawDocument` | Settings need their own document type and a discriminator. |
| `discover_remote` is a single chain driven by `CompiledDesired`. | `dokploy-cli/src/remote.rs:557` | Settings need their own discovery entry that reuses leaf helpers, not an extension of this chain. |
| Executor dispatches on `ResourceKind`. | `dokploy-cli/src/executor.rs` | One executor module per new kind, following `executor/schedule.rs`. |
| Servers, registries and destinations are *external selectors* resolved by name from fresh reads. | ADR 0045, `dokploy-cli/src/external.rs` | They stay valid across documents. Project config keeps referencing them by name. |
| Cost of one resource kind in this repo: SDK adapter ≈ 2.2–3.3k lines, config/core/state ≈ 2.5–3k, CLI reconcile ≈ 3.7–4k (≈ 60–70% tests). | `git show --stat` of `42b64df`, `2080d05`, `bc95480`, `f43b30d` | Sizing basis for the estimates below. |

---

## Track A: hierarchical, recursive project import

### A1. Schema: environments nest under `project`

Target shape:

```yaml
version: 1

project:
  name: shop
  description: ...
  environments:
    production:
      applications: { api: { ... } }
      postgres:     { main: { ... } }
      domains:      { api-host: { ... } }
      backups:      { nightly: { ... } }
    staging: { ... }

moves: []     # workspace directives stay top-level
removed: []
```

`moves` and `removed` stay at the root because they describe the workspace, not
the project's contents.

Code changes (all in `dokploy-config`):

- `parser.rs`: `RawProject` gains `environments: BTreeMap<String,
  Spanned<RawEnvironment>>`; `RawDocument` loses it. The loop at
  `parser.rs:479` iterates `raw.project.value.environments`. Source locations
  and diagnostic paths follow the new nesting.
- `writer.rs`: `render` (≈ L795–L820) emits `environments:` inside `project:`
  with every child indented one level deeper. `ConfigDocument` already stores
  `environments` separately, so the in-memory API (`add_environment`,
  `environment_mut`) is unchanged and import code keeps compiling.
- `template.yaml`, the generated JSON schema, README/docs examples.
- A legacy top-level `environments:` key gets a dedicated diagnostic
  ("`environments` moved under `project`") instead of the generic unknown-field
  error. The crate is 0.1.0 and the changelog is "Unreleased", so there is no
  compatibility shim; record the break in `CHANGELOG.md`.

Fixture migration: about 280 `environments:` occurrences in about 40 files,
mostly raw-string YAML in `dokploy-cli/tests` and `dokploy-config/tests`. Use a
one-off script, not hand edits: for each YAML literal, indent the
`environments:` block by two spaces under `project:` and leave `moves:`/
`removed:` alone. Verify with the full test suite plus a diff of
`dokploy schema`. Do not commit the script.

### A2. SDK: one new read

Add `schedules().by_compose(compose_id)` returning every Schedule of a Compose
with its `service_name`. Current `schedules_by_target` rejects a collection
whose target differs from the requested one (`client.rs:1379`), which cannot
describe a Compose with Schedules on several services. The new read keeps the
same bounds (item limit, unique ids and names, `is_valid`) and validates that
every item's target is `Compose { compose_id, .. }` for the requested Compose.

No other SDK work is needed. Per-kind collection reads already exist
(`by_environment`, `by_application`, `by_target`).

### A3. Strangler refactor of the builders (no behavior change)

Today each `build_*` in `dokploy-cli/src/import.rs` takes
`(project, environment, remote, target)` and returns a whole
`ImportedWorkspace`, rebuilding the project and environment around one
resource. Replace the boilerplate with an accumulator:

```rust
struct ImportContext {
    document: ConfigDocument,
    resources: Vec<ImportedResource>,   // address + ResourceState
    names: NameAllocator,               // see A5
}

fn add_application(ctx: &mut ImportContext, env: &EnvScope,
                   remote: &ApplicationDetails, assoc: &ImportedAssociations)
                   -> Result<ResourceAddress, ImportError>;
```

- One `add_*` per kind (17 kinds incl. Backup). Their bodies are the existing
  `build_*` bodies minus the `build_project`/`push_environment_state` calls, so
  the per-kind semantics (protect flags, unmanaged secrets, inputs recorded into
  state, dependency edges) are preserved verbatim.
- `import_target` in `import/mount.rs` and the ancestry code in
  `import/schedule.rs` disappear.
- Keep the current single-resource entry points as thin wrappers over the new
  functions in this slice. Every existing import test must stay green. This is
  the safety net for A4.

### A4. The recursive crawler

New module `dokploy-cli/src/import/project.rs`. Pipeline of five stages, with
I/O only in stage 1 and 5.

1. **Inventory (I/O).** `projects().get(id)` gives the environments and, per
   environment, applications, Postgres, Redis and LibSQL summaries. Then:
   - per environment: `composes/mysql/mariadb/mongo().by_environment`;
   - per application: `applications().get`, domains, ports, redirects,
     security entries, mounts, schedules, associations;
   - per Compose: `composes().get`, mounts, schedules (A2);
   - per database (all six kinds): details, mounts, and backups for the five
     backup-capable kinds;
   - external directory loaded **once** for server/registry/destination names,
     not once per application as `imported_associations` does today.

   Output is a plain `RemoteProject` struct tree. No YAML yet.

   Call volume is roughly `10 × applications + 4 × databases + 4 × composes +
   environments`. Start sequential (deterministic, simple); add bounded
   concurrency (`buffered(8)`) only if real projects are slow, since ordering of
   results is already normalised in stage 3.

2. **Validate (pure).** Duplicate remote ids across the tree, empty names,
   direct-read vs authoritative-collection agreement (reuse
   `validate_{port,redirect,security,compose}_import_authority` and the Mount/
   Schedule/Backup checks), unresolvable or ambiguous external names. Anything
   wrong fails the whole import. Add bounds (max environments, max resources)
   alongside the existing `INTERACTIVE_LIBSQL_ITEM_LIMIT` pattern.

3. **Name allocation (pure), see A5.**

4. **Build (pure).** Walk the tree in a fixed order (environments by name,
   then kind order, then remote id) calling the `add_*` functions. Then:
   - render, re-parse with `DokployConfig::parse`, run
     `compile_desired_for_instance`, and assert every managed input in the new
     state equals the compiled desired value. This is an **offline convergence
     proof**: if the first plan could not converge, import fails before writing.
   - `persist_import` is unchanged (write config, checkpoint state, roll back
     the file if the checkpoint fails).

5. **Re-read (I/O).** Re-fetch `projects().get(id)` and compare the topology
   fingerprint (ids per environment). A change during the crawl returns
   `ImportError::RemoteChanged` and writes nothing.

Policies carried over unchanged from today's protected import:

- databases, Compose, Mount, Schedule, Backup, Port, Redirect, Security are
  imported with `protect: true`;
- secrets, Compose documents, Schedule commands, mount file contents, security
  passwords are left **unmanaged** (omitted), never read into config or state;
- external servers/registries/destinations are written as name selectors,
  never ids.

Service placement (ADR 0048) is new to import: `imported_associations` must be
generalised from applications to all seven service kinds.

### A5. Name allocation

Addresses are `kind.name`, so collisions only matter within one kind.

1. Base name per resource: slug of the remote name (`logical_name`), or for
   leaf kinds that have none: domain → host; port → `<published>-<protocol>`;
   redirect → regex slug; security → username; mount → path slug; schedule →
   name; backup → `<target-name>-<prefix or destination>`.
2. Group by `(kind, base name)`. Any group of more than one is a collision:
   **every** member is prefixed with its environment slug
   (`application.production-api`, `application.staging-api`). Prefixing all of
   them is order-independent and avoids a privileged "first" resource.
3. If a prefixed name still collides (or a single remaining duplicate exists
   inside one environment), append `-2`, `-3` in remote-id order. Deterministic
   for a given remote tree.

The import report lists every renamed address with its remote name, so the user
can rename afterwards with the existing `moves:` mechanism.

### A6. CLI surface

```
dokploy import project [PROJECT_ID] [--file PATH]
dokploy import settings [--file PATH]          # Track S
```

- `import` becomes a command with subcommands. `ImportKind` (17 values),
  `--as`, and the positional kind/id/address triple are removed.
- With no id, `import project` runs the interactive picker over
  `projects().all()` (one call; terminal required, same rule as today). The
  ~300-line flat resource discovery in `select_with_prompter` is deleted.
- The project address and `project.name` are derived from the remote project
  name via `logical_name`.
- Refuses when the config or state already exists (unchanged). Re-importing into
  an existing workspace (merge) is explicitly not part of this work; see open
  question 3.
- Output: the existing "Import complete: N resource(s) tracked" line, followed
  by a per-environment, per-kind table, the renamed addresses, and the list of
  deliberately unmanaged fields.

### A7. Tests

- Rewrite `tests/import.rs`, `mount_import.rs`, `schedule_import.rs`,
  `external_import.rs` (≈ 2,100 lines) as project-level fixtures. Most existing
  scenarios become assertions on one subtree of a shared fixture project.
- New cases: two environments with colliding names; double collision; compose
  with Schedules on two services; every kind present in one project; ambiguous
  external name; remote changes during crawl; failure at each stage leaves no
  config and no state; the offline convergence check catching a deliberately
  broken builder; apply-then-plan zero-change on the imported result.
- Live: extend the disposable integration suite (`compose.integration.yaml`)
  with one create-then-import-then-plan round trip.

### A8. Slices

| # | Commit | Content | Est. |
|---|---|---|---|
| A1 | `feat(config)!: nest environments under project` | Parser, writer, template, schema, legacy diagnostic, fixture migration | 1–1.5 d |
| A2 | `feat(sdk): read every Schedule of a Compose` | New read + tests | 0.5 d |
| A3 | `refactor(cli): append-style import builders` | `ImportContext`, `add_*`, wrappers; all old tests green | 1.5–2 d |
| A4 | `feat(cli): import a whole project` | Crawler, allocator, offline convergence check, re-read, new CLI, test rewrite | 3–4 d |
| A5 | `docs: ADR 0049 recursive project import` | ADR, README, CHANGELOG, amend ADRs 0028–0048 import sections | 0.5 d |

Total ≈ 7–9 days. A1 can ship alone and is the only breaking change.

---

## Track S: instance settings as a second document

### S1. Document model

The file is discriminated by its top-level key: exactly one of `project:` or
`settings:`. `dokploy_config::load` returns an enum of the two document types;
`plan`, `apply`, `destroy`, `validate`, `import` and `schema` dispatch on it.
`dokploy init --settings` writes the starter file. Default file name:
`dokploy.settings.yaml`.

Layout follows the requested grouping. It is purely a YAML presentation; every
resource keeps a flat address `kind.name`.

```yaml
version: 1

settings:
  server:
    ssh_keys:
      deploy:
        description: CI deploy key
        public_key: "ssh-ed25519 AAAA..."
        private_key: { env: DEPLOY_SSH_KEY }        # or { file: ./keys/deploy }
    servers:
      build-1:
        ip_address: 10.0.0.5
        port: 22
        username: root
        type: build                                  # deploy | build
        ssh_key: ssh_key.deploy                      # typed reference -> dependency edge
        description: Build host
    tags:
      prod: { color: "#e11d48" }
    git_providers:
      company-gitlab:
        type: gitlab                                 # gitlab | gitea | bitbucket | github (adopt only)
        url: https://gitlab.example.com
        application_id: "..."
        secret: { env: GITLAB_APP_SECRET }
  registries:
    ghcr:
      type: cloud                                    # cloud | self_hosted
      url: ghcr.io
      username: ci
      password: { env: GHCR_TOKEN }
      image_prefix: acme
  dns_providers:
    cloudflare:
      type: cloudflare                               # cloudflare | route53 | porkbun | ...
      api_token: { env: CF_TOKEN }
  destinations:
    offsite:
      provider: s3
      bucket: acme-backups
      region: eu-west-1
      endpoint: https://s3.eu-west-1.amazonaws.com
      access_key: { env: S3_KEY }
      secret_access_key: { env: S3_SECRET }
  certificates:
    wildcard:
      certificate_data: { file: ./certs/wildcard.pem }
      private_key: { file: ./certs/wildcard.key }
      auto_renew: false
  notifications:
    ops-slack:
      type: slack                                    # one of 12 provider types
      webhook_url: { env: SLACK_WEBHOOK }
      channel: "#ops"
      events: [app_deploy, app_build_error, database_backup]
  license:
    key: { env: DOKPLOY_LICENSE }
```

Resource kinds added to `ResourceKind`: `ssh_key`, `server`, `tag`,
`git_provider`, `registry`, `dns_provider`, `destination`, `certificate`,
`notification`, `license`. A later optional kind covers web-server settings.

Rules:

- Secret-bearing fields are **descriptors only** (`env` or `file`), reusing
  `SecretSource` and the opaque sensitive-intent machinery (ADRs 0010, 0012,
  0013). Literal secrets are a parse error. The planner stores a keyed
  fingerprint, never bytes (invariant 18). Rotation means changing the
  descriptor's target and re-running `apply`.
- Intra-document references (`server.ssh_key`) are typed addresses and become
  dependency edges, exactly like a Mount's `target`. Deletion order follows the
  existing `DependencyOrdering`.
- Settings resources have **no containment parent**. `DesiredResource` already
  allows `containment = None`.
- `notification` and `git_provider` are tagged unions (`type:`), like the
  existing Mount source union, so each provider's required fields and secrets
  are statically known.
- New diagnostics use a fresh range, `DOKCFG070`+, so they cannot collide with
  `DOKCFG0xx` codes allocated by project resources.

### S2. State scope

- Bump the state format to 4 and add `scope: project | settings`. Version 3
  files decode as `scope = project` and are rewritten on next checkpoint.
- `StateStore::new(workspace, instance, scope)`:
  - project scope keeps `<dir>/.dokploy/`;
  - settings scope uses `<dir>/.dokploy/settings/`.

  Lock file, journal, backup file and recovery scan are per directory, so a
  crash in one scope never blocks the other.
- `StateFile` validation rejects resources whose kind does not belong to its
  scope, and the loader rejects a settings state opened as a project state (and
  vice versa). A state file copied into the wrong place fails closed.
- `dokploy.yaml` and `dokploy.settings.yaml` can live in the same directory.
- Instance binding (`InstanceIdentity`) is unchanged and applies to both.

### S3. Config, core and CLI changes common to all kinds

Done once, in slice S0:

- `dokploy-config`: `InstanceConfig` (resources map, no parents, locations, moves,
  removed), a settings parser module, a canonical writer (`InstanceDocument`)
  for import, schema output (`dokploy schema --document project|settings`).
  `ResourceConfig` is extended with the new variants so the existing compile
  path (`compile_desired`) and the planner consume one uniform type; the parse
  step checks that a document only contains kinds allowed for its scope.
- `dokploy-core`: `PropertyPath` variants and per-kind allow lists; type
  validation in `snapshot.rs`; sensitive property classification. Add the new
  secret field names (`webhook_url`, `api_token`, `certificate_data`, `license`)
  to `SENSITIVE_KEY_SUFFIXES` in `dokploy-state/src/state.rs`.
- `dokploy-cli`: `discover_settings_remote` (own entry, reuses
  `remote/leaf.rs` helpers), `executor/<kind>.rs` dispatch arm, scope-aware
  `planning.rs` (`prepare_workspace` selects scope from the loaded document),
  scope-aware `saved_plan` and recovery.

### S4. Per-kind contract (to be proven in each kind's SDK slice)

Every row is a hypothesis from the generated request bodies; the SDK slice must
confirm each against live Dokploy `v0.30.6` and record it in its ADR, as ADR
0033/0039 did.

| Kind | Create needs | Update accepts | Planned mutation modes | Secrets | Notes |
|---|---|---|---|---|---|
| `tag` | name, color? | name, color | all in place | none | Also `tag.assignToProject`: later `project.tags` selector list, optional. |
| `ssh_key` | name, description?, private_key, public_key, org | name, description only | name/description in place; **key material = replace** | private key | Update body has no key fields, so rotation is delete-before-create. Blocked when a server uses it. |
| `server` | name, ip, port, username, ssh_key?, type | name, ip, port, username, ssh_key, type, cleanup flag | in place where accepted | none | `create` only registers the record. `setup`/`validate` provisioning are never run by `apply`. `remove` guarded by usage. |
| `registry` | name, username, password, url, type, prefix?, server? | all | in place | password | Selector name already used by applications. |
| `destination` | name, provider, access key, bucket, region, endpoint, secret key, flags?, server? | all | in place | access key, secret key | Referenced by Backups via selector. |
| `certificate` | name, certificate_data, private_key, path?, auto_renew?, server?, org | name, certificate_data, private_key | content in place; `auto_renew`/`server` **replace** | private key | Update body lacks `auto_renew`/`server`. |
| `dns_provider` | name, config union (cloudflare, route53, porkbun, ...) | name, config | in place | per-provider token/keys | Record management (`createRecord`...) is out of scope. |
| `git_provider` | per provider | per provider | in place | app secret | GitHub: **adopt/update only**. Creating it declaratively is a validation error. |
| `notification` | provider-specific, plus event toggles | same | in place | webhook/token/password | Twelve providers; each is one union arm. |
| `license` | key | n/a (activate/deactivate) | singleton, action-style | key | Enterprise only; semantics (activate vs validate) decided in its ADR. |

Cross-cutting rules per kind, copied from the existing adapters:

- Authoritative collection read plus direct-read agreement (`*.all` vs `*.one`);
  collection absence is the deletion proof.
- Collision key: the name (and type where names can repeat across types).
- Creates that return no id use before/after collection diff; ambiguity is
  *outcome unknown*, journaled, never auto-retried (invariant 16).
- `organizationId` comes from a fresh `organization.active` read, never from
  configuration.
- The SDK keeps a minimal safe projection; it never deserialises upstream
  secret fields (extend `secret_error_policy` tests and ADR 0037 sanitising to
  each new endpoint family).
- Import marks every settings resource `protect: true` and leaves secrets
  unmanaged. Settings objects are referenced by running services, so a
  destructive plan must be opt-in per resource.

### S5. Import: `dokploy import settings`

One pass, same pipeline as A4 (inventory → validate → allocate names → build →
offline convergence check → persist → re-read). Reads: `server.all`,
`sshKey.all`, `tag.all`, `gitProvider.getAll` (+ per-provider `one`),
`registry.all`, `dnsProvider.all`, `destination.all`, `certificates.all`,
`notification.all` (+ per-type `one`), license state. Name collisions inside
one kind get a numeric suffix (there is no environment to prefix with).
Server → SSH key references are written as addresses and recorded as
dependencies. The import grows kind by kind: each kind slice adds its reader and
`add_*` builder, so `import settings` is never half-wired for a shipped kind.

### S6. Interaction with project documents

- Project documents keep selecting servers, registries and destinations by name
  (ADR 0045). No change to application, service or Backup config.
- Apply order is the user's responsibility: settings first, then projects. If a
  project references a name that does not exist yet, the project plan is blocked
  by the existing external-selector diagnostic (zero or multiple matches).
- Renaming a server in the settings document renames it remotely and blocks
  project plans that still select the old name. That fail-closed behaviour is
  intended; document it.
- CI examples (`.github/examples/dokploy-{plan,apply}.yml`) get a second job
  pair for the settings file and a documented ordering. Their concurrency note
  already forbids two jobs mutating one state; the scopes make settings and
  projects independent states.

### S7. Slices

Each kind follows the repo's three-commit rhythm: `feat(sdk)` (+ADR contract),
`feat(config)` (config, core, state), `feat(cli)` (remote, executor, import,
recovery, live verification note).

| # | Slice | Content | Est. |
|---|---|---|---|
| S0 | Foundation + `tag` end to end | Document discriminator, `InstanceConfig`, state v4 + scope, scope-aware planning/recovery/saved plans, `init --settings`, `schema --document`, `import settings` skeleton, ADR 0050 | 4–5 d |
| S1 | `ssh_key`, `server` | First secrets in settings, first dependency edge, usage guard | 3.5–4.5 d |
| S2 | `registry`, `destination`, `certificate` | Three secret-bearing kinds; destination/registry already referenced as selectors | 5–6 d |
| S3 | `dns_provider`, `git_provider` | Config unions; GitHub adopt-only rule | 5–6 d |
| S4 | `notification` | Twelve-arm union, event toggles | 4–6 d |
| S5 | `license` (+ optional web-server settings, SSO, trusted origins) | Singleton/action semantics; web-server settings add 3–5 d if wanted | 1–1.5 d (+3–5) |
| — | Docs and CI examples | Per-slice ADRs, README, example workflows, changelog | 2–3 d |

Total ≈ 25–32 days for the full list; the first useful milestone (S0 + S1 + S2:
tag, ssh_key, server, registry, destination, certificate) ≈ 12–15 days.

---

## Definition of done per kind (both tracks)

Copied from how Phase 8 closed each adapter:

1. Strict config with typed `DOKCFG` diagnostics; JSON schema updated.
2. SDK projection is minimal and tolerant, with redaction tests; mutation
   contract recorded in an ADR.
3. Planner mutation contract (`MutationContract`) with create-allowed,
   create-required, in-place, replace and unsupported modes.
4. Fresh authoritative discovery with direct/collection agreement.
5. Journaled, bounded execution; outcome-unknown recovery for creates without a
   returned identity.
6. Protected import and the invariant "first plan after import converges".
7. Disposable live test against `compose.integration.yaml` (create → plan clean
   → update → plan clean → delete). Kinds that cannot be exercised on a local
   Dokploy (license, GitHub, SSO) are covered by mock-server tests and flagged
   as such in their ADR.
8. `apply` followed by `plan` shows no changes (invariant 22).

## Risks

1. **Blast radius of deleting shared infrastructure.** A removed server,
   registry or destination breaks every service that points at it. Mitigation:
   protected import, and a delete guard that checks remote usage (each kind's
   ADR must say which read proves "unused").
2. **Secret drift is invisible.** Dokploy does not return secrets, so a
   changed password cannot be detected remotely. Only the fingerprint of the
   declared descriptor is compared; this is the same trade-off as Mount content
   and Security passwords today.
3. **`PropertyPath` growth.** ~50 variants across the settings kinds. Following
   the existing typed style keeps security-relevant fields explicit; the
   alternative is a bounded `Attribute(&'static str)` variant validated against
   a per-kind table. I recommend staying typed for sensitive fields at least.
4. **State format bump.** Version 4 is forward-only. Old binaries refuse new
   state, which is the desired fail-closed behaviour, but release notes must say
   so.
5. **Crawl consistency.** Hundreds of reads are not atomic. The final topology
   re-read narrows but does not eliminate the window; keep the failure mode
   closed (write nothing).
6. **Untestable kinds.** License, SSO and GitHub depend on paid or external
   setup; they ship with weaker evidence than other kinds.

## Open questions

1. **`secret`.** The API has no secret resource. Did you mean the instance's own
   SSH private key (`settings.saveSSHPrivateKey`), API keys, or just the `env`/
   `file` secret descriptors that already exist? Nothing is planned for it until
   answered.
2. **Layout.** I took your grouping literally (`settings.server.{servers,
   ssh_keys, tags, git_providers}`, everything else flat). Confirm, or choose a
   fully flat layout. It is a parser/writer mapping only.
3. **Re-import.** Importing is no-clobber and only creates a new workspace. Do
   you want a later `import project --merge` that adds newly created remote
   resources to an existing config, or is plan-time visibility of unmanaged
   resources enough?
4. **Web-server settings** (domain, Traefik, cleanup, concurrency, SSO
   enforcement): include in S5, or defer?
5. **Tags on projects.** Include `project.tags` (selector list) in S0, or leave
   `tag` as a standalone resource until you need assignment?
