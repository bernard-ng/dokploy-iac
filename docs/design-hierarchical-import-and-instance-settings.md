# Design: hierarchical project import and instance settings

Status: proposed (revision 3). Slices A1 (nested `project.environments`, `DOKCFG032`) through A4 (recursive project import) are implemented; see "A4 as built". Baseline: `master` at `30f26b7`
(Phase 8 complete, ADRs 0001–0048). New ADRs are numbered in the order they
land, starting at 0049.

Goal: configure Dokploy entirely from code, never from the Dokploy UI.

Two tracks, each independently shippable:

- **Track A**: import a whole project (project → environments → every resource)
  in one recursive pass, with environments nested under the project in YAML.
- **Track S**: instance settings as a second declarative document with its own
  state: servers, SSH keys, tags, git providers, registries, DNS providers,
  **vault providers** (Infisical, HashiCorp, AWS, Doppler, Azure, Scaleway,
  Phase), destinations, certificates, notifications, license, and the web
  server (domain, Traefik, cleanup). Environment variables can then reference
  vault secrets with `${{vault.<provider>.<secret>}}`, which Dokploy resolves.

## 0. Decisions and scope

Decided by the user:

- Import selects a **project only**, always recursive. Per-resource import is
  removed. **`--merge` is not supported**: import only creates a new workspace.
- The environment lives **under** the project in YAML.
- Name collisions get an **environment prefix**.
- The imported project keeps the **remote project name**.
- Settings state is **separate** from project state.
- **Tags are in scope**, both as a settings resource and as a project
  association.
- **Web-server settings are in scope**: domain, Traefik, cleanup, and the
  adjacent toggles (builds concurrency, request logging, remote-servers-only,
  server IP), each only if its state can be read back (S6). **SSO is not.**
- **Vault references** are supported in every field Dokploy resolves them in,
  not only application and Compose `environment` (S10).
- `secret` means **`vault_provider`**: Dokploy's native external secret
  manager integration (Settings → Secrets; API `vaultProvider.*`), referenced
  from environment variables as `${{vault.<name>.<secret>}}`.

Out of scope:

- Environment-scoped addresses. Addresses stay global `kind.name` (invariant 11).
- Dokploy users, organizations, billing, passkeys, sessions, SSO.
- Other instance-level API families: `ai`, `customRole`, `whitelabeling`,
  `forwardAuth`, `network` (deferred by the user, "not now").
- Client-side secret resolution by the CLI (see "Secret providers" below).
- Creating a GitHub App from code (Dokploy has no create endpoint for it).
- Running deployments, server provisioning (`server.setup`), connection tests or
  backups from `apply` (consistent with ADRs 0044–0047).
- Re-syncing an already-imported workspace. Resources created in Dokploy after
  import stay unmanaged and untouchable (invariant 21).

Corrections to earlier estimates, found while reading the tree:

- Backup (ADR 0047) and service server placement (ADR 0048) have landed.
  Project import must cover both.
- My earlier claim that the API has no secret endpoint was wrong (my search was
  truncated). `vaultProvider.*` exists, so the client-side "Track P" of
  revision 2 is dropped in favour of a native `vault_provider` resource.
- Compose Schedules are enumerable: upstream `schedule.list` takes only the
  Compose id, so one relaxed SDK read closes the gap (A2).

## 1. Architecture facts that shape the design

| Fact | Where | Consequence |
|---|---|---|
| The planner is kind-agnostic: it consumes `DesiredResource` property maps. | `dokploy-core/src/planner.rs`, `snapshot.rs` | New kinds need property vocabulary and mutation contracts, not planner logic. |
| `PropertyPath` is a closed enum with a per-kind allow list. | `dokploy-core/src/property.rs` (~L270) | Every new kind adds variants and an allow-list arm. |
| `ResourceKind` is a closed enum used by state. | `dokploy-state/src/resource.rs` | New kinds are a state-format change. |
| State is one file per config directory: `<dir>/.dokploy/state.json`, format v3. | `dokploy-state/src/storage.rs:20`, `state.rs` | Two documents in one directory would share a lineage; separate state needs an explicit scope (S2). |
| `DokployConfig` requires exactly one `project`. | `dokploy-config/src/parser.rs` `RawDocument` | Settings need their own document type and discriminator. |
| `discover_remote` is one chain driven by `CompiledDesired`. | `dokploy-cli/src/remote.rs:557` | Settings get their own discovery entry that reuses leaf helpers. |
| Secret descriptors are `SecretSource::{Env, File}`, resolved once inside `compile_desired_for_instance`; exact bytes go to the HMAC fingerprint and a one-shot execution sidecar. Config values are `ConfigValue::{Literal, Reference, Secret}`. | `dokploy-config/src/types.rs:421,543`, `dokploy-cli/src/desired/sensitive_compilation.rs`, ADRs 0012/0013 | Existing descriptors cover every secret the CLI must send. A vault reference is a new non-secret value kind (S10), not a new secret source. |
| Servers, registries, destinations are *external selectors* resolved by name from fresh reads. | ADR 0045, `dokploy-cli/src/external.rs` | They stay valid across documents. Tags (S3) and, for vault assignments, projects and environments (S9) become further selector kinds. |
| Cost of one resource kind: SDK adapter ≈ 2.2–3.3k lines, config/core/state ≈ 2.5–3k, CLI reconcile ≈ 3.7–4k (≈ 60–70% tests). | `git show --stat` of `42b64df`, `2080d05`, `bc95480`, `f43b30d` | Sizing basis. |

