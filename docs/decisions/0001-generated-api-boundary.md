# ADR 0001: Generated API boundary

## Status

Accepted for the initial MVP.

## Context

Dokploy publishes an OpenAPI 3.1 document with broad request coverage, but all
604 success responses are declared as closed empty objects. The pinned document
also contains an invalid operation-level security reference.

Phase 0 compared `oas3-gen` 0.28.0 and OpenAPI Generator 7.25.0 against the same
vendored document. `oas3-gen` generated compilable Rust. OpenAPI Generator
emitted a missing model reference and failed compilation.

Neither candidate generated response models suitable for infrastructure state
or reconciliation.

## Decision

Use `oas3-gen` 0.28.0 to generate low-level request and endpoint bindings.

Keep the generated crate private behind `dokploy-sdk`. The handwritten SDK owns
authentication, transport policy, stable identifiers, critical response models,
and conversion from generated request types.

Runtime response models ignore unknown fields and are verified with sanitized
fixtures from a pinned local Dokploy instance. Generated files are never edited
manually.

## Consequences

- Broad imperative endpoint coverage remains automatable.
- Regeneration can change internal types without becoming a public API break.
- Authentication and critical reads require handwritten code.
- OpenAPI normalization and warning checks become part of generation.
- Upstream response-schema improvements can replace handwritten models
  incrementally without changing the SDK boundary.
