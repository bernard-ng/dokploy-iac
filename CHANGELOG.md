# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Changed

- Milestone M2 is complete ([`docs/design/cost-gate.md`](docs/design/cost-gate.md)): `tag`,
  `registry`, and `redirect` run end to end with no per-kind Rust or test code (specs of 42, 66, and
  41 lines), against the simulator and the live Dokploy on 0.30.6 and 0.30.7. The union shape is not
  measured yet.
- **Breaking: the rest of the first engine is deleted** (ADR 0016). The `dokploy-config` crate, the SDK's
  per-kind models, services, and client methods (`dokploy-sdk` is now the connection, `Transport`,
  and the raw `api` commands), the first engine's live scripts (`test-*-apply.sh`, `test-*-sdk.sh`,
  `*-evidence*.sh`) and the CI job that ran them are gone. The capture scripts and the fixtures stay:
  they are the evidence the next kinds are ported from.
- **Breaking: the CLI runs on the v2 engine and the first engine is deleted** (milestone M2,
  ADR 0016; no support for first-engine documents, as the product is unreleased).
  `validate`, `schema`, `plan`, `apply`, `recover`, `destroy`, `state`, and `init` read
  `version: 2` documents (`dokploy.yaml` for a project, `dokploy.settings.yaml` for settings), with
  the kind specs compiled into the binary; they cover `tag`, `registry`, and `redirect` until M3
  ports the rest. **Removed:** `import` (returns in M6), saved plans (`plan --out`, `apply PLAN`),
  and `--parallelism` (returns when the executor runs steps concurrently). Deleted: the CLI's
  `remote`, `executor`, `desired`, `import`, `external`, `recovery`, `planning`, `saved_plan`, and
  `sensitive` layers and all their tests. The golden ledger now lets a scenario outlive its test:
  what the first engine asserted stays `pending` as the definition of done for porting its kind.
  `DOKPLOY_FINGERPRINT_KEY` and the per-instance key in the credential store work as before.
- **Breaking: durable state accepts a plain `command`**: the name check on managed inputs no longer
  refuses it, because an application's `command` is public text. Secret-looking names are enforced
  by the spec lint instead, and every other name in the check is unchanged.
- The `application` prototype is found in the collection `environment.one` embeds, not the paged
  `application.search`. The simulator returns a column the client never set with the value a fresh
  resource held in the capture (`replicas: 1`), as a real Dokploy does.
- **Breaking: the kernel carries no closed vocabulary** (ADR 0002, ADR 0016). `dokploy-state` has no
  built-in resource kinds, `DocumentId::Workspace`, or `StateFile::new_in_scope`/`StateStore::new`:
  every kind is registered from a spec, `StateFile::new(version, instance, document)` is the one
  constructor, and a journal that lacks evidence is `RecoveryError::IncompleteEvidence`.
  `dokploy-core` has no per-kind property list: `PropertyPath` is a handle on a spec's
  `PropertyInfo` (`from_spec`), `SpecPath`, `EnvironmentVariableName`, and `PropertyPath::FromStr`
  are gone, and `StoredState::try_from_state` and `compare_resource_observation` take the spec
  registry. **Containment is the address path**: `ChangeKind::Reparent`,
  `MetadataChangeKind::Containment`, `PlanDiagnosticCode::InvalidIgnoredCheckpoint` (`DOKPLAN016`,
  left unassigned), and the mutation contract's containment mode are removed; placing a resource
  under another parent is a `Move`.
- The planner tests are rewritten on three synthetic kinds (`widget`, a nested `gadget`, `cache`;
  `crates/dokploy-core/tests/support`) instead of the first engine's kinds, so they no longer move
  when a spec does. They keep the first engine's coverage of ownership and relinquishing, secret
  receipts, the three-way comparison, protection, dependency order and cycles, moves and removals,
  replacement and mutation contracts, `ignore_changes`, selector resolution, snapshot validation,
  and checkpoint materialization. About 8,000 lines of per-kind tests (mount, schedule, backup,
  tag, placement, redirect, security, and the value rules of ports and sources) are deleted; their
  ledger scenarios stay `pending` until the kind is ported.
- The fingerprint test no longer pins the first engine's vector: its vector is recomputed
  independently (a separate HMAC over the documented framing) for a registry address.

### Added

