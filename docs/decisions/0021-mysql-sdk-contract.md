# ADR 0021: MySQL SDK mutation contract

## Status

Accepted. Begins Phase 8 resource breadth.

## Context

Dokploy's MySQL API differs from the existing Postgres boundary in two
security-sensitive ways. Creation requires a separate root password, and
password rotation is an explicit operation whose body selects either the
database user or root account. Detail responses also contain both plaintext
passwords and environment data that callers do not need for reconciliation.

The generated client accurately pins routes but exposes secret-bearing
response shapes and does not provide the bounded, environment-scoped lookup
required by the infrastructure adapter.

## Decision

The public SDK exposes an owned MySQL boundary with a strong `MySqlId`, safe
detail and search models, and bounded environment-scoped collection. Safe
models omit both passwords, raw environment data, and other unowned fields.
Unknown response fields remain tolerated so additive upstream changes do not
break reads.

Creation requires user and root passwords as zeroizing values. Non-secret
metadata updates and password changes are separate operations. Password
changes use an explicit user-or-root variant and serialize the pinned
`type` discriminator. Secret-bearing request models redact their debug output.

The adapter uses pinned generated route constants while retaining owned,
narrow request and response types. Transport failures after a mutation is
sent remain outcome-unknown; structured server rejections remain available to
callers. Search follows authoritative pagination metadata and rejects unstable,
incomplete, or oversized results.

## Consequences

- Callers cannot accidentally obtain MySQL passwords through the safe read
  API or confuse MySQL identifiers with another resource type.
- User and root password rotation are reviewable, explicit operations rather
  than optional fields on a general update.
- The adapter can tolerate additive Dokploy response fields while rejecting
  ambiguous pagination and mutation outcomes.
- Live contract capture does not deploy a database. Dokploy `v0.30.6` therefore
  returns a structured rejection for password changes; mock transport tests
  cover their successful wire contract.
