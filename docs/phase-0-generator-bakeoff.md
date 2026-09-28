# Phase 0: external contract and generator bake-off

## Goal

Prove the current Dokploy API contract before selecting the generated transport
layer or implementing the reconciliation engine.

## Fixed input

Both candidates must use [`openapi/dokploy.json`](../openapi/dokploy.json),
whose immutable upstream source and checksum are recorded in
[`openapi/SOURCE.md`](../openapi/SOURCE.md).

## Acceptance checks

- Generation completes from the vendored OpenAPI 3.1 document.
- Generated Rust compiles without hand-editing generated files.
- `x-api-key` authentication is represented correctly.
- Query parameters and request bodies are represented correctly.
- Nullable unions and `anyOf` constructs are usable.
- Operation names are stable and discoverable.
- The following operations are inspected directly:
  - `application.create`
  - `application.one`
  - `project.all`
  - `project.one`
  - `postgres.one`
- Incomplete response schemas are recorded rather than silently trusted.

## Candidate results

### `oas3-gen` 0.28.0

Result: generation and compilation pass.

- Generated all 604 operations and 2,004 types in about 49,800 lines.
- The untouched generated workspace passes `cargo check` on Rust 1.97.1.
- The five-operation acceptance subset also compiles.
- Operation names, required query inputs, request bodies, and enums are usable.
- Authentication is not generated. The owned SDK adapter must inject the
  `x-api-key` header.
- Success responses remain dynamic because the source document provides no
  response fields.
- Nullable request fields collapse omitted and explicit null into one Rust
  representation.
- Twenty-five unsupported regular expressions are discarded with warnings.

Pinned reproduction:

```bash
cargo install oas3-gen --version 0.28.0 --locked

oas3-gen generate client-mod \
  -i openapi/dokploy.json \
  -o generated \
  --workspace \
  --module-version 0.0.0

cargo check --manifest-path generated/Cargo.toml
```

The `--only` option expects normalized underscore names. Automation must reject
an empty generated operation set because raw hyphenated operation IDs produce
no operations while the command still exits successfully.

### OpenAPI Generator 7.25.0

Result: generation passes, but the untouched generated Rust crate does not
compile.

- Generated all 604 operations, 410 model files, and about 56,300 Rust lines.
- Three compile errors reference a missing `AnyOfLessThanGreaterThan` model.
- The missing type comes from backup metadata expressed as
  `anyOf: [{}, {"type": "null"}]`.
- Mapping flags do not repair the generated output.
- A manual type alias proves this is the immediate compile blocker, but editing
  generated output violates the project boundary.
- Required identifiers and several required request fields become optional.
- Authentication is generated correctly as `x-api-key`.
- Success results remain `serde_json::Value` or maps of dynamic values.

Pinned reproduction:

```bash
docker run --rm \
  -v "$PWD:/local" \
  openapitools/openapi-generator-cli:v7.25.0 \
  generate \
  -g rust \
  -i /local/openapi/dokploy.json \
  -o /local/generated \
  --additional-properties=packageName=dokploy_client,packageVersion=0.0.0,useSingleRequestParameter=true

cargo check --all-targets --manifest-path generated/Cargo.toml
```

### Source drift

The pinned SDK artifact and the OpenAPI document served by the Dokploy docs
site both contain 604 operations, but they are not byte-identical. The pinned
SDK artifact references an undefined operation security scheme named
`Authorization`; the document served by the docs site references its defined
API-key scheme. The vendored SDK artifact remains the reproducible source
because its generation provenance points to Dokploy `v0.30.6`.

## Decision

Select `oas3-gen` 0.28.0 for generated request and path bindings.

The generated client is not the public SDK. It sits behind an owned transport
adapter that:

- installs `x-api-key` on every authenticated request;
- owns base URL normalization, timeouts, and error decoding;
- rejects generation that emits no operations;
- exposes handwritten response models for IaC-critical reads;
- treats unsupported regex warnings as explicit contract debt;
- preserves a separate field-ownership type where omitted and null differ.

OpenAPI Generator remains a fallback only if its invalid `anyOf` output is
fixed upstream or reproducibly normalized before generation.

## Static contract findings

- All 604 operations declare HTTP 200 as a closed empty object.
- `components.schemas` contains only the generic 400, 401, 403, 404, and 500
  error shapes.
- The pinned document defines the global `apiKey` scheme using the `x-api-key`
  header, while every operation overrides it with undefined `Authorization`.
- The official Dokploy handlers return rich application, project, and database
  records despite the empty schemas.
- Generated request bindings are useful; generated success models are not a
  safe state or reconciliation contract.

## Runtime contract gate

Static generation cannot prove the real response shapes or authenticate against
a deployment. The local environment in
[`docs/integration-testing.md`](integration-testing.md) provides a disposable
Dokploy `v0.30.6` instance for sanitized live fixtures and authenticated request
tests.

An external Dokploy instance can be tested with:

```text
DOKPLOY_URL
DOKPLOY_API_KEY
```

The local scripts keep generated credentials only under ignored `.integration/`
storage. No credential is written to a tracked fixture.

## Runtime verification

The contract gate passed against the digest-pinned Dokploy `v0.30.6` local
environment.

- An administrator was created through Dokploy's self-hosted signup flow.
- An API key was created with organization metadata and rate limiting disabled
  for the bounded fixture run.
- Authenticated `x-api-key` requests succeeded.
- `project.all` returned an array, contradicting its declared object schema.
- `application.create`, `application.one`, `project.create`, `project.one`,
  `project.all`, `postgres.create`, and `postgres.one` returned rich runtime
  records.
- Raw responses remained in ignored local storage.
- Stable identifiers and timestamps were normalized.
- Password, token, secret, environment, and build-secret fields were redacted
  before fixtures were written to the repository.

The sanitized evidence is stored under
[`fixtures/api/live/v0.30.6`](../fixtures/api/live/v0.30.6).