---

## Track A: hierarchical, recursive project import

### A1. Schema: environments nest under `project`

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

moves: []     # workspace directives stay at the root
removed: []
```

Changes (all in `dokploy-config`):

- `parser.rs`: `RawProject` gains `environments`; `RawDocument` loses it. The
  loop at `parser.rs:479` iterates `project.environments`; locations and
  diagnostic paths follow the new nesting.
- `writer.rs`: `render` (≈ L795–L820) emits `environments:` inside `project:`
  one level deeper. `ConfigDocument`'s in-memory API (`add_environment`,
  `environment_mut`) is unchanged, so import code keeps compiling.
- `template.yaml`, the generated JSON schema, README/docs examples.
- A legacy top-level `environments:` gets a dedicated diagnostic ("moved under
  `project`") instead of the generic unknown-field error. No compatibility
  shim: the crate is 0.1.0 and the changelog is "Unreleased". Record the break
  in `CHANGELOG.md`.

Fixture migration: ≈ 280 `environments:` occurrences in ≈ 40 files, mostly
raw-string YAML in `dokploy-cli/tests` and `dokploy-config/tests`. Use a
throwaway script (indent each `environments:` block two spaces under
`project:`, leave `moves:`/`removed:` alone), then verify with the full suite
and a diff of `dokploy schema`. Do not commit the script.

### A2. SDK: one new read

`schedules().by_compose(compose_id)` returns every Schedule of a Compose with
its `service_name`. `schedules_by_target` currently rejects a collection whose
target differs from the requested one (`client.rs:1379`), which cannot describe
several services of one Compose. The new read keeps the same bounds (item
limit, unique ids and names, `is_valid`) and requires every item's target to be
`Compose { compose_id, .. }` for the requested Compose. No other SDK change.

### A3. Strangler refactor of the builders (no behavior change)

Today each `build_*` in `dokploy-cli/src/import.rs` takes
`(project, environment, remote, target)` and returns a whole
`ImportedWorkspace`, rebuilding project and environment around one resource.
Replace that with an accumulator:

```rust
struct ImportContext {
    document: ConfigDocument,
    resources: Vec<ImportedResource>,   // address + ResourceState
    names: NameAllocator,               // A5
}

fn add_application(ctx: &mut ImportContext, env: &EnvScope,
                   remote: &ApplicationDetails, assoc: &ImportedAssociations)
                   -> Result<ResourceAddress, ImportError>;
