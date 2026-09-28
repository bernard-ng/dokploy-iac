# Dokploy OpenAPI source

`dokploy.json` is vendored from the official Dokploy SDK repository.

- Repository: <https://github.com/Dokploy/sdk>
- Commit: `da303e6be3afec70f664b48d2c90392f0471515e`
- Commit date: 2026-09-23
- Dokploy source commit: `a0e0574bcd3664ea6ae528fa89ea98300c69e242`
- Dokploy source version: `v0.30.6`
- Source: <https://raw.githubusercontent.com/Dokploy/sdk/da303e6be3afec70f664b48d2c90392f0471515e/openapi.json>
- OpenAPI version: 3.1.0
- SHA-256: `533101f9d106bce0b738ee12eb1111d26b2836e7cddacd80cac95e4863ea82e5`

The SDK artifact is used because the source repository's committed OpenAPI file
is stale; Dokploy generates and synchronizes a fresh document in CI. The
immutable commit URL and checksum keep generator tests reproducible. Do not
replace this document from a moving branch without updating the Phase 0 report
and rerunning both generator evaluations.
