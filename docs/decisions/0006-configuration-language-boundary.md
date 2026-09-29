# ADR 0006: Configuration language boundary

## Status

Accepted for the initial MVP.

## Context

Declarative infrastructure input is untrusted and field presence changes its
meaning. A missing value must not overwrite a setting managed in the Dokploy
dashboard, while an explicit `null` must remain distinguishable from an empty
string, empty collection, zero, or `false`. Configuration can also contain
secret descriptors and invalid values that must not leak through diagnostics.

The state subsystem already owns the global `kind.name` resource identity.
Configuration nests resources under environments, so flattening that input
must retain containment and prevent ambiguous identities.

## Decision

The `dokploy-config` crate exposes one parse-and-validate boundary. It accepts
at most one MiB of YAML through `serde-saphyr`, rejects duplicate keys, merge
keys, tags, anchors, aliases, and multiple documents, and enforces explicit
event, node, scalar, and depth budgets. Unknown fields are rejected at every
fixed-shape boundary.

`Field<T>` preserves three ownership states: an omitted key is unmanaged,
`null` clears the value, and any concrete value is managed. Nested application
environment maps preserve the same distinction for both the collection and
each variable.

The validated model uses the state subsystem's typed resource addresses and
retains deterministic child-to-parent edges. Because the initial address form
does not include environment scope, names of the same resource kind must be
globally unique across environments. Data references cannot cross environment
boundaries.

Secrets remain deferred environment-variable or workspace-relative file
descriptors. Parsing and validation never resolve them. Inline secret values,
unsafe paths, unsupported reference outputs, invalid lifecycle paths, and
conflicting moves or removals are rejected before any API call.

Parser errors expose only line and column. Semantic errors expose stable issue
codes and redaction-safe source locations without retaining source text.
Normalized configuration equality excludes location metadata. Generated JSON
Schema assists editors, while runtime parsing and semantic validation remain
authoritative.

## Consequences

- Planner code receives only normalized, semantically valid configuration.
- YAML formatting and map order do not change normalized equality.
- Configuration debug output and errors do not expose user-provided scalar
  values.
- A future environment-qualified address format requires an explicit state
  migration; the MVP fails on cross-environment name collisions instead of
  guessing.
- Filesystem secret resolution, dependency-cycle planning, and the CLI
  commands remain separate checkpoints.
