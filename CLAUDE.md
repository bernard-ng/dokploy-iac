# dokploy-iac

Read `CONTEXT.md` (domain language), `ARCHITECTURE.md` (layers and invariants), and the relevant ADRs in `docs/decisions/` before changing behavior.

## Before finishing

Mirror CI (`.github/workflows/ci.yaml`):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo xtask codegen --check
cargo xtask specs --check
cargo xtask goldens --check
cargo xtask versions --check
cargo xtask docs --check
```

## Rules

- The project is in beta. Record every user-visible or developer-facing change in `CHANGELOG.md`, and call out breaking changes explicitly.
- Behavior of a resource kind belongs in its spec under `specs/`, not in engine Rust (ADR 0002). A new field is a spec entry, a new flat kind is a spec file.
- `goldens/` preserves parity evidence from the first engine and inventories test-bearing files (`docs/design/golden-ledger.md`). Keep `cargo xtask goldens --check` green when tests move or change.
- `crates/dokploy-api/src/generated` is generated; never hand-edit it. Regenerate with `cargo xtask codegen`.
- Generated response types must not be used as reconciliation read models (see `dokploy-api` docs).
- Fixtures under `fixtures/api/live` are captured from a real Dokploy; do not hand-edit them.
- `scripts/integration/*` and `compose.integration.yaml` need Docker. Run them in CI, not in cloud sessions.
- Never commit real `DOKPLOY_URL` / `DOKPLOY_API_KEY` values.
