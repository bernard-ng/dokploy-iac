# ADR 0004: Property model, field types, and mutability

## Status

Proposed. Keeps the conclusions of the first engine's ADR 0006 (field presence
semantics) and ADR 0008 (pure property planner seam).

## Context

Dokploy objects mix scalars, enumerations, lists, maps, tagged unions
(application sources, mount types, notification providers), free-form JSON blobs
(Swarm settings), key/value text blocks (environment variables), file contents,
references to other resources, and secrets. Fields differ in whether they can
change in place, whether Dokploy lets you read them back, and which endpoint
writes them.

## Decision

### Presence

Unchanged from the first engine, and still the most important rule:

| Written | Meaning |
|---------|---------|
| omitted | **unmanaged**: never read for comparison, never written |
| `null` | **cleared**: the remote value is made empty (only for nullable fields) |
| a value | **managed**: the remote value is made to match |

### Field types

| Type | Notes |
|------|-------|
| `text`, `int`, `number`, `bool` | with optional `min`, `max`, `pattern`, `min_len` |
| `enum[a, b]` | closed set; unknown remote values read as *unknown*, not coerced |
| `list<T>` | ordered; compared in order |
| `set<T>` | unordered; compared sorted (tags, middlewares, watch paths) |
| `map<text, T>` | planned **per key** when `granularity: key` |
| `struct{...}` | named fields; planned as one value unless `granularity: field` |
| `union(tag){a: struct, b: struct}` | tagged; the tag is a field; each arm lists its fields |
| `blob(schema)` | opaque JSON validated against a JSON Schema; one **atomic property** |
| `env` | environment block: `map<text, envvalue>` rendered to Dokploy's `KEY=VALUE` text |
| `file` | content from a workspace file; compared by content digest (ADR 0010) |
| `ref(kind)` | address of another resource in the same document; creates a dependency edge |
| `selector(kind)` | name of a resource outside the document, resolved against fresh remote state |
| `secret(T)` | write-only; configuration holds a source, state holds a fingerprint |

### Mutability

Every field has exactly one class:

| Class | Planning behaviour |
|-------|--------------------|
| `in_place` | a change plans an update |
| `create_only` | a change plans a **replacement** (delete then create, or create then delete per kind); refused when the resource is protected |
| `reparent` | a change moves the resource under another parent through a move operation |
| `write_only` | never read back; compared by keyed fingerprint; an update that needs to resend it requires the source to be declared |
| `computed` | readable, never configurable (status, timestamps); used for import hints and verification only |
| `action` | an operation, not state (start, stop, rebuild); never modeled as a field |

### Paths

A property path is a dotted string validated against the kind's spec:
`name`, `swarm.placement`, `environment.LOG_LEVEL`. A map key is a path segment.
`PropertyPath` in `dokploy-core` becomes a validated newtype over this string; the
spec registry answers "is this path legal for this kind" and "what is its
granularity". Planning granularity:

- scalars, enums, lists, sets, files, refs, selectors, blobs: one property each;
- `env` and `map` with `granularity: key`: one property per key, so a plan names
  the variables that change and never their values;
- `struct` with `granularity: field`: one property per member.

### Comparison

Values are normalised before comparison: JSON numbers canonicalised, text trimmed
only where the spec says (`trim: true`), sets sorted, unions reduced to their
active arm, `env` blocks parsed to key/value pairs (order, blank lines, and
comments in the remote text do not count as changes unless the spec says
`preserve_comments`). Normalisation lives in `dokploy-model` so import, plan, and
verification use one definition.

## Consequences

- The planner stays pure and deterministic; only the path type changes.
- Plans for large blobs show "property changed" with the path, never the value.
- Union arms and `env` handling are the two places most likely to need hooks;
  both have first-class types so most kinds need none.
