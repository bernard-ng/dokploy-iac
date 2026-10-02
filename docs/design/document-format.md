# Document format

How a version 2 document is read, checked, and written
([ADR 0005](../decisions/0005-document-model-and-workspace.md),
[ADR 0010](../decisions/0010-secrets-environment-and-content.md)). Implemented by
`dokploy-model`, which knows no kind: what a document may contain is decided by the kind
specs in `specs/`.

```yaml
version: 2
settings:                    # or: project:
  registries:                # a section of the settings document: key -> resource
    ghcr:
      type: cloud
      url: ghcr.io
      username: ci
      password: { env: GHCR_TOKEN }        # a secret is a source, never a literal
      server: { name: edge-1 }             # a selector: a resource outside the document
```

A project document is the project resource itself, keyed by `slug`, with its children nested
inside their parent (`environments:` → `applications:` → `redirects:`).

## API

| Call | What it does |
|------|--------------|
| `Document::parse(text, &registry)` | Parse and validate; every problem is reported with its position |
| `Document::render()` | The canonical text |
| `json_schema(&registry, scope)` | The JSON Schema of one document root, for editors |

## Reading rules

- `version: 2` and exactly one of `project` or `settings`. A `version: 1` document gets a
  message to re-import.
- **Unknown fields are rejected everywhere**, with the valid names listed. YAML anchors,
  aliases, merge keys, tags, duplicate keys, multiple documents, non-finite floats, and
  documents over 2 MiB or 32 levels deep are rejected.
- Resource keys match `[a-z][a-z0-9_-]*`. Text must be quoted if it looks like a number: `123` is
  an integer, not text.
- An omitted field is unmanaged; `null` clears it, and only for a `nullable` field (or a keyed
  `env`/`map` as a whole). Types, `enum` values, `min`/`max`/`min_len`/`pattern`, set uniqueness,
  and selectors (`{ name }`, or `{ local: true }` for a server) follow the spec.
- A `secret` field is written as a **source**: `{ env: NAME }`, `{ file: path }`, or
  `{ vault: { provider, secret } }`. A `content` field is `{ file: path }`. A literal is rejected
  and its text is never echoed in a diagnostic. File paths are relative to the workspace, without
  `..`, a leading `/`, or empty segments.
- `environment` values are `text`, `{ value: text }`, `{ secret: <source> }`, or `{ vault: ... }`.
  Names are letters, digits, and underscores.
- Every resource may also carry `lifecycle: { protect, ignore_changes }` (each path must be a
  legal property of the kind) and `depends_on: [address]`.

Not read yet: `moves`/`removed`, `lifecycle.deploy`, `env_file`, shared types, and checking that a
`file` exists or that a `depends_on` address resolves. Those belong to the workspace and the
engine.

## Diagnostics

`Diagnostics` lists every problem in source order. Each has a stable code, a dotted path, and a
line and column.

| Code | Meaning |
|------|---------|
| `DOKDOC001` | Syntax, budget, or size |
| `DOKDOC002` | `version` missing or not 2 |
| `DOKDOC003` | The root is not a supported shape |
| `DOKDOC004` | Unknown field, section, or member |
| `DOKDOC005` | A value of the wrong type |
| `DOKDOC006` | A value that breaks a rule (range, length, enum, pattern) |
| `DOKDOC007` | A resource or collection key that is not allowed |
| `DOKDOC008` | A secret or content field written as a literal |
| `DOKDOC009` | A malformed source |
| `DOKDOC010` | `null` where the field cannot be cleared |
| `DOKDOC011` | A duplicate in a set |
| `DOKDOC012` | A malformed `lifecycle` or `depends_on` |

## Canonical text

`render()` is deterministic: `version`, then the root; within a resource the fields in alphabetical
order, then `depends_on`, `lifecycle`, and the child sections; resources by key; sets sorted.
Strings are plain only when that reads back as the same string, otherwise double-quoted.
Parsing the output gives back an equal document (spans are positions, not content), and
rendering that gives back the same text. This is held by randomized tests over every field type.

## JSON Schema

`json_schema` accepts what `parse` accepts, as far as JSON can say it: shapes, enums, ranges,
patterns, set uniqueness, unknown-field rejection, unions by tag, sources, selectors, and keys.
Rules that need the whole document or the filesystem stay with the parser.
