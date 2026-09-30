# ADR 0029: Typed application Port contract

## Status

Accepted. Extends Phase 8 resource breadth.

## Context

Dokploy `v0.30.6` exposes application Ports through `port.create`, `port.one`,
`port.update`, and `port.delete`. Its generated OpenAPI request types represent
the two port numbers as floating-point values even though the runtime accepts
and returns JSON integers. The declared success responses are empty while the
runtime returns complete Port records.

An application's `ports` relation in `application.one` is the authoritative
collection. A direct `port.one` lookup returns HTTP 400, rather than 404, after
successful deletion. Create, update, and delete are non-idempotent POST
operations whose outcome is unknown when no response is received.

## Decision

The handwritten SDK owns `PortId`, `PublishMode`, and `PortProtocol`. Create and
update accept `NonZeroU16` values for both published and target ports, and
serialize them directly as JSON integers. The generated floating-point request
models remain private and are not used by this adapter.

A Port has exactly one `ApplicationId` parent. Create validates the returned
physical identity, parent, port numbers, publish mode, and protocol against the
request. Update sends every mutable field because Dokploy's update operation is
a complete replacement contract; it cannot reparent a Port.

Safe response models retain only the Port identity, application identity, port
numbers, publish mode, and protocol. Unknown fields and nested application data
are ignored. Direct reads require the returned physical identity and a nonempty
application identity. Parent reads parse the `ports` relation from
`application.one`, reject a mismatched application, duplicate or empty Port
identities, children attached to another application, and collections larger
than 10,000 records.

Reads retain the shared bounded retry policy. Every mutation is attempted once
and maps a missing response to an outcome-unknown error. Deletion is proven by
authoritative absence from the parent collection; callers do not depend on the
runtime's inconsistent direct-read status.

The pinned live capture and SDK test use an undeployed disposable application.
They prove create, direct and parent read, an all-field update, delete, parent
absence, idle application state, empty deployment history, and complete project
cleanup.

## Consequences

- Zero, negative, fractional, and out-of-range port numbers cannot cross the
  typed SDK seam.
- Callers cannot attach a Port to a non-application target or change its parent
  through update.
- `application.one` supplies complete deletion and collision evidence even when
  `port.one` reports a nonstandard status.
- The adapter fails closed on contradictory parent data instead of presenting
  a partial collection.
- A transport interruption after mutation requires recovery; the SDK never
  retries the write automatically.
