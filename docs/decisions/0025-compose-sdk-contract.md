# ADR 0025: Compose SDK opaque-document and identity contract

## Status

Accepted. Extends Phase 8 resource breadth.

## Context

Dokploy `v0.30.6` declares empty success objects for Compose operations, while
the runtime returns complete records. Those records contain the raw Compose
document, a refresh token, environment values, provider relations, and other
runtime data that must not enter reconciliation snapshots. Raw Compose content
can itself contain credentials.

Unlike LibSQL, `compose.create` returns a physical `composeId` directly.
`compose.search` provides environment-scoped pagination with `items` and
`total`, but its generated response remains untyped.

## Decision

The handwritten SDK owns strong `ComposeId` values, tolerant safe read models,
and raw create/update inputs. Compose documents live in zeroizing strings and
are always redacted from debug output. Safe detail and search models omit the
document, refresh token, environment content, provider relations, mounts,
domains, deployments, backups, and other unowned runtime data.

Create accepts only a raw source document, an explicit environment, and the
small owned metadata surface. It validates that the runtime response contains
a non-empty identity and agrees with the requested environment and name before
returning that identity. Search collects fixed-size pages up to a local bound,
requires a stable total, rejects incomplete pages, verifies every returned
parent, and rejects duplicate identities. Update owns only the human-readable
name, nullable description, and opaque Compose document. Delete requires an
explicit preserve-or-delete volume policy.

The pinned live capture creates a disposable project and raw Compose record,
updates its metadata and document, and deletes both. It verifies that the
record remains idle with no deployments throughout, publishes only sanitized
fixtures, and proves cleanup through detail, search, and project-topology
reads.

## Consequences

- Callers cannot accidentally retain or log raw Compose content or Dokploy's
  refresh token through safe reads.
- Create can use the direct runtime identity without a discovery race, while a
  conflicting returned parent or name fails closed.
- Environment discovery costs as many requests as bounded pagination requires
  and rejects contradictory remote evidence instead of returning a partial
  collection.
- Git-provider configuration and deployment controls remain outside this
  initial contract until their mutation and secret-handling behavior is proven
  independently.
