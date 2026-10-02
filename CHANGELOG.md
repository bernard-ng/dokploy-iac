# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

- `dokploy-spec` crate and `specs/`: the kind spec format (grammar v0) with a parser,
  structural lint, cross-spec registry, and an OpenAPI coverage ledger, plus working
  specs for `registry`, `redirect`, and partial `application`, `environment`, and
  `project`. `cargo xtask specs --check` runs in CI. Nothing in the engine reads the
  specs yet (milestone M0 of `docs/roadmap.md`).

### Direction

- Reset the documentation to the project's original goal (a Dokploy instance
  described entirely in YAML and rebuilt from it). Removed the first engine's 50
  ADRs, phase plans, design notes, integration-test guide, generator bake-off, and
  README (still in git history at `96cab73`). Added `ARCHITECTURE.md`, `CONTEXT.md`,
  ADRs 0001 to 0017 (all Proposed), the kind spec format, the roadmap, and the
  vision maps. The product is unreleased, so the document format, state format, CLI,
  and Rust APIs may break without migration until the beta (ADR 0001).

### Added

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

### Changed

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

### Added

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
