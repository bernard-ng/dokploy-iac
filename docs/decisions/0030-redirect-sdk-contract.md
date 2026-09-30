# ADR 0030: Authoritative application Redirect identity discovery

## Status

Accepted. Extends Phase 8 resource breadth.

## Context

Dokploy `v0.30.6` exposes Redirects through `redirects.create`,
`redirects.one`, `redirects.update`, and `redirects.delete`. The generated
OpenAPI response types are empty, while the runtime returns a boolean from
create and complete records from reads. Create therefore supplies no physical
identity.

The `redirects` relation in `application.one` is the authoritative application
collection. A Redirect's regular expression is its collision key within that
application. Create, update, and delete are non-idempotent POST operations
whose transport outcome is unknown when no response is received.

## Decision

The handwritten SDK owns `RedirectId`, complete create and update inputs, safe
`RedirectDetails`, and a bounded `RedirectCollection`. A Redirect has exactly
one `ApplicationId` parent. Its regular expression, replacement, and permanent
flag are all mutable in place. The update interface does not expose the parent;
changing the application remains a replacement operation for later declarative
work.

Create reads the authoritative parent before mutation and rejects a preexisting
regular-expression collision. It attempts `redirects.create` once and requires
the boolean response to be true. It then reads the parent again, requires every
preflight identity to remain present, and accepts only one new identity whose
parent, regular expression, replacement, and permanent flag all match the
request. Missing, multiple, or conflicting candidates fail closed.

Direct reads require the returned identity and parent to be nonempty, then read
the parent collection and require the same complete record there. Parent
collections reject mismatched roots, children attached to another application,
empty or duplicate identities, invalid owned fields, and more than 10,000
records. Unknown fields and nested application data are ignored so application
environment values never enter the safe model.

Reads retain the shared bounded retry policy. Mutations are attempted once and
map a missing response to an outcome-unknown error. Collection absence is the
authoritative deletion proof.

The pinned live capture and SDK test use an undeployed disposable application.
They prove boolean create, preflight/postflight identity discovery, direct and
parent agreement, an all-field update, delete, parent absence, idle application
state, empty deployment history, and complete project cleanup.

## Consequences

- Callers never guess a Redirect identity from the boolean create response.
- Concurrent or contradictory collection changes prevent identity adoption.
- Direct reads cannot silently disagree with the parent used for planning.
- Application reparenting cannot pass through the in-place update seam.
- A transport interruption after mutation requires recovery; the SDK never
  retries the write automatically.
