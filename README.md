# Dokploy infrastructure as code

Describe a [Dokploy](https://dokploy.com/) instance in YAML, keep it in a
repository, and rebuild it from that description instead of re-creating it through
the dashboard: Docker Compose, but for Dokploy itself.

> [!IMPORTANT]
> **Pre-release, mid re-engineering.** The first engine has been deleted (it is preserved at
> commit `96cab73`, a reference point in history, not a release) and the spec-driven engine that
> replaces it is being built kind by kind. Today `dokploy validate`, `schema`, `plan`, `apply`,
> `recover`, and `destroy` work on version 2 documents for `tag`, `registry`, and `redirect`
> (`dokploy init` writes a starter). The rest of the dashboard arrives with the milestones in
> [`docs/roadmap.md`](docs/roadmap.md); `import` returns in M6.

## Where this is going

```
workspace/
  dokploy.workspace.yaml     documents and their order
  settings.yaml              servers, registries, destinations, notifications, web server, ...
  projects/leganews.yaml     environments, services, domains, env vars, backups, ...
  files/                     compose files, configs, scripts
  secrets/                   dotenv files, git-ignored
```

```bash
dokploy import all           # adopt an existing instance into this workspace
dokploy plan --all           # what would change, secret-free
dokploy apply --all --deploy # reconcile settings, then projects, then deploy
```

Applying the workspace to a fresh instance, after a few documented manual steps
(install Dokploy, create the admin and an API key), reproduces the instance;
importing the result gives an equivalent workspace. That round trip is the beta's
acceptance test. The field-level target is the pair of annotated maps in
[`docs/vision/`](docs/vision/): [`dokploy.full.yaml`](docs/vision/dokploy.full.yaml)
(a project) and [`dokploy.settings.full.yaml`](docs/vision/dokploy.settings.full.yaml)
(instance settings).

## How it works

- **Kind specs** describe every Dokploy resource once, as data: fields, types,
  secrets, mutability, endpoints, write groups. The engine is generic over them, so
  parsing, planning, applying, importing, schema, docs, and tests all come from one
  source. A CI ledger checks that every API field is classified.
- **A safe kernel**: a pure three-way planner, durable
  state with journals and recovery, fingerprinted secrets, and stale-state and
  instance-binding protection.
- **Honest about secrets**: configuration holds references; state holds fingerprints;
  import writes secrets to git-ignored files and never into YAML.

Architecture and rules: [`ARCHITECTURE.md`](ARCHITECTURE.md). Vocabulary:
[`CONTEXT.md`](CONTEXT.md). Decisions: [`docs/decisions/`](docs/decisions/).

## Documentation

- [Roadmap](docs/roadmap.md): milestones, gates, risks, open decisions.
- [Architecture decisions](docs/decisions/) and [kind spec format](docs/design/spec-format.md).
- [Vision maps](docs/vision/): every Dokploy field, annotated.
- [CI and dependency policy](docs/ci.md), [Releasing](docs/releasing.md),
  [Shell completions](docs/shell-completions.md).

## Building today

```bash
cargo build --release -p dokploy-cli
target/release/dokploy --help
```

The imperative commands (`dokploy api <resource> <operation>`, generated from the Dokploy
OpenAPI document) act on Dokploy directly. The declarative commands read `version: 2`
documents; a first-engine `version: 1` document is refused with a message to rewrite it.

Before committing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo xtask codegen --check
```

## Status of the design

ADRs 0001, 0005, 0006, and 0016 are Accepted; the rest are **Proposed** and awaiting review. The product is
unreleased, so any of this can change without migration (ADR 0001).
