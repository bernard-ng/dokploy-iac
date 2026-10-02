# ADR 0009: State format 5 and scopes

## Status

Proposed. Keeps invariants 7, 13, 14, 15, and 18 and the storage, locking, and
journal design of the first engine's ADRs 0004, 0005, and 0012.

## Context

State format 4 keys resources by flat `kind.name` and knows two scopes. The new
address model (ADR 0006) and per-document lineages need a new key shape. The
locking, atomic replacement, journal, and fingerprint machinery is sound.

## Decision

**Format 5** differs from format 4 in three ways:

1. Resources are keyed by **full hierarchical address** (ADR 0006).
2. The file records its **document id**: `settings` or `project.<slug>`. A store
   refuses a file recorded under another document, as it already does across scopes.
3. Property values are keyed by validated spec paths (ADR 0004), including
   per-key `environment.KEY` entries.

Everything else is unchanged: lineage UUID and serial for stale-write protection,
instance binding, remote id per resource, containment and dependency lists,
protection flag, last-applied non-sensitive inputs, keyed fingerprints for
`write_only` and `file` values, atomic replacement with a backup, owner-only
permissions, the advisory writer lock, and the operation journal with recoverable
steps.

**Layout.** One directory per document under `.dokploy/`:
`.dokploy/settings/` and `.dokploy/projects/<slug>/`. Lock, journal, and backup are
per directory, so a crash while applying one project never blocks another.

**What state never holds.** Secret values, secret-derived text, file contents,
environment values that came from a secret source, and anything the response
projection dropped. For `env` blocks state holds, per key, either the last-applied
public value or a fingerprint.

**Migration.** None (pre-release policy, ADR 0001). The engine refuses a format 4
file with a message that says to re-import; `dokploy import` creates format 5 state
directly.

## Consequences

- `dokploy-state` changes its resource key type and adds the document id; the
  journal, lock, and storage modules change only where they name the key.
- Moving a service between environments is a `reparent` with a changed address
  prefix, not a delete and create.
- Saved plans (strict owner-only evidence envelopes) bind to the document id and
  serial as they bind to scope and serial today.
