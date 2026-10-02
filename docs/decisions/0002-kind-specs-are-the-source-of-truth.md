# ADR 0002: Kind specs are the single source of truth

## Status

Proposed.

## Context

Adding one field to the first engine took about ten edits across six crates:
the property enum, the raw and validated config types, the parser, the writer
document and renderer, the SDK model and request bodies, the remote projection,
the desired-state compiler, the executor's create, update, and replace paths, the
importer, and the convergence check. Two small kinds (Redirect and Security)
cost about 1,900 lines of production code and 3,150 lines of tests. Roughly 265
project-side fields and about twenty settings kinds remain. Hand-writing them is
the reason progress is slow, and it lets forward and reverse mappings drift,
which is how fields end up applied but not imported.

## Decision

Each resource **kind** is described once, as data, in a **kind spec** under
`specs/`. The engine is kind-agnostic: it reads specs and does the rest. A spec
declares:

- identity: scope, containment parent, key, collision rule, address prefix;
- the Dokploy operations: create, read-one, list, update, remove, and optional
  deploy, each by OpenAPI `operationId`;
- fields: document name, API name (a JSON pointer into request and response
  bodies), type, nullability, mutability, secrecy, defaults, and the Dokploy
  versions it exists in (ADR 0004);
- write groups: which update endpoint carries which fields (ADR 0008);
- children: kinds that nest under it and how they attach.

From one spec the tool derives, with no per-kind code: the JSON Schema for
editors, the parser and writer, the desired-state compiler, remote projection,
create and update bodies, importer, convergence proof, reference documentation,
conformance tests, and simulator behavior (ADR 0015).

**The coverage ledger.** `cargo xtask specs --check` loads every spec and resolves
every operation and field against two inputs:

- `openapi/dokploy.json` for the **request** side: every request-body and query
  field of each referenced operation;
- recorded live fixtures under `fixtures/api/live/<version>/` for the **response**
  side. Dokploy's OpenAPI declares every response as an empty object (checked
  for all 30 create operations), so response shapes can only come from captures
  of a real instance (ADR 0007).

Each field in either input must be classified as exactly one of: `mapped` (a spec
field), `readonly` (status, ids, timestamps), `action` (not configuration),
`ignored` (with a written reason), or `derived`. CI fails on an unclassified field,
an unknown operation, or a spec field that no longer exists. This makes "every
field Dokploy exposes" checkable, and it replaces the hand maintained `✔/✘`
markers in `docs/vision/`. A response field can only be classified once a fixture
shows it, so a kind is not complete until its live capture exists (ADR 0015).

**Hooks.** Behavior a spec cannot express goes in a Rust hook registered by kind
name (for example tag membership, the multi-service Compose schedule read, and
disruptive web-server changes). A hook may override discovery, a create body, a
post-create step, or a diff, never the document model. A hook needs a written
justification in the spec (`hook: reason`) and its own tests. The target is that
fewer than a fifth of kinds need one.

**Format.** Specs are YAML (`docs/design/spec-format.md`). They are embedded in the
binary at build time and validated at startup in debug builds and in CI.

## Consequences

- A new field is one spec entry. A new flat kind is a spec file plus fixtures.
- The OpenAPI document is an input to CI, not just to code generation, so a new
  Dokploy release shows up as a failing ledger check with a precise list.
- Behavior is not discoverable by reading Rust alone. Mitigation: `dokploy
  coverage` prints the effective spec per kind, and the reference docs are
  generated from the same files.
- The spec grammar is a public design surface; it is settled by the M0 prototype
  and changed only by ADR.
