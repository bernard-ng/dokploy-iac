# ADR 0027: Typed Mount target and opaque file contract

## Status

Accepted. Extends Phase 8 resource breadth.

## Context

Dokploy `v0.30.6` declares empty success objects for Mount operations, while
the runtime returns full records. A Mount can target any of eight service
kinds. The target is represented redundantly by `serviceType` and one of eight
nullable identity fields. File Mount responses can also contain plaintext
file content.

`mounts.listByServiceId` returns an unpaginated array. Create, update, and
remove are non-idempotent POST operations whose transport outcome is unknown
when no response is received.

## Decision

The handwritten SDK owns `MountId`, `MountType`, and a closed `ServiceTarget`
union containing strong identifiers for application, Compose, LibSQL,
MariaDB, MongoDB, MySQL, PostgreSQL, and Redis. Safe response models require
exactly one non-empty target identity and require it to agree with
`serviceType`. Nested target records and file content never enter the model.

Create uses type-specific constructors for bind, volume, and file Mounts. File
content is held in zeroizing memory and redacted from debug output. Create
accepts the returned identity only when the target, Mount type, target path,
and type-specific source agree with the request. Update exposes only the Mount
path and type-specific source; it does not expose implicit reparenting.

Target listing is bounded to 10,000 records and rejects duplicate identities
or any record attached to another target. Reads retain the shared bounded
retry policy. Every mutation is attempted once and maps a missing response to
an outcome-unknown error.

The pinned live capture uses an undeployed volume Mount because it requires no
secret content. It proves create, detail, target list, update, remove, and
complete disposable-project cleanup. Mock transport tests prove exact file
content requests and verify that content is absent from debug output and
errors. Fixture sanitization independently rejects any unredacted non-empty
`content` field.

## Consequences

- Callers cannot combine an arbitrary service type string with the wrong
  physical identity.
- File bytes cross only the mutation serialization seam and cannot enter safe
  reads, debug output, or tracked fixtures.
- The unpaginated target list fails closed above its local safety bound rather
  than presenting partial or unreviewable evidence.
- Reparenting and deployment-driven filesystem behavior remain outside this
  initial adapter until their mutation and recovery contracts are proven.