```

- One `add_*` per kind (17, including Backup). Bodies are the existing `build_*`
  bodies minus the project/environment boilerplate, so per-kind semantics
  (protect flags, unmanaged secrets, recorded inputs, dependency edges) are
  preserved verbatim.
- `import_target` (`import/mount.rs`) and the ancestry code in
  `import/schedule.rs` disappear.
- The current single-resource entry points stay as thin wrappers in this
  slice, so every existing import test stays green. That is the safety net for
  A4.

### A4. The recursive crawler

New `dokploy-cli/src/import/project.rs`. Five stages; I/O only in 1 and 5.

1. **Inventory (I/O).** `projects().get(id)` gives environments and, per
   environment, application, Postgres, Redis and LibSQL summaries. Then:
   - per environment: `composes/mysql/mariadb/mongo().by_environment`;
   - per application: details, domains, ports, redirects, security, mounts,
     schedules, external associations;
   - per Compose: details, mounts, schedules (A2);
   - per database (six kinds): details, mounts, and backups for the five
     backup-capable kinds;
   - the external directory is loaded **once** per crawl (today it is loaded per
     application in `imported_associations`).

   Output is a plain `RemoteProject` tree; no YAML yet. Call volume is roughly
   `10 × applications + 4 × databases + 4 × composes + environments`. Start
   sequential and deterministic; add bounded concurrency only if real projects
   are slow.

2. **Validate (pure).** Duplicate remote ids across the tree, empty names,
   direct-read vs authoritative-collection agreement (reuse the existing
   `validate_*_import_authority` helpers and the Mount/Schedule/Backup checks),
   unresolvable or ambiguous external names. Any failure aborts the whole
   import. Bound environments and total resources like the existing
   `INTERACTIVE_LIBSQL_ITEM_LIMIT`.

3. **Name allocation (pure)**: A5.

4. **Build (pure).** Walk in a fixed order (environments by name, then kind
   order, then remote id) calling `add_*`. Then render, re-parse with
   `DokployConfig::parse`, run `compile_desired_for_instance`, and assert every
   managed input in the new state equals its compiled desired value. This is an
   **offline convergence proof**: if the first plan could not be clean, import
   fails before writing. `persist_import` is unchanged (write config,
   checkpoint state, roll the file back if the checkpoint fails).

5. **Re-read (I/O).** Re-fetch `projects().get(id)` and compare the topology
   fingerprint. A change during the crawl returns `ImportError::RemoteChanged`
   and writes nothing.

Policies carried over from today's protected import:

- databases, Compose, Mount, Schedule, Backup, Port, Redirect, Security are
  imported with `protect: true`;
- secrets, Compose documents, Schedule commands, mount file contents, security
  passwords are left **unmanaged** (omitted), never read into config or state;
- external servers/registries/destinations are written as name selectors,
  never ids.

New for import: service server placement (ADR 0048) for all seven service
kinds, not only applications. Project tags are imported once Track S adds them
(S3); until then the project's tags are simply unmanaged, so the first plan
still converges.

### A4 as built

The five stages are implemented as `import/inventory.rs` (stage 1 and the
stage-5 re-read), `import/project.rs` (stages 2 and 4), `import/names.rs`
(stage 3), and `import/converge.rs` (the offline proof inside stage 4). What
differs from the plan above, and why:

- **Convergence proof.** It compares what `compile_desired` produces from the
  rendered document with what `StoredState` projects from the new state:
  identical resource sets, owned properties (both directions, so an input the
  state records but the document leaves unmanaged is caught), containment,
  dependencies, and protection. `StoredState::properties` was added so the
  comparison can see every owned property of a resource.
- **Leaves are read from collections only.** A Port, Redirect, Security entry,
  Domain, Mount, Schedule, or Backup comes from its parent's authoritative
  collection, the same records reconciliation reads. The per-leaf direct read
  and its agreement check are dropped; validation instead checks parentage,
  identity uniqueness across the project, and the natural-key uniqueness the
  planner needs (Redirect regex, Security username). Compose still checks its
  search item against the direct read.
- **Leaf collisions use the parent service as the prefix** (the environment
  prefix cannot separate two leaves of one environment), then `-2`, `-3`.
  Services and environments follow the plan above.
- **Fails closed where the planner cannot plan.** Import refuses a project with
  two same-named resources of one kind in an environment, two same-named
  environments, a project name shared with another project, or a Compose with
  Schedules on more than one service. The last is a standing limit of the
  configuration model (`DOKCFG055`, ADR 0046): A2 added the SDK read, but the
  config rule, the planner's target-scoped read, and the executor still assume
  one service per Compose. Lifting it is separate work. The "Compose with
  Schedules on two services" test case of A7 therefore became a fail-closed case.
- **Re-read.** The topology fingerprint covers the project record, the
  project-scoped environment collection, and the Compose, MySQL, MariaDB, and
  Mongo collections of every environment, ignoring volatile status fields.

### A5. Name allocation

Addresses are `kind.name`, so collisions only matter within one kind.

1. Base name per resource: slug of the remote name (`logical_name`); for leaf
   kinds without one: domain → host; port → `<published>-<protocol>`; redirect
   → regex slug; security → username; mount → path slug; schedule → name;
   backup → `<target-name>-<prefix or destination>`.
2. Group by `(kind, base name)`. In any group larger than one, **every** member
   is prefixed with its environment slug (`application.production-api`,
   `application.staging-api`). Prefixing all members is order-independent and
   gives no resource a privileged name.
3. A prefixed name that still collides gets `-2`, `-3`, … in remote-id order.
   Deterministic for a given remote tree.

The report lists each renamed address with its remote name; users can rename
later with the existing `moves:` mechanism.

### A6. CLI surface

```
dokploy import project [PROJECT_ID] [--file PATH]
dokploy import settings [--file PATH]            # Track S
```

- `import` gains subcommands. `ImportKind` (17 values), `--as` and the
  positional kind/id/address triple are removed.
- With no id, `import project` runs a terminal picker over `projects().all()`
  (one call; same terminal requirement as today). The ≈ 300-line flat
  discovery in `select_with_prompter` is deleted.
- Project address and `project.name` derive from the remote project name via
  `logical_name`.
- Import refuses when the config or state already exists (unchanged). There is
  no merge or refresh mode.
- Output: the existing "Import complete: N resource(s) tracked" line, then a
  per-environment, per-kind table, the renamed addresses, and the list of
  deliberately unmanaged fields.

### A7. Tests

- Rewrite `tests/import.rs`, `mount_import.rs`, `schedule_import.rs`,
  `external_import.rs` (≈ 2,100 lines) as project-level fixtures; most existing
  scenarios become assertions on one subtree of a shared fixture project.
- New cases: colliding names across two environments; double collision; Compose
  with Schedules on two services; every kind in one project; ambiguous external
  name; remote change during the crawl; failure at each stage leaves no config
  and no state; the offline convergence check catching a deliberately broken
  builder; apply-then-plan zero-change on the imported result.
- Live: one create → import → plan-clean round trip in the disposable
  integration suite (`compose.integration.yaml`).

### A8. Slices

| # | Commit | Content | Est. |
|---|---|---|---|
| A1 | `feat(config)!: nest environments under project` | Parser, writer, template, schema, legacy diagnostic, fixture migration | 1–1.5 d |
| A2 | `feat(sdk): read every Schedule of a Compose` | New read + tests | 0.5 d |
| A3 | `refactor(cli): append-style import builders` | `ImportContext`, `add_*`, wrappers; old tests green | 1.5–2 d |
| A4 | `feat(cli): import a whole project` | Crawler, allocator, offline convergence check, re-read, new CLI, test rewrite | 3–4 d |
| A5 | `docs: ADR 0049 recursive project import` | ADR, README, CHANGELOG, amend ADRs 0028–0048 import sections | 0.5 d |

Total ≈ 7–9 days. A1 is the only breaking change and can ship alone.

---

## Secret providers: use Dokploy's native feature (Track P dropped)

Revision 1 and the first half of revision 2 planned a client-side "Track P" in
which the CLI itself would fetch secrets from Infisical. That was based on my
mistaken reading that the API had no secret-provider endpoint. It does:
`vaultProvider.{all,one,create,update,remove,testConnection,listSecretNames}`,
and Dokploy itself resolves `${{vault.<provider>.<secret>}}` references in
environment variables at deploy time (Settings → Secrets in the UI).

So the work is modelled as Dokploy resources, in Track S:

- the `vault_provider` resource (S9), and
- a `vault` reference value for environment variables (S10).

Consequences:

- The secret value never reaches the CLI or CI for environment variables;
  Dokploy resolves it.
- No provider HTTP client, host allow-list, or async secret pre-resolution is
  needed in this repository. That removes about 4.5–6 days from revision 2.
- Fields that Dokploy cannot resolve from a vault (registry password, SSH private
  key, destination keys, certificates, notification tokens, and the vault
  provider's own credentials) keep using the existing `env`/`file` descriptors.
  In CI, a secrets manager can supply those without any CLI change, for example
  `infisical run -- dokploy apply`, which exports them as environment
  variables.
- A client-side `{ provider: ... }` descriptor would only be needed if you want
  non-environment fields read straight from a secret manager without wrapping
  the CLI. I recommend not building it unless that need appears.

---

## Track S: instance settings as a second document

### S1. Document model

The file is discriminated by its top-level key: exactly one of `project:` or
`settings:`. `dokploy_config::load` returns an enum of the two document types;
`plan`, `apply`, `destroy`, `validate`, `import` and `schema` dispatch on it.
`dokploy init --settings` writes a starter. Default file: `dokploy.settings.yaml`.

Layout follows the requested grouping; it is YAML presentation only, and every
resource keeps a flat address `kind.name`.

```yaml
version: 1

