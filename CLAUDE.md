# dokploy-iac

Read `CONTEXT.md` (domain language), `ARCHITECTURE.md` (layers and invariants), and the ADRs in `docs/decisions/` before changing behavior. The v2 engine is being built kind by kind: see `docs/roadmap.md` for what exists at each milestone.

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
```

## Rules

- The product is unreleased: breaking changes to the CLI, document format, state format, and Rust APIs are allowed (ADR 0001). Record them in `CHANGELOG.md`.
- Behavior of a resource kind belongs in its spec under `specs/`, not in engine Rust (ADR 0002). A new field is a spec entry, a new flat kind is a spec file.
- `goldens/` is the ledger of what the first engine's tests asserted (`docs/design/golden-ledger.md`). The first engine is deleted; a `pending` scenario is the definition of done for porting its kind. When a conformance scenario or engine test covers one, classify it `covered` (naming the test) or `dropped` (with the reason).
- The first engine is gone and gets no support (ADR 0016); commit `96cab73` is the reference for it (a point in history, not a release; there are no tags because nothing is stable yet).
- `crates/dokploy-api/src/generated` is generated; never hand-edit it. Regenerate with `cargo xtask codegen`.
- Generated response types must not be used as reconciliation read models (see `dokploy-api` docs).
- Fixtures under `fixtures/api/live` are captured from a real Dokploy; do not hand-edit them.
- `scripts/integration/*` and `compose.integration.yaml` need Docker. Run them in CI, not in cloud sessions.
- Never commit real `DOKPLOY_URL` / `DOKPLOY_API_KEY` values.