- Struct members and environment blocks in the executor (milestone M3, ADR 0004;
  [`docs/design/engine.md`](docs/design/engine.md#structs-and-environment-blocks)). A struct planned
  per member (`resources.memory_limit`) reads and writes each member under the key of its own, in
  `partial` and `full` write groups; an `env` field is observed per owned variable (present or not)
  or as a whole, and written by setting the owned variables in the text Dokploy holds, leaving every
  other line as it was. `PropertyInfo` gains `api` and `pattern`. Recovery proves an added variable by
  its presence. The conformance suite renders members and environment blocks (update, drift, canary
  scan) and the simulator nulls struct members and environment columns. The databases gain `resources`
  and `environment`; the application gains `resources`, `environment`, `build_args`, `build_secrets`,
  and `create_env_file` (`application.saveEnvironment` takes all four every time, a `full` group), and
  passes the whole suite with 55 scenarios when its union `source` is left out.

- The databases `postgres`, `mysql`, `mariadb`, `mongo`, and `redis` (milestone M3): five specs of about
  60 lines, no Rust, each found in the collection `environment.one` embeds. They pass the conformance
  suite for the fields they carry (39 to 44 scenarios each; the swarm settings, resource limits,
  networks, and environment block wait for struct and env values in the executor, so they stay
  `coverage: partial`), and 61 of their legacy ledger scenarios are classified. `libsql` waits for the
  decision on unions (its `node`). Whether a password change reaches a running database through
  `update` (as the spec says) or through `changePassword` is to be verified in the live suite.

- Selectors in the executor (milestone M3, ADR 0007; [`docs/design/engine.md`](docs/design/engine.md#selectors)).
  Discovery reads the collection of each selector target kind once and uses it to read ids back as
  names, to resolve the names a document uses (a name that matches nothing or two resources blocks
  the plan), and apply resolves again right before it writes `registryId` and the like. The
  `application` prototype gains `registry`, `build_registry`, and `rollback_registry`. The
  conformance suite seeds the targets of a kind's selector fields, runs update and drift on them,
  and adds `selector_unmatched` and `selector_ambiguous`. The simulator keeps an id given to a
  seeded object.

- The leaf kinds `port` and `security` (milestone M3): two specs of 48 and 44 lines, no Rust. Both pass
  the full conformance suite (26 and 22 scenarios) as children of an application, the vision's
  `ports:` and `security:` blocks now validate (two lines closed in `docs/vision/gaps/project.txt`),
  and 38 of their 39 legacy ledger scenarios are classified `covered` (the other is the live suite).

- Follow-up writes after a create (milestone M3, ADR 0008). A property the create operation does not
  accept (a project kind's create takes a name and little else) is no longer refused: the create is
  a journaled step that expects exactly what it writes, and the spec's write groups write the rest
  as a second journaled step. `ResourceCheckpoint::without` gives the first step's target. The
  conformance suite sends and checks both, and adds `follow_up_rejected`, `follow_up_lost_before`,
  and `follow_up_lost_after` (the resource exists, state describes what the create wrote, recovery
  settles the step, nothing is repeated, one more apply finishes). A trimmed `application`
  (no union or env yet) passes the whole suite as a project kind.
- A spec lint: a public field cannot be named like a secret or a file body (`password`, `token`,
  `api_key`, `content`, `script`, `document`, and so on); declare `class: secret` or `class: content`.
  This is the rule the state's name check enforced at runtime, now stated where the classes live.

- The vision ratchet (milestone M3; `crates/dokploy-model/tests/vision.rs`,
  [`docs/vision/README.md`](docs/vision/README.md)). The two vision documents are now version 2
  files, parsed against the repository's specs on every test run; what the parser rejects is
  recorded in `docs/vision/gaps/` and the test fails when it changes, so each spec that learns a
  field shows up as a closed line. Today: 43 gaps in the project document, 19 in the settings one.

- A live acceptance test of the engine (`crates/dokploy-engine/tests/live.rs`,
  `scripts/integration/test-engine-live.sh`, CI job `engine-live`; milestone M2, ADR 0015). Against the
  digest-pinned Dokploy it applies, converges, updates, detects drift, rotates a secret, recovers, and
  removes a `tag` and a `registry`, including the quirk that a registry whose `docker login` is rejected
  is still saved. It passes on **0.30.6 and 0.30.7**.
- The golden ledger records what the v2 engine now covers (milestone M2, ADR 0016): of the 55
  legacy scenarios for `tag`, `registry`, and `redirect`, **37 are covered** (each names the
  conformance scenario or engine test that proves it), 1 is dropped with its reason, and 17 stay
  pending because they belong to later work (project tag membership and the application `registry`
  selector in M3, import in M6, the live suite in M8). No legacy test is deleted yet. New tests
  close the gaps the classification found: a model test that empty text is refused before any
  request, and a meta-test that a partial collection is never proof of absence.
- Discovery and apply refuse to conclude from reads that contradict themselves (milestone M2, ADR
  0007): a collection that lists an identity twice, holds more than 10,000 items, or names another
  parent's items, and a direct read that names another parent. A removal that Dokploy acknowledges
  while the resource can still be read fails its step (`ApplyError::NotRemoved`) and the resource
  stays tracked. The simulator gains three more ways a real Dokploy misbehaves (`Swallow`,
  `Duplicate`, `tamper`) and the conformance suite four scenarios that use them.
- Nested kinds in the engine (milestone M2, ADR 0007; [`docs/design/engine.md`](docs/design/engine.md)).
  Discovery reads parents before children and reads a child's collection once per parent, scoped by
  it (`list: { op, scope: { param } }`, now implemented in the spec grammar as documented) or embedded
  in the parent's direct read, which costs no extra request. A child of a missing parent is missing
  and of an unavailable parent is unavailable; a kind with no collection read is found by its
  recorded identity only. Apply attaches a child to its parent and learns a nested create's identity
  by diffing the parent's collection. `redirect` now passes the full conformance suite (21
  scenarios) with its ancestors seeded; an apply that is blocked names the diagnostics.
  The prototype `application` spec drops its collection read (`application.search` is paged; M3).
- The conformance suite (`dokploy-conformance`, milestone M2, ADR 0015;
  [`docs/design/conformance.md`](docs/design/conformance.md)). It derives test scenarios from a kind's
  spec and runs them through the real engine against the simulator: create, minimal create,
  convergence, an update per field (exactly its write group is sent), a replacement per `create_only`
  field, drift per field, delete and protected delete, an unmanaged and an ambiguous resource with the
  same identity, an unreadable remote, a declined plan, a rejected create, and a mutation interrupted
  before and after Dokploy applies it for create, update, and delete, each settled by recovery without
  being repeated. Every scenario scans the workspace and its output for canary secrets. `registry`
  and `tag` pass 45 scenarios between them with no per-kind test code, and a new flat kind is enrolled
  by its spec alone. The suite is itself tested by breaking specs on purpose.
- Recovery in `dokploy-engine` (milestone M2, ADR 0008; [`docs/design/engine.md`](docs/design/engine.md#recovery)).
  `Engine::recover` settles an apply that stopped between sending a mutation and recording its
  result, from fresh evidence and never by repeating the mutation: a lost create is adopted by finding
  the resource, a create or update that never arrived is confirmed as no change, a lost delete is
  checkpointed when the resource is gone, and anything it cannot prove (a changed resource, an
  unreadable one, a secret rotation) is left to a person with nothing recorded. The identity of a
  create whose response does not carry one is found the same way. `dokploy-core` gains
  `compare_resource_observation_with_specs` so the comparison works for spec-defined kinds.
- Apply in `dokploy-engine` (milestone M2, ADR 0008; [`docs/design/engine.md`](docs/design/engine.md#apply)).
  `Engine::apply` takes the writer lock, plans from the state it will change, lets the caller
  decline, checks in a preflight that every change can be made, then journals and executes one step
  per change: create (identity learned from the response or by diffing the collection), update by
  write group (`partial`, or `full` from a fresh read), remove, replace (delete then create), rename,
  and adopt. A mutation is sent once; an unknown outcome leaves its step open for recovery; a
  definitive rejection fails the step; and after each step the resource is read back and must read as
  written. Secret values stay in zeroizing memory and never reach the plan, journal, state, or an
  error. Tested against the simulator: create then empty plan, partial and full updates, secret
  rotation, removal, decline, rejection, lost response, failed pre-read, replacement, and a read-back
  mismatch. Composite values, follow-up updates, and create-before-delete are refused in the preflight
  until the project kinds need them.
- `dokploy-sim` (milestone M2, ADR 0015; [`docs/design/simulator.md`](docs/design/simulator.md)): an
  in-memory Dokploy reached through `Transport`, driven only by the kind specs. It checks every
  request against the generated contract, stores objects on create, merges updates (patches),
  removes what hangs below a parent, embeds child collections in their parent's read, filters
  collections by query, and copies response shapes from the recorded fixtures (`registry.one` omits
  the password, `registry.all` lists it, `registry.update` answers `true`). Faults drop the connection
  before or after a request, reject it, or save and then fail (the registry login quirk). Adding a
  flat kind changes nothing in it.
- The `tag` kind (`specs/settings/tag.yaml`): a second flat settings kind, a spec file and nothing
  else. Corrected from live captures: `registry` learns its identity from the create response and its
  update is a patch (no `resend_on_update`), and `redirect` learns its identity by diffing the
  parent's collection, because `redirects.create` answers `true`.
- `DokployError::new` is public, so a `Transport` other than HTTP can answer with a rejection.
- Live captures of the `registry` and `tag` settings kinds, on Dokploy 0.30.6 and 0.30.7
  (`scripts/integration/capture-registry-contract.sh`, `capture-tag-contract.sh`, and the shared
  `capture-lib.sh`; milestone M2, ADR 0007). They pin facts the simulator and the engine depend on:
  `registry.create` runs `docker login` and returns the whole object, password included, and so does
  `registry.all` while `registry.one` omits it; `registry.update` returns `true`, is a patch, and a
  rejected login (HTTP 400) **still persists** the change; registries may share a name. A tag's
  create and update return the object, update is a patch, an explicit `null` clears the color, remove
  returns `{"success": true}`, and a duplicate name is refused. Both versions behave identically.
  The tag capture used to record shapes only and now publishes fixtures; `capture-all.sh` includes it.
- Request contracts in `dokploy-api` (milestone M2, ADR 0008 and 0015). `request_contract("registry.create")`
  returns the query parameters and JSON body properties an operation accepts, and which are
  required, generated from the pinned OpenAPI by `cargo xtask codegen` and kept current by
  `codegen --check`. The engine uses it to decide which fields a `create` carries and which need an
  update, and the simulator to refuse a request the real Dokploy would reject.
- `dokploy-engine`, the skeleton (milestone M1; [`docs/design/engine.md`](docs/design/engine.md)).
  `Engine::compile` turns a validated document into the planner's desired state (hierarchical
  addresses, canonical values, secrets read once, fingerprinted and forgotten, `default: key`,
  `depends_on` by suffix), and `Engine::plan` discovers the remote with the minimum reads and
  plans. **A settings document containing a registry is planned against a canned remote** with no
  first-engine code, and over real HTTP: create, converge after an apply, in-place update, drift,
  secret rotation, unmanaged collision, recreation, and the failure modes (unavailable, invalid or
  disagreeing responses, rejected credentials) are covered. Nothing is written to Dokploy; apply,
  nested kinds, selector resolution, and the CLI follow in later milestones.
- `Transport` in `dokploy-sdk` (milestone M1, ADR 0003 and 0015): the seam the engine sends
  through. A request is an operation id (`registry.all`), a query, and a JSON body; its HTTP
  method comes from the pinned contract and an undeclared operation is refused before any
  request. `Dokploy` implements it over HTTP with everything the client already guaranteed:
  bounded responses, sanitised errors, a mutation never retried, and `OutcomeUnknown` when
  completion cannot be proven. Any other implementor, such as the simulator, plugs in the same
  way, so the engine can run in-process against an in-memory Dokploy.
- `dokploy-model` (milestone M1, ADR 0005; [`docs/design/document-format.md`](docs/design/document-format.md)):
  the version 2 document model. `Document::parse` validates a `project:` or `settings:` document
  against the kind specs and reports every problem with a stable code (`DOKDOC001`-`012`), a
  dotted path, and a line and column; `Document::render` writes the canonical text; `json_schema`
  describes the format for editors. Unknown fields are rejected everywhere, secrets must be
  sources (`env`, `file`, `vault`) and never literals, and a rejected literal is never echoed.
  Randomized tests hold that every document renders, parses back equal, and re-renders to the same
  text, and that the schema accepts what the parser accepts. Nothing reads these documents yet.
- State format 5 (milestone M1, ADR 0009; [`docs/design/state-format.md`](docs/design/state-format.md)).
  Resources are keyed by hierarchical address (`project.shop/environment.staging/application.api`),
  the file records its document (`settings`, `project.<slug>`), and each document has its own
  directory, lock, journal, and backup. `ResourceKind` is open: the first engine's kinds stay
  constants, and spec kinds are registered (`dokploy_core::register_spec_kinds`).
  `SensitivePropertyPath` is a validated dotted path, so spec-declared secrets checkpoint.
  Addresses support suffix resolution and subtree moves. First-engine addresses are one-segment
  paths and keep their text. **Breaking:** formats 3 and 4 are refused (re-import); there is no
  migration. Managed inputs now store a clear (`null`) under any secret-looking key, since a clear
  holds no secret; a value is still rejected wherever it appears. Sensitive collection keys may
  be lowercase.
- Spec-driven property paths (milestone M1). `dokploy-spec` resolves a dotted path against a
  kind spec (`KindSpec::property`, `SpecRegistry::property`): which paths are legal, their
  type, class, mutability, nullability, selector kind, and whether they are required on create;
  keyed `env`/`map` fields give a collection root plus one entry per key. `dokploy-core` gains
  `PropertyPath::Spec`, `MutationContract::from_spec`, and
  `StoredState::try_from_state_with_specs`, and the planner validates values, selectors,
  collection roots, and checkpoints for spec paths with no per-kind code. The closed
  `PropertyPath` variants stay until their kinds are ported (ADR 0016). Spec-declared secrets in
  kinds the closed state grammar does not know plan but cannot yet be checkpointed; state
  format 5 opens that (a test pins the boundary).
- `specs/versions.yaml` and `cargo xtask versions [--check | --image VERSION]`: the Dokploy
  versions the tool knows (0.30.6 `supported`, 0.30.7 `candidate`), each with its digest-pinned
  image (ADR 0014). `--check` is in CI and keeps the compose file and
  `scripts/integration/version.sh` equal to the default pin.
- The manually dispatched **Capture fixtures** workflow (ADR 0015): captures any listed
  version against its pinned image, scans the result for secrets, and uploads the sanitised
  fixtures as an artifact; it never commits. `scripts/integration/capture-all.sh` runs the same
  thing locally. The integration scripts take `DOKPLOY_VERSION` and `DOKPLOY_IMAGE`
  (default 0.30.6), and `check-fixtures.sh` has a `partial` mode for versions being captured.
- `fixtures/api/live/v0.30.7/`: a full capture against a real Dokploy v0.30.7. Apart from
  metadata and generated names, its responses match v0.30.6; `postgres.one` reports a mount's
  `applicationId` as `null` where v0.30.6 reported an application id.
- `cargo xtask goldens` and `goldens/`: a ledger of the 891 first-engine tests that
  define "done" for each ported kind (ADR 0016). Each test is mined (operations,
  expected request lines, fixtures, canned data) and classified `pending`, `covered`,
  or `dropped`; `cargo xtask goldens --check` runs in CI and refuses a legacy test
  that is unlisted, stale, or deleted while still `pending`.

- `dokploy-spec` crate and `specs/`: the kind spec format (grammar v0) with a parser,
  structural lint, cross-spec registry, and an OpenAPI coverage ledger, plus working
  specs for `registry`, `redirect`, and partial `application`, `environment`, and
  `project`. `cargo xtask specs --check` runs in CI. Nothing in the engine reads the
  specs yet (milestone M0 of `docs/roadmap.md`).

- Add a second document type for instance settings. A file with a top-level
  `settings:` key (default `dokploy.settings.yaml`) is planned, applied,
  recovered, and inspected against its own state lineage in `.dokploy/settings/`,
  so it never blocks or reads project state. A document with both `project:` and
  `settings:` is rejected with `DOKCFG070`. `dokploy init --settings` writes a
  starter, `dokploy schema --document settings` prints its schema, and
  `dokploy import settings` adopts the instance's tags into a new settings
  workspace after the same offline convergence proof as project import.
- Add the `tag` kind (`settings.server.tags`), with an optional remote `name`
  and a set-only `color`, plus `project.tags`, a list of tag names a project
  carries. Tags are matched by name and the executor resolves names to
  identities from a fresh `tag.all` immediately before assigning or removing
  them; a desired name that does not exist blocks the plan. Project import
  writes the project's tags. New diagnostics: `DOKCFG071` (invalid tag field),
  `DOKCFG072` (duplicate tag name), `DOKCFG073` (null tag color or project
  tags), `DOKCFG074` (duplicate project tag). The tag response shapes are pinned
  by SDK tests rather than a live capture; run
  `scripts/integration/capture-tag-contract.sh` against a local instance to
  record them. Anything the adapter does not recognise fails closed.

- Add `schedules().by_compose`, a bounded authoritative read of every Schedule
  of one Compose across its services, so whole-project import can enumerate
  Compose Schedules without knowing their service names in advance.
- Add create-only server placement selectors for Compose, PostgreSQL, MySQL,
  MariaDB, MongoDB, LibSQL, and Redis, reusing the ADR 0045 selector
  vocabulary and `DOKCFG029`-`DOKCFG031`: a `server` field (`local: true` or an
  exact `name`, never `null`), observation of the attached server as a name
  selector, `DOKPLAN019` blocking of unmatched, ambiguous, or unreadable names,
  keyed saved-plan receipts that bind the resolved identity, explicit
  create-time placement, journaled delete-before-create replacement on a
  changed placement that is refused while durable state holds contained or
  dependent resources (Mounts, Schedules, Backups, and any other dependent) or
  the service is protected, manual recovery when a selector stops resolving
  uniquely, name-selector import that fails closed on unknown, ambiguous, or
  unreadable servers, and a live acceptance with inert, tripwired server
  records and zero deployments (ADR 0048).
- Add external selector resolution for application server, build-server, and
  runtime, build, and rollback registry associations: a typed local-or-named
  selector vocabulary with strict parsing, `DOKCFG029`-`DOKCFG031`, canonical
  writing, and schema support; fresh minimal `server.all` and `registry.all`
  reads with exact-name matching where zero or multiple matches block the plan
  with `DOKPLAN019`; keyed saved-plan receipts that bind the resolved external
  identities so a removed, renamed, duplicated, or re-created record invalidates
  a saved plan; create-time server placement with in-place nullable registry
  and build-server updates and delete-before-create replacement on a changed
  placement; manual recovery when a selector stops resolving uniquely;
  name-selector import; and a live acceptance with inert, tripwired external
  records (ADR 0045).
- Add end-to-end declarative database Backup reconciliation for PostgreSQL,
  MySQL, MariaDB, MongoDB, and LibSQL targets with a typed target union, an
  external destination selected by exact name, owned schedule, prefix,
  database, enabled, retention, and encryption-key fields, parse-time
  collision-key and malformed-value rejection, authoritative per-target
  `backups` discovery with direct-read agreement, fresh-read complete-replacement
  updates that carry ignored and unmanaged values from the remote, in-place
  destination re-selection, delete-before-create replacement on a target
  change, saved-plan receipts that bind the resolved destination identity,
  journaled outcome-unknown recovery that requires the destination to resolve
  uniquely, protected import with the target ancestry and a destination name
  selector, and a disabled, never-executed, tripwired live acceptance that
  proves zero destination contact (ADR 0047). Compose and web-server Backups
  remain unsupported.
- Add end-to-end declarative Schedule reconciliation for application and
  Compose-service targets with a typed target union, an explicit target-scoped
  name, cron expression, shell, and required `enabled` flag, descriptor-only
  fingerprinted command and script, parse-time duplicate-name, malformed-value,
  and null-executable rejection, authoritative per-target `schedule.list`
  discovery with direct-read agreement, fresh-read complete-replacement updates
  with exact executable proofs, delete-before-create replacement on target or
  service change, journaled outcome-unknown recovery with manual intervention
  for unprovable executable rotation, protected import with the target
  ancestry, and a disabled, never-executed, canary-scanned disposable live
  acceptance (ADR 0046).
- Add end-to-end declarative Mount reconciliation for application, Compose,
  and database targets with a typed target union, bind, volume, and file
  sources, descriptor-only fingerprinted file content, target-scoped
  mount-path collision rejection, authoritative per-target discovery with
  direct-read agreement, in-place path and source updates, delete-before-create
  replacement on target or storage-type change, journaled outcome-unknown
  recovery with manual intervention for unprovable content rotation, protected
  import with the target ancestry, and an undeployed, canary-scanned disposable
  live acceptance (ADR 0044).
- Add end-to-end declarative Redirect reconciliation for application-contained
  regular expression, replacement, and permanence with duplicate regular
  expression rejection, authoritative `application.one` discovery with
  direct-read agreement, fresh-read complete-replacement updates that preserve
  unowned fields, delete-before-create containment replacement, journaled
  outcome-unknown recovery, protected import, and undeployed disposable live
  acceptance.
- Add end-to-end declarative Security (basic-auth) reconciliation for
  application-contained usernames with descriptor-only, fingerprinted
  passwords, duplicate username rejection, authoritative discovery that never
  retains remote passwords, complete-credential updates that fail closed
  without a declared password, delete-before-create containment replacement,
  manual-intervention recovery for unprovable password rotations, protected
  secret-free import, and a canary-scanned undeployed live acceptance.
- Add end-to-end declarative Port reconciliation for application-contained
  published port, target port, publish mode, and protocol with duplicate
  collision rejection, authoritative `application.one` discovery with
  direct-read agreement, fresh-read complete-replacement updates,
  delete-before-create containment replacement, journaled outcome-unknown
  recovery, protected import, and undeployed disposable live acceptance.
- Add end-to-end declarative Compose reconciliation with authoritative bounded
  environment discovery, direct-read agreement, descriptor-only document
  fingerprints, journaled mutations, outcome-unknown recovery, protected
  document-free import, preserve-volume deletion, and undeployed disposable
  live acceptance.
- Add a typed database Backup SDK contract for PostgreSQL, MySQL, MariaDB,
  MongoDB, and LibSQL targets, with safe destination validation, bounded
  authoritative collections, set-difference identity proof, single-attempt
  mutations, transactional sanitized fixtures, and a disabled live lifecycle
  that proves zero deployment, execution, or destination traffic.
- Add end-to-end declarative LibSQL reconciliation with authoritative
  `project.one` topology, direct-read agreement, safe identity proof for the
  no-ID create response, separate metadata and password recovery steps,
  delete-before-create node replacement, protected secret-free import, and
  disposable live acceptance.
- Add bounded, typed, secret-safe external selector reads for Dokploy servers,
  container registries, and backup destinations, with duplicate-ID rejection,
  ambiguity-preserving names, inert live verification, and sanitized fixtures.
- Add end-to-end declarative MongoDB reconciliation with authoritative
  environment-scoped discovery, direct-read agreement, collision detection,
  journaled batched mutations, outcome-unknown recovery, protected secret-free
  import, and disposable live acceptance. Username and replica-set mode update
  in place while post-create password changes remain fail-closed.
- Add end-to-end declarative MariaDB reconciliation with authoritative
  environment-scoped discovery, direct-read agreement, collision detection,
  optional create-only root credentials, journaled batched mutations,
  outcome-unknown recovery, protected secret-free import, and disposable live
  apply-to-delete acceptance. Database and username update in place while all
  post-create credential changes remain fail-closed.
- Add the shared declarative vocabulary for environment-contained MariaDB,
  MongoDB, and LibSQL resources, including strict canonical configuration,
  descriptor-only secret fingerprints, kind-scoped properties, and an atomic
  LibSQL node selection. Remote reconciliation and import remain fail-closed
  until their adapter checkpoints are complete.
- Define typed containment, polymorphic-target, external-selector, sensitive
  input, collision, and create-identity boundaries for the remaining Phase 8
  leaf resource adapters.
- Add a typed Compose SDK contract with safe reads, bounded environment
  discovery, redacted raw-document mutations, explicit volume deletion policy,
  sanitized live fixtures, and an undeployed create-update-delete proof.
- Add a typed application Port SDK contract with nonzero integer ports,
  authoritative parent collection validation, exact single-attempt mutations,
  sanitized live fixtures, and an undeployed all-field lifecycle proof.
- Add a typed application Redirect SDK contract with collision preflight,
  authoritative set-difference identity discovery, direct-parent agreement,
  sanitized live fixtures, and an undeployed all-field lifecycle proof.
- Add a typed application Security SDK contract with zeroizing credential
  inputs, password-presence-only response models, authoritative identity
  discovery, sanitized fixtures, and an undeployed credential lifecycle proof.
- Add a typed Application and Compose Schedule SDK contract with closed targets,
  zeroizing executable inputs, presence-only safe reads, authoritative
  target-scoped validation, transactional sanitized fixtures, and disabled
  undeployed lifecycle proofs.
- Add read-only interactive and non-interactive resource import with canonical
  configuration generation, durable identity adoption, unmanaged secrets,
  protected databases, and immediate plan convergence.
- Add end-to-end declarative MySQL reconciliation with strict environment
  containment, canonical configuration writing, independent user and root
  password fingerprints, fresh discovery, journaled create-update-delete,
  recovery, protected import, and live apply-then-plan convergence. Database
  and username updates reconcile in place while post-create credential changes
  remain fail-closed.
- Enforce independent generated-SDK compilation, explicit RustSec auditing,
  cargo-dist workflow drift checks, release-plan validation, and a disposable
  live Dokploy apply-and-converge job in pull-request CI.
- Add `dokploy plan --out` and `dokploy apply PLAN` with a strict, owner-only
  saved-plan envelope that is revalidated under the writer lock against
  configuration, state lineage and serial, and keyed fresh-remote evidence.
- Add `dokploy recover` with durable checkpoint reconstruction, fresh remote
  verification, exact approval, safe create adoption, and fail-closed handling
  for unreadable or ambiguous interrupted outcomes.
- Add `dokploy destroy` with a fresh dependent-first deletion plan, protection
  enforcement, exact approval, journaled checkpoints, and an absent-state no-op.
- Add atomic state address moves, idempotent protection toggles, and guarded
  state-only forgetting with dependent-reference validation.
- Add `dokploy apply --auto-approve` for explicitly non-interactive execution
  while preserving fresh plan rendering and validation.
- Add offline shell completion generation for Bash, Zsh, Fish, PowerShell, and
  Elvish from the public CLI command tree, with deterministic command coverage
  and installation guidance for every supported shell.
- Add an explicit `DOKPLOY_FINGERPRINT_KEY` source for deterministic,
  keyring-free sensitive-intent receipts in headless CI environments.
- Add typed, single-attempt delete operations for all six MVP SDK resources,
  preserving structured remote rejections and outcome-unknown transport errors.
- Add read-only `dokploy state list` and `dokploy state show` commands with
  workspace-instance validation and value-free managed-field rendering.
- Add owned, read-only Domain SDK operations with strong identifiers and
  fixture-backed models that expose only host and application linkage.
- Add application-scoped Domain discovery with managed-ID matching, exact-host
  collision probes, explicit collection authority, and fail-closed duplicate
  and endpoint-consistency checks.
- Add adapter-projected mutation contracts, explicit reparent actions, stable
  unsupported-transition diagnostics, and protected ordered replacements.
- Add the public read-only `dokploy plan` workflow with single-read source
  digests, deterministic absent-state planning, canonical JSON, human summaries,
  recovery/state revalidation, and detailed exit status.
- Add the first typed mutation SDK path for project creation, preserving both
  the project and Dokploy-created default-environment identities.
- Add planner-selected checkpoint materialization into validated durable
  resource state without exposing non-null sensitive input receipts.
- Add the first journaled executor slice for fresh project creation, including
  exclusive locking, initial lineage creation, per-step checkpoints, and
  apply-then-plan convergence coverage.
- Add typed environment creation and executor handling for both explicit
  environment mutations and safe adoption of Dokploy's project-created default
  environment without a duplicate create.
- Add typed application, Postgres, Redis, and Domain creation with redacted
  secret-bearing inputs, required-create contracts, dependency-ordered
  execution, and a durable checkpoint after every successful resource.
- Add typed in-place updates across all MVP resources, multi-step application
  configuration, deploy-on-runtime-change, state-only lifecycle checkpoints,
  and redacted environment merging that preserves unowned remote variables.
- Add the public `dokploy apply` workflow with fresh plan rendering, exact
  interactive confirmation, a validated execution-bound option, and a concise
  applied-change summary.
- Add durable multi-step execution for all six MVP resource types, bounded
  overlap for independent Postgres and Redis mutations, and partial-failure
  journaling that checkpoints successful in-flight siblings without rollback.
- Add a disposable live apply-and-converge check against Dokploy `v0.30.6`.

- Add first-class containment across desired, stored, checkpoint, ordering, and
  remote-discovery seams. State format version 3 rejects missing or invalid
  containment and older formats instead of inferring parents from general
  dependencies.

- Add the pinned Dokploy OpenAPI 3.1 contract and a reproducible contract audit.
- Add a disposable local Dokploy integration environment.
- Add Phase 0 architecture notes, fixture requirements, and tolerant SDK model prototypes.
- Add the initial `dokploy` CLI with local context selection, safe connection
  resolution, OS credential storage, diagnostics, and tracing.
- Add a pinned Rust toolchain and CI checks for formatting, linting, tests,
  dependency policy, API contracts, and the integration Compose definition.
- Add reproducible `oas3-gen` bindings, generated endpoint metadata, and
  imperative CLI commands for all 604 upstream operations.
- Add owned project, application, and Postgres SDK reads with bounded GET
  retries, tolerant error decoding, and explicit outcome-unknown mutation
  failures.
- Add recursive secret redaction for imperative CLI responses and a live
  regression check against Dokploy `v0.30.6`.
- Add fixture-backed and live SDK contract tests for project, application,
  Postgres, and authentication responses.
- Add the typed Phase 3 state model with canonical resource addresses,
  instance identity, non-sensitive managed inputs, lineage, revisions, and
  serial-checked mutation invariants.
- Add instance-bound local state persistence with fail-fast locking, strict
  decoding, stale-revision protection, owner-only artifacts, atomic writes,
  and previous-state backups.
- Add durable JSONL operation journals with action-bound state transitions,
  append-before-checkpoint ordering, constrained failure codes, strict recovery
  scanning, and mutation refusal while recovery evidence is unresolved.
- Add the strict Phase 4 `dokploy.yaml` model with ownership-aware fields,
  bounded parsing, typed references and containment, descriptor-only secrets,
  lifecycle rules, moves, removals, JSON Schema, and redaction-safe semantic
  diagnostics.
- Add offline `dokploy init`, `dokploy schema`, and `dokploy validate`
  commands with no-clobber initialization, bounded regular-file loading, and
  deterministic schema and diagnostic output.
- Add the first pure Phase 5 planner checkpoint with typed nested property
  paths, value-free sensitive ownership, three-way drift attribution, immutable
  state targets, protected deletion, deterministic redaction-safe plans, and
  fail-closed snapshot validation and partial remote observations.
- Add deterministic dependency-first desired action ordering, dependent-first
  removal ordering, conservative mixed-plan phases, and typed cycle diagnostics
  backed by `petgraph`, with a crate-scoped Zlib license allowance for its
  `foldhash` dependency.
- Add pure-planner move and removal semantics with idempotent declarations,
  explicit target collision probes, atomic move checkpoint targets, retain or
  destroy policies, and redaction-safe two-address diagnostics.
- Add stored-baseline `ignore_changes` planning for existing resources,
  including explicit write exclusions, create and recreate semantics, move
  support, lifecycle-only suppression, and fail-closed selector validation.
- Add a configuration-to-planner compiler for all MVP resource kinds with
  derived containment and reference dependencies, lifecycle directives, and a
  redacted deferred-execution sidecar for logical references. Concrete
  sensitive inputs fail closed until a convergent intent fingerprint exists.
- Add read-only project remote discovery with explicit topology authority,
  pre-transport instance binding, presence-aware descriptions, managed-ID and
  unmanaged-name matching, explicit replacement collisions, move and removal
  probes, fresh reads, duplicate-topology rejection, and redaction-safe failure
  classification.
- Add strict durable sensitive-intent receipts, canonical sensitive property
  paths, raw-value rejection, disjoint clear ownership, and value-free planner
  projection in state format version 2. This is a deliberate pre-release
  incompatibility with version 1 state.
- Add opaque sensitive-intent comparison to the pure planner so matching
  durable receipts converge across write-only remote observations, while new,
  changed, absent, cleared, and relinquished intents remain distinct and plan
  without exposing receipt material.
- Add per-instance sensitive-fingerprint keys in the OS credential store with
  strict versioned envelopes, fail-closed initialization, immediate readback,
  zeroized key buffers, and domain-separated HMAC-SHA-256 receipt calculation.
- Add combined project and environment remote discovery with separate
  authority assertions, parent-scoped exact-name probes, managed-ID and
  containment validation, presence-aware descriptions, move and removal
  support, fresh reads, and secret-safe SDK response models.
- Add instance-bound sensitive configuration compilation with reference
  preflight, one-pass zeroized literal and environment resolution, secure
  bounded workspace-relative file reads, opaque desired receipts, effective
  content-aware digests, and a redacted one-shot execution sidecar.
- Add combined application remote discovery with separately asserted search
  authority, bounded stable pagination, managed-ID and containment checks,
  exact parent-scoped collision probes, move and reparent support,
  presence-aware owned fields, and value-free secret environment projection.
- Add combined Postgres remote discovery with separately asserted search
  authority, bounded stable pagination, managed-ID and containment checks,
  exact parent-scoped collision probes, presence-aware database fields,
  write-only password observations, and fail-closed physical reparenting.
- Add a failure-safe disposable Redis contract capture with owner-only raw
  evidence, deterministic secret-safe fixtures, provenance metadata, and
  three-way post-removal verification without deploying the resource.
- Add combined Redis remote discovery with separately asserted search
  authority, bounded stable pagination, managed-ID and containment checks,
  exact parent-scoped collision probes, write-only password observations, and
  fail-closed physical reparenting.

### Direction

- Reset the documentation to the project's original goal (a Dokploy instance
  described entirely in YAML and rebuilt from it). Removed the first engine's 50
  ADRs, phase plans, design notes, integration-test guide, generator bake-off, and
  README (still in git history at `96cab73`). Added `ARCHITECTURE.md`, `CONTEXT.md`,
  ADRs 0001 to 0017 (all Proposed), the kind spec format, the roadmap, and the
  vision maps. The product is unreleased, so the document format, state format, CLI,
  and Rust APIs may break without migration until the beta (ADR 0001).

### Changed

- The six capture scripts that published straight into `fixtures/api/live/` (compose, libsql,
  mariadb, mongo, mount, mysql) wrote mode 0600 files that the fixture check rejects; they now
  install mode 0644. Capture dates in metadata are the capture day (`DOKPLOY_CAPTURED_AT`
  overrides) instead of a literal.
- **Breaking:** the generated API commands moved under one prefix:
  `dokploy api <resource> <operation> [options]` replaces `dokploy <resource>
  <operation>`. The top level now holds only the declarative commands (`init`,
  `plan`, `apply`, `import`, `state`, ...), so a command's place says whether it
  acts on declared state or makes one direct API call. There is no un-prefixed
  alias. Integration scripts and examples were updated.

- State format 4 records a `scope` (`project` or `settings`). Version 3 files
  decode as project scope and are rewritten on their next checkpoint. Older
  CLIs cannot read format 4.
- **Breaking:** nest `environments` under `project` in `dokploy.yaml`
  (`project.environments`), matching the Dokploy hierarchy and preparing
  whole-project import. A document that still has a top-level `environments`
  key is rejected with `DOKCFG032`. Move the block two spaces deeper under
  `project:`; `moves` and `removed` stay at the document root. Resource
  addresses and state are unchanged, so existing state files stay valid.
- **Breaking:** `dokploy import` now adopts one whole project. The per-resource
  forms (`dokploy import <kind> <id> --as <address>`) and the flat interactive
  resource picker are removed. Use `dokploy import project [PROJECT_ID]`, which
  reads the project, every environment, and every service with all of its leaves
  (domains, ports, redirects, security, mounts, schedules, and backups) in one
  read-only pass, then writes a new configuration and state in a single step.
  Without an id it lists projects to choose from. Import only creates a new
  workspace; it still refuses to run when the configuration or state exists.
  Logical names come from the remote names; a name shared by several resources
  of one kind is prefixed with its environment (services) or its parent service
  (leaves), then numbered if it still collides, and the report lists every
  renamed address. The import now proves offline that the first plan will be
  empty and refuses if the project changed while it was being read. It fails
  closed, before writing anything, on a project that could not plan: two
  same-named resources of one kind in an environment, a project name shared with
  another project, or a Compose with Schedules on more than one service
  (`DOKCFG055`). Service server placement is now imported for Compose and every
  database kind, not only applications.
- Send an explicit `serverId: null` when a LibSQL create leaves placement
  unmanaged. `libsql.create` declares the key required and Dokploy `v0.30.6`
  rejects a body that omits it, so unmanaged LibSQL creation had stopped working
  when placement became optional; every other create still omits the key.
- Apply the dependent-resource replacement refusal to LibSQL replacements caused
  by a node change, which could previously orphan Mounts, Schedules, and Backups.
- Represent SDK server and registry associations with presence-aware
  `ResponseField` values, add explicit unmanaged, local, and external server
  placement for supported creates, and classify unproven post-create placement
  as outcome unknown. This is an intentional pre-release source break: callers
  must replace `None` with `ResponseField::NotReturned` or `ResponseField::Null`
  as appropriate, and `Some(id)` with `ResponseField::Value(id)`.
- Sanitize SDK failure bodies through a fail-closed endpoint policy, including
  shared reads, deploys, deletes, and unknown generated operations, while
  preserving structured errors only for explicitly safe operations and
  documenting request-buffer limits.
- Require Schedule update and delete to prove a supported authoritative target
  before mutation, reject rename collisions, compare exact executable response
  bytes in zeroizing private proofs, and verify create/delete identity deltas.
- Bound every buffered SDK JSON response to 16 MiB with incremental zeroizing
  reads, redaction-safe oversized remote errors, and outcome-unknown
  classification when an accepted mutation response is malformed or too large.
- Require managed Postgres, MySQL, MariaDB, and Redis direct reads to agree
  with a successful parent-scoped collection read before planning mutations,
  and retain redaction-safe live-test evidence whenever cleanup is unproven.
- Classify an unusable MySQL create identity as an unknown outcome in the CLI
  executor so the journal step stays recoverable, matching the SDK contract.
- Validate atomic LibSQL node values at both desired and stored planner seams,
  rejecting malformed tagged shapes and empty replica URLs.
- Separate optional create-only mutation properties from required create
  properties, preserve every uncertain serial or batched mutation as an
  in-progress recovery step, and allow write-only update recovery only when
  sensitive fingerprints prove that no secret rotation was attempted.
- Record the completed Phase 7, 9, and 10 acceptance work and the remaining
  adapter-by-adapter Phase 8 scope in the implementation roadmap.
- Separate command results on standard output from plans, prompts, warnings,
  and diagnostics on standard error, and reject non-interactive apply,
  recovery, and destroy approval unless `--auto-approve` is explicit.
- Reframe the README as a project presentation focused on the product,
  capabilities, architecture, maturity, and contributor entry points.