settings:
  server:
    ssh_keys:
      deploy:
        description: CI deploy key
        public_key: "ssh-ed25519 AAAA..."
        private_key: { env: DEPLOY_SSH_KEY }
    servers:
      build-1:
        ip_address: 10.0.0.5
        port: 22
        username: root
        type: build                          # deploy | build
        ssh_key: ssh_key.deploy              # typed reference -> dependency edge
        docker_cleanup: true
    tags:
      prod: { color: "#e11d48" }
    git_providers:
      company-gitlab:
        type: gitlab                         # gitlab | gitea | bitbucket | github (adopt only)
        url: https://gitlab.example.com
        application_id: "..."
        secret: { env: GITLAB_APP_SECRET }
  registries:
    ghcr:
      type: cloud
      url: ghcr.io
      username: ci
      password: { env: GHCR_TOKEN }
      image_prefix: acme
  dns_providers:
    cloudflare:
      type: cloudflare
      api_token: { env: CF_TOKEN }
  destinations:
    offsite:
      provider: s3
      bucket: acme-backups
      region: eu-west-1
      endpoint: https://s3.eu-west-1.amazonaws.com
      access_key:        { env: S3_KEY }
      secret_access_key: { env: S3_SECRET }
  certificates:
    wildcard:
      certificate_data: { file: ./certs/wildcard.pem }
      private_key:      { env: WILDCARD_KEY }
      auto_renew: false
  notifications:
    ops-slack:
      type: slack                            # one of 12 provider types
      webhook_url: { env: SLACK_WEBHOOK }
      channel: "#ops"
      events: [app_deploy, app_build_error, database_backup]
  vault_providers:
    infisical-staging:
      type: infisical          # hashicorp | infisical | aws | aws_parameter_store
                               # | doppler | azure | scaleway | phase
      site_url: https://app.infisical.com
      client_id:     { env: INFISICAL_CLIENT_ID }
      client_secret: { env: INFISICAL_CLIENT_SECRET }
      project_id: 6f1c0b5e-...             # Infisical workspace id
      environment_slug: staging
      secret_path: /
      assignments:                         # which Dokploy projects/environments may use it
        - project: shop                    # project name selector
          environments: [production]       # environment name selectors
  license:
    key: { env: DOKPLOY_LICENSE }
  web_server:                                 # singleton, see S6
    domain: { host: dokploy.example.com, https: true, certificate: letsencrypt,
              letsencrypt_email: ops@example.com }
    traefik:
      ports: [ { target: 8080, published: 8080, protocol: tcp } ]
      dashboard: false
      env: { file: ./traefik/env }
      config: { file: ./traefik/traefik.yml }
    cleanup:
      docker: true
      logs: "0 0 * * *"
    builds_concurrency: 2
    request_logging: true
    remote_servers_only: false
    server_ip: 203.0.113.10
