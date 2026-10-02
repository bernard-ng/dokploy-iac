# dokploy-iac

Read `CONTEXT.md` (domain language) and `ARCHITECTURE.md` before changing behavior.

## Before finishing

Mirror CI (`.github/workflows/ci.yaml`):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo xtask codegen --check
```

## Rules

- `crates/dokploy-api/src/generated` is generated; never hand-edit it. Regenerate with `cargo xtask codegen`.
- Generated response types must not be used as reconciliation read models (see `dokploy-api` docs).
- Fixtures under `fixtures/api/live` are captured from a real Dokploy; do not hand-edit them.
- `scripts/integration/*` and `compose.integration.yaml` need Docker. Run them in CI, not in cloud sessions.
- Never commit real `DOKPLOY_URL` / `DOKPLOY_API_KEY` values.
