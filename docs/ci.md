# Continuous integration

The CI workflow uses Rust 1.97.1, the version that compiled the selected
`oas3-gen` 0.28.0 output during the generator bake-off. The repository-level
toolchain file gives contributors and CI the same compiler, formatter, and
linter.

Every push and pull request runs three independent jobs:

- Rust formatting, Clippy with warnings denied, and workspace tests;
- byte-stable regeneration of all OpenAPI bindings with pinned `oas3-gen`, the
  vendored contract audit, and static validation of the local Dokploy Compose
  configuration;
- dependency advisory, license, duplicate-version, wildcard, and source
  checks through `cargo-deny`.

The Compose check only renders and validates `compose.integration.yaml`. It
does not pull images, create secrets, access the Docker socket, or start the
Dokploy integration environment.

GitHub Actions are referenced by immutable commit hashes. Dependency policy is
deliberately narrow: the workspace currently accepts Apache-2.0, MIT, and
Unicode-3.0 licensed code from crates.io. Exact transitive crates have narrow
ISC and MPL-2.0 exceptions where their dependency paths require them. New
sources or broader licenses require an explicit policy review.

Run the equivalent checks locally with:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo xtask codegen --check
scripts/audit-openapi.sh
docker compose --file compose.integration.yaml config --quiet
cargo deny --all-features --locked check
```
