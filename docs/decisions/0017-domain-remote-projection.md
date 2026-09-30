# ADR 0017: Application-scoped Domain remote projection

## Status

Accepted for the Domain Phase 5 checkpoint.

## Context

Dokploy domains are configured inside an environment but attach to an
application by physical application ID. `domain.one` returns a full routing
record, while `domain.byApplicationId` returns the unpaginated collection for
one application. Dokploy `v0.30.6` accepts duplicate hosts, so a host lookup is
not safe unless the application scope and uniqueness are both proven.

## Decision

The SDK exposes only strong Domain reads and retains only the domain ID, host,
and application ID. Routing middleware, certificate settings, and nested
application runtime data do not cross the owned read seam.

Combined discovery resolves each configured logical application through its
stored physical identity and fresh Application observation. New domains use an
exact host lookup in that application collection. More than one matching host,
duplicate physical IDs, cross-application records, or disagreement between
direct and collection reads fail closed. Absence is conclusive only when the
caller asserts authoritative Domain collection visibility.

Managed domains bind to their stored physical IDs. Their direct read must
agree with the selected application and with an authoritative collection.
Remote properties contain the observed host and the canonical logical
application address, never the physical application ID.

## Consequences

- Project, environment, application, Postgres, Redis, and Domain observations
  share one fresh instance-bound snapshot.
- Domain host and application changes can be classified by the pure planner
  without retaining unrelated routing configuration.
- An unmanaged duplicate host is never silently adopted.
- Mutation and Traefik writes remain unreachable from discovery and planning.
