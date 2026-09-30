# ADR 0031: Secret-safe application Security identity discovery

## Status

Accepted. Extends Phase 8 resource breadth.

## Context

Dokploy `v0.30.6` exposes application basic-auth entries through
`security.create`, `security.one`, `security.update`, and `security.delete`.
Create returns only a boolean, so it does not supply the new physical identity.
Direct and parent reads return the plaintext password. Update requires a
complete username and password, and the API has no password-clear operation.

The `security` relation in `application.one` is the authoritative application
collection. A username is the collision key within one application. Create,
update, and delete are non-idempotent POST operations whose transport outcome
is unknown when no response is received.

## Decision

The handwritten SDK owns `SecurityId`, complete create and update inputs, safe
`SecurityDetails`, and a bounded `SecurityCollection`. Create and update
password inputs are owned by `Zeroizing<String>`. Their debug representations
are redacted. Response decoders consume the plaintext password, explicitly
zeroize an owned response string, and retain only whether a non-empty password
was present. No response model or public accessor retains password bytes.

Create reads the authoritative parent before mutation and rejects a username
collision. It attempts `security.create` once and requires the boolean response
to be true. It then reads the parent again, requires every preflight identity
to remain present, and accepts only one new identity whose parent, username,
and password-presence evidence match the request. Missing, multiple, or
conflicting candidates fail closed.

Direct reads require the returned identity, parent, and username to be
nonempty. They then require the same safe record in the parent collection.
Parent collections reject mismatched roots, children attached to another
application, duplicate or empty identities, duplicate usernames, invalid owned
fields, and more than 10,000 records. Unknown fields and nested application
data are ignored.

Update always sends the complete username and password. It cannot clear the
password, omit a credential field, or change the application parent. Reads
retain the shared bounded retry policy. Mutations are attempted once and map a
missing response to an outcome-unknown error. Collection absence is the
authoritative deletion proof.

The pinned live capture privately verifies the exact created and updated
passwords, then publishes only sanitized fixtures. The capture and SDK test use
an undeployed disposable application and prove create identity discovery,
direct and parent agreement, complete credential update, delete, parent
absence, idle application state, empty deployment history, and project
cleanup.

## Consequences

- Plaintext response passwords do not enter safe models, debug output, errors,
  or tracked fixtures.
- Callers cannot guess an identity from the boolean create response.
- Duplicate usernames or contradictory collection changes prevent identity
  adoption.
- Password changes require a replacement value; password clearing is not
  represented.
- A transport interruption after mutation requires recovery; the SDK never
  retries the write automatically.