```

Resource kinds added to `ResourceKind`: `ssh_key`, `server`, `tag`,
`git_provider`, `registry`, `dns_provider`, `vault_provider`, `destination`,
`certificate`, `notification`, `license`, `web_server`.

Rules:

- Secret-bearing fields are **descriptors only** (`env`, `file`, `provider`);
  literals are a parse error. The planner stores a keyed fingerprint, never
  bytes (invariant 18). Rotation means changing the secret behind the
  descriptor and re-running `apply`.
- Intra-document references (`server.ssh_key`) are typed addresses and become
  dependency edges, like a Mount's `target`.
- Settings resources have **no containment parent**
  (`DesiredResource.containment = None` is already allowed).
- `notification` and `git_provider` are tagged unions (`type:`), like the
  Mount source union, so required fields and secrets are statically known.
- New diagnostics use `DOKCFG070`+ so they cannot collide with project codes.

### S2. State scope

- State format 4 adds `scope: project | settings`. Version 3 files decode as
  `scope = project` and are rewritten on the next checkpoint.
- `StateStore::new(workspace, instance, scope)`: project scope keeps
  `<dir>/.dokploy/`; settings scope uses `<dir>/.dokploy/settings/`. Lock, journal,
  backup and recovery scan are per directory, so a crash in one scope never
  blocks the other.
- `StateFile` rejects resources whose kind does not belong to its scope, and
  the loader rejects a settings state opened as a project state and vice versa.
- `dokploy.yaml` and `dokploy.settings.yaml` can live in the same directory.
- `InstanceIdentity` binding is unchanged and applies to both.

### S3. Tags (settings resource and project association)

Two parts, because a tag is both an instance object and a project attribute.

- **`tag` resource** (settings document): `tag.create/update/remove/all/one`,
  fields `name`, `color`. No secrets. First kind through the new foundation.
- **Project association** (project document): `project.tags` is a list of name
  selectors, `- { name: prod }`, resolved against a fresh `tag.all` exactly like
  server/registry/destination selectors. This adds `SelectorKind::Tag`
  (`dokploy-config/src/types.rs:130`), a tag read in `ExternalDirectory`
  (`external.rs`), and a `Tags` property on Project. Convergence uses
  `tag.assignToProject` / `tag.removeFromProject` (or `bulkAssign` if the
  contract slice proves it atomic). Omitted `tags` leaves associations
  unmanaged; `tags: []` clears them (same null-versus-omitted rule as other
  fields).
- Project import (A4) writes the project's tags as selectors once this lands. The
  SDK's project read projection must be extended to carry tag names; ambiguous or
  duplicate tag names fail closed, like ambiguous server names.

### S4. Config, core and CLI work common to every kind (slice S0)

- `dokploy-config`: `InstanceConfig` (resource map, no parents, locations,
  moves, removed), settings parser module, canonical writer (`InstanceDocument`)
  for import, `dokploy schema --document project|settings`. `ResourceConfig` is
  extended with the new variants so the existing `compile_desired` path and the
  planner consume one uniform type; parsing enforces that each document only
  contains kinds allowed for its scope.
- `dokploy-core`: `PropertyPath` variants and per-kind allow lists; type
  validation in `snapshot.rs`; sensitive classification. Add new secret field
  names (`webhook_url`, `api_token`, `certificate_data`, `license`, traefik env)
  to `SENSITIVE_KEY_SUFFIXES` in `dokploy-state/src/state.rs`.
- `dokploy-cli`: `discover_settings_remote` (own entry, reusing `remote/leaf.rs`
  helpers), an `executor/<kind>.rs` arm per kind, scope-aware `planning.rs`,
  `saved_plan` and recovery.

### S5. Per-kind contracts (hypotheses to prove in each SDK slice)

Each row comes from the generated request bodies. The SDK slice must confirm it
against live Dokploy `v0.30.6` and record it in an ADR, as ADRs 0033 and 0039
did.

| Kind | Create needs | Update accepts | Planned modes | Secrets | Notes |
|---|---|---|---|---|---|
| `tag` | name, color? | name, color | all in place | none | Plus project association (S3). |
| `ssh_key` | name, description?, private key, public key, org | name, description only | name/description in place; **key material replace** | private key | Update body has no key fields; rotation is delete-before-create. Blocked while a server uses it. |
| `server` | name, ip, port, username, ssh key?, type | name, ip, port, username, ssh key, type, cleanup flag | in place where accepted | none | `create` only registers the record; `setup`/`validate` never run by `apply`. `remove` guarded by usage. |
| `registry` | name, username, password, url, type, prefix?, server? | all | in place | password | Already referenced by name from applications. |
| `destination` | name, provider, access key, bucket, region, endpoint, secret key, flags?, server? | all | in place | access key, secret key | Referenced by Backups via selector. |
| `certificate` | name, certificate data, private key, path?, auto-renew?, server?, org | name, data, private key | content in place; `auto_renew`/`server` **replace** | private key | Update body lacks `auto_renew`/`server`. |
| `dns_provider` | name, config union (cloudflare, route53, porkbun, …) | name, config | in place | per-provider token/keys | DNS record management is out of scope. |
| `git_provider` | per provider | per provider | in place | app secret | GitHub: adopt/update only; declaring a create is a validation error. |
| `notification` | provider-specific plus event toggles | same | in place | webhook/token/password | Twelve providers, one union arm each. |
| `license` | key | activate/deactivate | singleton, action-style | key | Enterprise only; semantics decided in its ADR. |
| `web_server` | n/a (singleton) | see S6 | update only | traefik env | See S6. |

Rules copied from the existing adapters:

- Authoritative collection read plus direct-read agreement; collection absence
  is the deletion proof; collision key is the name (plus type where needed).
- Creates without a returned id use a before/after collection diff; ambiguity is
  *outcome unknown*, journaled, never auto-retried (invariant 16).
- `organizationId` comes from a fresh `organization.active` read, never from
  configuration.
- The SDK keeps a minimal safe projection and never deserialises upstream
  secret fields; extend the `secret_error_policy` tests and ADR 0037 sanitising
  to each new endpoint family.
- Import marks every settings resource `protect: true` and leaves secrets
  unmanaged (the importer never reads secret values).

### S6. Web server (domain, Traefik, cleanup)

A singleton kind, address `web_server.<name>` with exactly one allowed per
settings document. It differs from the other kinds in three ways:

- **No create or delete.** Dokploy always has a web server. "Create" is adoption
  and "destroy" only removes it from state; it never resets the remote to
  defaults. Removing the block from config behaves as `removed: destroy: false`.
- **Write endpoints are many and read endpoints are separate.** The surface
  splits across: `assignDomainServer` (host, certificate type, Let's Encrypt
  email, https), `updateTraefikPorts`/`getTraefikPorts`,
  `toggleDashboard`/`haveTraefikDashboardPortEnabled`,
  `writeTraefikEnv`/`readTraefikEnv`,
  `updateWebServerTraefikConfig`/`readWebServerTraefikConfig`,
  `updateMiddlewareTraefikConfig`/`readMiddlewareTraefikConfig`,
  `updateTraefikFile`/`readTraefikConfig`, `updateDockerCleanup`, and
  `updateLogCleanup`/`getLogCleanupStatus`. `getWebServerSettings` has no typed
  response in the OpenAPI document. Per `ARCHITECTURE.md` invariant 4, the SDK
  must define its own tolerant projections from **live captures**
  (`fixtures/api/live`, never hand-edited) before the contract can be written.
  This is the main schedule uncertainty.
- **Disruptive mutations.** Changing the domain, ports or Traefik config
  reloads or restarts Traefik, which can drop the API connection mid-apply or
  lock the operator out. Rules: the plan marks these changes as disruptive and
  requires explicit approval (`--auto-approve` is not implicit for them);
  a connection loss during one is *outcome unknown* and recovery verifies by a
  fresh read instead of retrying; settings with `web_server` should be applied
  in their own step after the other settings. Document this prominently.

Data-handling choices to settle in the contract ADR: Traefik `env` is treated as
sensitive (it can carry DNS-challenge tokens) and compared by fingerprint;
Traefik config files are not secret and are compared by content digest against
the readable remote value. Per-remote-server Traefik settings (the endpoints
accept a `serverId`) are out of the first cut; a server's own cleanup flag is
already part of the `server` kind (`docker_cleanup`).

Adjacent settings, now in scope as long as they are manageable:

| Setting | Write | Read | Status |
|---|---|---|---|
| `builds_concurrency` | `updateBuildsConcurrency` (integer) | none dedicated; presumably in `getWebServerSettings` | include if the live capture shows it |
| `request_logging` | `toggleRequests` (boolean) | `haveActivateRequests` | manageable |
| `remote_servers_only` | `updateRemoteServersOnly` (boolean) | none dedicated; presumably in `getWebServerSettings` | include if the live capture shows it |
| `server_ip` | `updateServerIp` (string) | `getIp` reports the detected address, not necessarily the stored override; presumably `getWebServerSettings` has the stored value | include if the live capture shows the stored value |

Manageability rule: a setting whose current value cannot be read from fresh
remote state cannot be reconciled (invariants 1 and 22), so it is dropped from
the schema rather than shipped as a write-only field. The decision for each is
made from the live capture in the contract slice. `settings.updateServer` has an
empty request body and is an action, not a setting, so it is excluded.

### S7. Import: `dokploy import settings`

Same pipeline as A4: inventory → validate → allocate names → build → offline
convergence check → persist → re-read. Reads: `server.all`, `sshKey.all`,
`tag.all`, `gitProvider.getAll` (+ per-provider `one`), `registry.all`,
`dnsProvider.all`, `destination.all`, `certificates.all`, `notification.all`
(+ per-type `one`), license state and the web-server reads of S6. Name
collisions inside one kind get a numeric suffix (no environment to prefix with).
Server → SSH key references are written as addresses and recorded as
dependencies. Each kind slice adds its own reader and builder, so
`import settings` is never half-wired for a shipped kind. As with projects,
there is no merge: it creates a new settings workspace only.

### S8. Interaction with project documents

- Project documents keep selecting servers, registries and destinations by name
  (ADR 0045), and now tags too. No change to application, service or Backup
  config.
- Apply order is the operator's responsibility: settings first, then projects.
  A project referencing a name that does not exist yet is blocked by the
  existing external-selector diagnostic.
- Renaming a server, registry, destination or tag in the settings document
  renames it remotely and blocks project plans still selecting the old name.
  That fail-closed behaviour is intended; document it.
- CI examples (`.github/examples/dokploy-{plan,apply}.yml`) get a second job pair
  for the settings file with the ordering documented. The scopes make settings
  and projects independent states, so the existing "one job per state" rule
  holds for each.

### S9. Vault providers (`vault_provider`)

Dokploy's native secret-manager integration. API: `vaultProvider.create`,
`update`, `one`, `all`, `remove`, `testConnection`, `listSecretNames`.

Shape of the contract (from the generated request bodies; to be proven live):

- `create` takes `name` (1–64 characters, DNS-label-like pattern), a `config`
  union, and `assignments: [{ projectId, environmentIds? }]`.
- `update` takes the same plus the provider id: a **complete replacement** of
  name, config and assignments.
- `config` has eight arms, selected by `providerType`:

| `type` | Non-secret fields | Secret fields |
|---|---|---|
| `hashicorp` | `url`, `namespace?`, `mount?` | `token` |
| `infisical` | `site_url?`, `client_id`, `project_id`, `environment_slug`, `secret_path?` | `client_secret` |
| `aws` | `region`, `access_key_id`, `endpoint?` | `secret_access_key` |
| `aws_parameter_store` | `region`, `access_key_id`, `endpoint?`, `parameter_path` | `secret_access_key` |
| `doppler` | `project`, `config` | `service_token` |
| `azure` | `vault_uri`, `tenant_id`, `client_id` | `client_secret` |
| `scaleway` | `region`, `project_id`, `api_url` | `secret_key` |
| `phase` | `app_id`, `env`, `path`, `api_url` | `token` |

Design decisions:

- **Tagged union**, like Mount sources and notifications. Secret fields are
  `env`/`file` descriptors only; literals are a parse error. They enter the
  planner as keyed fingerprints and never reach state, plans or logs.
- **Updates need the secret.** Because `update` replaces the whole config, any
  update must resend the provider's secret. Same rule as ADR 0043 for Security
  passwords: an update that has to resend a secret is refused unless the
  descriptor is declared. A provider imported with its secret unmanaged is
  therefore protected and read-only until the user declares the descriptor.
- **Assignments live on the provider**, not on the project. The API mutates them
  only through `update`, which carries the provider credentials; letting each
  project workspace edit assignments would force every project workspace to hold
  every provider's secret, and concurrent project applies would overwrite one
  another's entries. Assignments therefore reference projects and environments
  **by name selector** (`SelectorKind::Project` and `SelectorKind::Environment`,
  the latter scoped to its project), resolved from a fresh `project.all`. Zero or
  several matches block the plan, as for servers.
- **Omitted `assignments`** leaves remote assignments unmanaged: updates resend
  the freshly read remote list (fresh-read complete replacement, as Redirect
  does). `assignments: []` clears them. How an empty `environment_ids` is
  interpreted upstream (every environment or none) must be established with a
  live capture and recorded in the ADR.
- **Bootstrap ordering.** A project must exist before it can be assigned.
  Apply the project document first, then the settings document; or create the
  provider without `assignments` first and add them in a second apply.
- **Never run by `apply`**: `testConnection`. `listSecretNames` is used only for
  advisory validation in S10.
- **Read model.** Whether `vaultProvider.one/all` returns secret fields is unknown.
  The SDK projection must never keep them regardless; if upstream returns them,
  sanitising follows ADR 0037.
- **Delete risk.** Removing a provider breaks every environment variable that
  references it at the next deploy, and the CLI cannot enumerate those (variables
  are a secret-bearing blob). Import therefore marks providers `protect: true`.
  Whether a type change is an in-place update or a replacement is decided in the
  contract ADR.

### S10. Vault references in environment variables

Dokploy resolves `${{vault.<provider>.<secret>}}` inside environment variables
at deploy time, so the CLI never handles the secret value.

```yaml
environment:
  DATABASE_PASSWORD:
    vault: { provider: infisical-staging, secret: DB_PASSWORD }
