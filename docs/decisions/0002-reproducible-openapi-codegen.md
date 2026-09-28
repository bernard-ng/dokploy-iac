# ADR 0002: Reproducible OpenAPI code generation

## Status

Accepted for the initial MVP.

## Context

ADR 0001 selected `oas3-gen` for low-level request and endpoint bindings. The
generator is an external binary rather than a library, its operation filter can
silently emit zero operations when given unnormalized identifiers, and its
output changes under `rustfmt`.

The pinned Dokploy contract contains 25 regular expressions that `oas3-gen`
cannot translate to Rust's regex dialect. Every operation also overrides the
valid root `apiKey` requirement with an undefined `Authorization` security
scheme. Generated clients do not implement authentication, but downstream
contract tooling must receive a valid document.

## Decision

Commit the generated bindings under `crates/dokploy-api/src/generated` and
regenerate them only through:

```bash
cargo install oas3-gen --version 0.28.0 --locked
cargo xtask codegen
```

`cargo xtask codegen`:

- rejects every generator version except `oas3-gen 0.28.0`;
- reads the vendored contract without modifying it;
- writes a scratch copy that replaces each undefined operation-level
  `Authorization` security reference with the already-defined `apiKey` scheme;
- generates all operations into temporary storage;
- accepts only the 25 known unsupported-regex warnings and rejects other
  diagnostics;
- compares the number of generated methods with the number of declared
  `operationId` values and rejects zero or partial output;
- formats generated Rust with the repository's pinned toolchain;
- adds a repository-owned `DO NOT EDIT` header; and
- writes only the four expected generated files, including repository-owned
  endpoint method and path metadata.

`cargo xtask codegen --check` performs the same generation and fails on any
byte-level drift without changing committed files.

Authentication references are the only OpenAPI normalization. Tests prove that
all 604 operations and their response objects are preserved and that the
vendored source remains byte-identical. Response schemas are never invented or
enriched. A future required normalization must be an explicit, narrow
transformation with a regression test proving that unrelated contract content
is preserved.

The generated `DokployApiClient` accepts an existing `reqwest::Client`. The
owned SDK installs `x-api-key` as a default header on that client and passes an
API base URL ending in `/api`. Repository-owned endpoint metadata lets the SDK
and generated imperative CLI validate method and path selection against the
same generated contract. IaC-critical reads and error decoding remain
handwritten because the published success schemas are empty and generated
error models are too strict for a stable reconciliation contract.

## Consequences

- Generator upgrades require an intentional version change and regenerated
  snapshot.
- CI can prove generated code is current without contacting Dokploy.
- Unsupported regex constraints remain visible contract debt instead of being
  silently normalized away.
- The generated crate remains replaceable behind the owned SDK interface.
