# Dokploy CLI and Infrastructure as Code

A native Rust toolkit for managing an existing [Dokploy](https://dokploy.com/)
instance from the command line and, progressively, through declarative
infrastructure configuration.

The project combines broad access to Dokploy's API with a safety-focused IaC
engine. It is designed for operators who want automation that remains explicit
about ownership, drift, secrets, remote identity, and recovery.

> [!IMPORTANT]
> This project is pre-release. The imperative CLI and offline configuration
> commands are usable today. Declarative `plan` and confirmed `apply` cover the
> six MVP resource types. Explicit interrupted-operation recovery and
> workspace-wide tracked-resource destruction are available.

## What it offers

### A native Dokploy CLI

- Commands generated for all 604 operations in the pinned Dokploy API contract.
- Named connection contexts with API keys stored in the operating system's
  credential store.
- Structured diagnostics, bounded retries for safe reads, and explicit
  outcome-unknown errors for interrupted mutations.
- Recursive redaction of secret-bearing imperative responses before they reach
  the terminal.

### A declarative engine built for safe reconciliation

- A strict, versioned `dokploy.yaml` format for projects, environments,
  applications, PostgreSQL, Redis, and domains.
- Ownership-aware fields, typed references, dependencies, lifecycle rules,
  moves, removals, and `ignore_changes` semantics.
- Dedicated containment separate from general dependency ordering, persisted
  in strict state format version 3.
- A three-way planner that compares desired configuration, durable state, and
  fresh remote observations.
- Deterministic dependency ordering, drift attribution, protected deletion,
  and fail-closed handling of incomplete or ambiguous remote data.
- Sensitive intent represented by opaque, instance-bound fingerprints rather
  than plaintext values in plans or state.

### An owned Rust SDK

- Generated request bindings kept private behind a handwritten SDK.
- Stable typed identifiers and tolerant runtime response models for critical
  reconciliation reads.
- Contract tests backed by sanitized responses captured from a digest-pinned
  Dokploy `v0.30.6` instance.

## Design principles

**Fresh state over cached assumptions.** Reconciliation reads Dokploy before it
plans. The project does not maintain a persistent API cache.

**Fail closed when evidence is incomplete.** Partial collections, conflicting
identities, uncertain containment, unsupported mutations, and unresolved
recovery records block changes instead of being guessed away.

**Secrets stay out of durable artifacts.** Configuration uses deferred secret
descriptors. Plans, state, diagnostics, journals, fixtures, and debug output are
designed not to retain raw secret values.

**Generated breadth, handwritten stability.** OpenAPI generation provides broad
request coverage. The public SDK owns transport policy, authentication, stable
types, and runtime contracts where the upstream schema is incomplete.

**Recovery is part of mutation design.** State writes are locked and atomic,
with previous-state backups and durable operation journals that prevent new
mutations while an earlier outcome remains unresolved.

## Architecture

| Crate | Responsibility |
| --- | --- |
| `dokploy-api` | Generated request bindings and endpoint metadata from the pinned OpenAPI contract |
| `dokploy-sdk` | Authentication, transport policy, typed resources, and safe runtime models |
| `dokploy-config` | Strict parsing, schema generation, and semantic validation for `dokploy.yaml` |
| `dokploy-state` | Durable resource identity, locking, checkpoints, backups, and operation journals |
| `dokploy-core` | Pure desired/stored/remote snapshots and deterministic planning |
| `dokploy-cli` | Command-line interface and composition of configuration, SDK, state, and planning |

The CLI manages resources through an existing Dokploy API. Installing or
provisioning the Dokploy server itself is outside the project boundary.

## Quick look

Build the CLI with the pinned Rust toolchain:

```bash
cargo build --release -p dokploy-cli
```

Explore the available commands:

```bash
target/release/dokploy --help
target/release/dokploy project --help
```

Create and validate a starter declarative configuration without contacting a
Dokploy instance:

```bash
target/release/dokploy init --empty
target/release/dokploy validate
target/release/dokploy schema > dokploy.schema.json
```

Connection settings can come from command-line overrides, environment
variables, or a selected local context. Use `dokploy context --help` to inspect
the context workflow without exposing stored API keys.

## Project status

The foundation, API/SDK, durable state, configuration language, and core
planner are implemented. Fresh remote projection covers projects,
environments, applications, PostgreSQL, Redis, and application domains.
Adapter-owned contracts classify in-place changes, reparenting, and ordered
replacement without admitting mutation code to the planner.

Phase 6 is complete, including the public confirmed `apply` workflow, durable
per-mutation checkpoints, bounded independent database mutations, multi-step
application configuration, and apply-then-plan convergence.

Preview a workspace without changing Dokploy or creating local state:

```bash
dokploy plan
dokploy plan --json
dokploy plan --json --detailed-exitcode
```

Detailed exit status follows the usual infrastructure-planning convention: 0
for success, 1 for an error or blocked plan, and 2 for an applyable plan with
changes.

Apply always builds and displays a fresh plan under the workspace writer lock.
It proceeds only after the operator types exactly `yes`:

```bash
dokploy apply
dokploy apply --parallelism 4
```

The execution bound accepts values from 1 through 64. Independent Postgres and
Redis mutations may overlap; hierarchy-dependent and multi-step operations stay
ordered. Each successful remote step is checkpointed before dependent work
continues. A failed operation is never rolled back automatically, and already
running successful siblings are still checkpointed before apply returns.

If an apply stops with an unresolved journal, inspect and complete its verified
recovery plan explicitly:

```bash
dokploy recover
dokploy recover --auto-approve
```

Recovery never retries an uncertain create. It adopts only a uniquely observed
resource whose readable owned values match the durable checkpoint. Uncertain
updates involving write-only values or ambiguous observations stop for manual
review.

Destroy renders a fresh dependent-first plan and honors stored protection
before issuing any deletion:

```bash
dokploy destroy
dokploy destroy --auto-approve
```

Only resources owned by the workspace state are considered. An uninitialized
workspace is a no-op and does not create state.

For delivery detail, see the [implementation phases](docs/implementation-phases.md).

## Documentation

- [Implementation phases](docs/implementation-phases.md) — current delivery
  sequence and remaining MVP work.
- [Architecture decisions](docs/decisions/) — design rationale and accepted
  constraints.
- [Integration testing](docs/integration-testing.md) — the disposable,
  digest-pinned Dokploy environment and live contract checks.
- [Phase 0 generator bake-off](docs/phase-0-generator-bakeoff.md) — evidence
  behind the generated/private API boundary.
- [CI and dependency policy](docs/ci.md) — repository validation and supply-chain
  checks.

## Development

Run the primary repository gate before committing implementation changes:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
```

Additional code generation, OpenAPI, fixture, Compose, and dependency-policy
checks are documented in [CI and dependency policy](docs/ci.md).