```

- New `ConfigValue::Vault { provider, secret }`, a **non-secret reference** (not
  `SecretSource`). Compilation renders the exact reference string
  `${{vault.infisical-staging.DB_PASSWORD}}`. Parsing validates the grammar
  (provider name per the API's name pattern, non-empty bounded secret name)
  offline; it cannot check existence because providers live in the settings
  document.
- The `environment` property is read as a presence-only blob, so the rendered
  reference is compared by keyed fingerprint through the existing sensitive
  machinery. No new leak surface, and no change to ADR 0013.
- **Fields that accept references: all that Dokploy resolves.** Dokploy
  documents them for environment variables. The contract slice captures, live,
  which fields honour a reference (candidates: application environment, build
  arguments and build secrets, preview environment, Compose environment, database
  environment, Mount file content) and the config accepts `vault:` in exactly
  those fields. A field that stores a reference but does not resolve it must not
  accept `vault:`. Resolution is verified by a live deploy-free check where
  possible; where only a deploy proves it, the ADR records which fields were
  confirmed and which are documented only.
- **Plan-time checks** against fresh remote state:
  - provider name does not exist (`vaultProvider.all`): blocking diagnostic;
  - provider not assigned to this project/environment, or secret name missing
    from `listSecretNames(provider, project, environment)`: **advisory** only,
    because of the bootstrap ordering above. If the plan model has no
    non-blocking severity, adding one is part of this slice.
- Import does not write environment values (they stay unmanaged), so existing
  references in an imported project are preserved untouched.

### S11. Slices

Each kind follows the repo's three-commit rhythm: `feat(sdk)` (+ contract ADR),
`feat(config)` (config, core, state), `feat(cli)` (remote, executor, import,
recovery, live-verification note).

| # | Slice | Content | Est. |
|---|---|---|---|
| S0 | Foundation + `tag` end to end | Discriminator, `InstanceConfig`, state v4 + scope, scope-aware planning/recovery/saved plans, `init --settings`, `schema --document`, `import settings` skeleton, `tag` kind, `SelectorKind::Tag`, `project.tags`, importer update for tags, ADR | 6–7.5 d |
| S1 | `ssh_key`, `server` | First secrets and first dependency edge in settings, usage guard | 3.5–4.5 d |
| S2 | `registry`, `destination`, `certificate` | Three secret-bearing kinds; two already referenced as selectors | 5–6 d |
| S3 | `dns_provider`, `git_provider` | Config unions; GitHub adopt-only rule | 5–6 d |
| S4 | `notification` | Twelve-arm union, event toggles | 4–6 d |
| S5 | `license` | Singleton/action semantics | 1–1.5 d |
| S6 | `web_server` | Live captures, read projections, singleton semantics, adjacent toggles, disruptive-change guard, recovery | 6–8 d |
| S7 | `vault_provider` + project/environment selectors, then vault references (S9, S10) | Eight-arm union, secret descriptors, assignments, `ConfigValue::Vault` in every resolving field, plan-time checks | 9–11 d |
| — | Docs and CI examples | Per-slice ADRs, README, example workflows, changelog | 2–3 d |

Track S total ≈ 41.5–53.5 days.

---

## Overall plan

Recommended order and milestones (dependencies in brackets):

| Milestone | Contents | Est. | Cumulative |
|---|---|---|---|
| M1 | A1–A5: nested schema, recursive project import | 7–9 d | 7–9 d |
| M2 | S0–S2: foundation, tags (incl. project association), ssh_key, server, registry, destination, certificate | 14.5–18 d | 21.5–27 d |
| M3 | S3–S7 + docs: DNS/git providers, notifications, license, web server (with adjacent toggles), vault providers and vault references | 27–35.5 d | 48.5–62.5 d |

- A and S are independent; M1 and M2 can swap.
- If you want vault providers sooner, S7 only depends on S0 (the foundation and
  the selector extensions), so it can move ahead of S1–S6.
- All providers Dokploy supports (eight arms) are covered by S9 in one slice;
  no extra per-provider cost.
- The estimate is now about 49–63 days, up from my first 25–32 days for settings
  because it includes tags with project association, the web server, and vault
  providers with environment references. The client-side secret track from
  revision 2 (4.5–6 days) was removed.

## Definition of done per kind

Taken from how Phase 8 closed each adapter:

1. Strict config with typed `DOKCFG` diagnostics; JSON schema updated.
2. SDK projection is minimal and tolerant, with redaction tests; mutation
   contract recorded in an ADR.
3. Planner `MutationContract` with create-allowed, create-required, in-place,
   replace and unsupported modes.
4. Fresh authoritative discovery with direct/collection agreement.
5. Journaled, bounded execution; outcome-unknown recovery for creates without a
   returned identity.
6. Protected import and "first plan after import converges".
7. Disposable live test against `compose.integration.yaml` (create → plan clean
   → update → plan clean → delete). Kinds that cannot be exercised on a local
   Dokploy (license, GitHub) rely on mock-server tests and are flagged in their
   ADR.
8. `apply` then `plan` shows no changes (invariant 22).

## Risks

1. **Deleting shared infrastructure breaks running services.** Servers,
   registries, destinations and tags are referenced elsewhere. Mitigation:
   protected import and a delete guard that checks remote usage (each kind's ADR
   names the read that proves "unused").
2. **Locking yourself out through `web_server`.** Domain, port and Traefik
   changes affect the very instance serving the API. Mitigation: disruptive
   classification, explicit approval, outcome-unknown recovery, separate apply
   step.
3. **Removing or reconfiguring a vault provider breaks deploys silently.**
   References live in secret-bearing environment blobs the CLI cannot enumerate.
   Mitigation: protected import, explicit approval for provider deletion, and
   the advisory checks in S10.
4. **Secret drift stays invisible remotely.** Dokploy does not return secrets,
   so only the declared descriptor's fingerprint is compared. A secret rotated
   in a vault is not visible to the CLI at all (Dokploy resolves it at deploy
   time); an out-of-band change of a descriptor-managed secret in Dokploy is not
   detected either.
5. **`PropertyPath` growth.** ≈ 60 variants across the settings kinds. Following
   the typed style keeps security-relevant fields explicit; the alternative is a
   bounded `Attribute(&'static str)` variant with a per-kind table. I recommend
   staying typed at least for sensitive fields.
6. **State format bump.** v4 is forward-only; older binaries refuse new state.
   That is the intended fail-closed behaviour, but the release notes must say so.
7. **Crawl consistency.** Hundreds of reads are not atomic. The final topology
   re-read narrows the window; the failure mode stays closed (write nothing).
8. **Web-server read models are undocumented.** Schedule risk until live captures
   exist.
9. **Untestable kinds.** License and GitHub depend on paid or external setup, so
   they ship with weaker evidence than other kinds.

## Open questions

None blocking. Items to settle inside the slices, by live capture:

1. Which web-server settings can be read back (S6), and therefore stay in the
   schema.
2. Which fields resolve `${{vault...}}` (S10).
3. How an empty `environment_ids` in a vault assignment is interpreted (S9).
