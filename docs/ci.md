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

Headless reconciliation that resolves sensitive values must set
`DOKPLOY_FINGERPRINT_KEY` from a masked secret. The value format is
`<non-nil UUID>:<64 lowercase hexadecimal characters>`. The same value and the
same protected `.dokploy` workspace state must be reused between planning and
apply; a plan artifact alone does not replace either input.

The example [plan workflow](../.github/examples/dokploy-plan.yml) treats exit
status 0 as no changes, status 2 as an applyable plan, and every other status as
a failure. It uploads both redaction-safe JSON review output and the strict
saved-plan envelope. The example
[apply workflow](../.github/examples/dokploy-apply.yml) accepts the approved
plan workflow run ID, downloads that run's envelope, waits for a protected
GitHub Environment, then invokes `dokploy apply PLAN --auto-approve`. The
envelope is not blindly executed: apply rebuilds and compares the plan under
the workspace writer lock using fresh remote reads.

Both examples intentionally select the same self-hosted runner label, fixed
workspace path, and concurrency group. The runner storage must preserve the
entire `.dokploy` directory between jobs and restrict it to the deployment
identity. Store `DOKPLOY_URL`, `DOKPLOY_API_KEY`, and one stable
`DOKPLOY_FINGERPRINT_KEY` as masked secrets. Do not copy only a plan artifact to
an ephemeral runner, rotate the fingerprint key between plan and apply, or
replace the state directory while refreshing `dokploy.yaml`.

Copy the examples into `.github/workflows/` only after adapting the environment
name, runner label, workspace path, secret scope, and repository-specific
configuration path. Configure required reviewers and deployment restrictions
on the `production` GitHub Environment before enabling apply.

Release automation is documented in [Releasing the CLI](releasing.md). The
generated cargo-dist workflow is also checked on pull requests in plan-only
mode and publishes archives, checksums, and installers only for version tags.

Run the equivalent checks locally with:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo xtask codegen --check
scripts/audit-openapi.sh
docker compose --file compose.integration.yaml config --quiet
cargo deny --all-features --locked check
dist generate --mode=ci --check
dist plan
```
