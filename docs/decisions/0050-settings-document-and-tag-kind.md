# ADR 0050: Settings document, state scope, and the tag kind

## Status

Accepted. Implements slice S0 of the hierarchical import and instance settings
design. ADR 0049 is reserved for name allocation (slice A5).

## Context

Every document so far describes one project and everything below it. Some
Dokploy objects belong to the instance, not to a project, and a project can only
refer to them. Tags are the first such object: a tag is created once and then
attached to any number of projects. The planner, state, and executor were built
around a single project lineage, so settings need a foundation before any kind
can use it.

## Decision

### Two documents, one model

A file declares exactly one of `project:` or `settings:`. Both present is
`DOKCFG070`. The parser peeks at the top-level keys to choose the grammar and
produces the same `DokployConfig` type with a `scope()`, so `compile_desired`,
the planner, and the executor are unchanged. Resources keep flat `kind.name`
addresses. Tags live under `settings.server.tags`; the nesting is presentation
only.

### State scope

State format 4 adds `scope: project | settings`; format 3 decodes as project.
`StateStore::new` stays project scope. `StateStore::with_scope` binds settings
scope, whose files live in `.dokploy/settings/` with their own lock and journal,
so a crash in one scope never blocks the other and both documents can share a
directory. A state file rejects resources whose kind does not belong to its
scope, and a store refuses a file recorded under the other scope. Every command
that reads state (`plan`, `apply`, `recover`, `destroy`, `state`, saved plans)
chooses the scope from the document, and re-checks it after loading so a file
swapped between the peek and the load is refused.

### The tag kind

`tag` has properties `name` (updatable in place, defaulting to the logical name)
and `color` (set-only: Dokploy offers no way to clear it, so a clear is
unsupported rather than guessed). A tag is a settings-scope root with no
containment parent. Discovery reads `tag.all` once and treats that collection as
the single authority: a tracked tag is found by identity, an untracked one by
exact name, and a name shared by several tags is ambiguous and blocks the plan.

### Project tags

`project.tags` is a list of tag names, compared as a sorted set. Names, not
identities, are the desired value, which keeps documents portable across
instances. Discovery resolves the project's tag identities to names through
`tag.all`; a desired name that resolves to nothing makes the property unknown and
blocks the plan. The executor resolves names to identities again from a fresh
`tag.all` immediately before mutating, computes the difference, and issues
`tag.assignToProject` and `tag.removeFromProject`. Omitting `tags` leaves the
association unmanaged; `tags: null` is rejected (`DOKCFG073`).

### Import

`dokploy import settings` reads `tag.all`, builds a settings document and
settings-scope state (tags protected), proves offline that the first plan is
empty, re-reads the collection to detect concurrent change, and only then writes.
It fails closed on a name shared by several tags or an invalid identity.
`dokploy import project` writes `project.tags` for a project that has tags, and
fails closed when a tag identity is unlisted or its name is shared.

## Consequences

- Diagnostics `DOKCFG070`-`DOKCFG074` are added; `DOKCFG075`-`DOKCFG079` stay
  free for the next settings kinds.
- The shared property `ScheduleName` is renamed `Name` and now serves Schedules
  and Tags.
- An older CLI cannot read format 4 state.
- The SDK tag adapter validates every response, but the response shapes are
  pinned by tests, not by a live capture, because the generated contract types
  them as untyped JSON. `scripts/integration/capture-tag-contract.sh` records the
  live shapes. A response the adapter does not recognise is an unexpected
  response on reads and an unknown outcome on mutations, never a guess.
- Tag error bodies are sanitized like every other endpoint with free-form
  remote text.
- The pre-existing sensitive-fingerprint tests need `DOKPLOY_FINGERPRINT_KEY`
  or an OS keyring; neither is specific to this change.
