# ADR 0011: Import and the round-trip contract

## Status

Proposed. Keeps the first engine's offline convergence proof and its refusal to
overwrite an existing workspace; replaces its per-kind builders.

## Context

The first engine's importer wrote what its builders knew about, so fields that
could be applied were not always imported, and nothing proved the reverse path.

## Decision

### One mapping, two directions

Import and apply use the same field table in the same spec. The importer walks
the remote tree through the projection (ADR 0007), maps each property to a document
field by the spec, applies the secrets policy (ADR 0010), allocates keys (ADR 0006),
and renders canonical YAML with the document writer. There is no separate importer
per kind.

### Pipeline

1. **Inventory (I/O).** Read the settings or project tree with the minimum reads.
2. **Validate (pure).** Duplicate identities, invalid names, parentage, natural-key
   collisions the planner could not plan, and size bounds.
3. **Build (pure).** Assemble the document and its state together.
4. **Render, re-parse, compile.** Render canonical YAML, parse it back, compile the
   desired state.
5. **Prove (pure).** Compare the compiled desired state with the new state
   property by property, in both directions, plus containment, dependencies, and
   protection. If the first plan would not be empty, fail before writing.
6. **Persist.** Write documents, content files, secret files, and state; roll the
   documents back if state fails.
7. **Re-read (I/O).** Compare a topology fingerprint with step 1; a change during
   the crawl writes nothing.

### Commands

`import settings`, `import project <id>`, and `import all` (settings plus every
project, plus the manifest). Import creates a new workspace and refuses when
documents or state already exist. `--dry-run` runs everything but persistence.
Adopting one more resource into an existing workspace is the `adopt` directive
(ADR 0006), not an import mode.

### The unsupported ledger

Everything the importer could not write is listed, grouped as: *unmanaged by
choice* (secrets skipped, `--env=skip`), *unsupported kind or field* (outside the
beta list), and *unrepresentable* (a source type like `drop`, an unknown enum
value). Silently dropping a manageable field is a defect (invariant 24).

### The round-trip contract

For a supported Dokploy version V and an instance I containing only supported kinds:

1. `D = import(I)`;
2. applying `D` (then deploying) to a fresh instance of V, after the documented
   manual steps, yields `I'`;
3. `import(I')` is equal to `D` after normalisation, which removes only: remote ids,
   timestamps, status fields, generated `app_name` suffixes that `D` did not pin, and
   values Dokploy generates for secrets that `D` did not declare.

The test is automated against the simulator on every change and against live
Dokploy for each supported version in CI (ADR 0015).

## Consequences

- Fixing a mapping fixes apply and import together.
- Import is the strongest test of the spec: a wrong mapping shows up as a
  convergence failure with a named property.
- Import needs plaintext secrets from the API and handles them as in ADR 0010.
