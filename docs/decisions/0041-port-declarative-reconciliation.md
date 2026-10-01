# ADR 0041: Port declarative reconciliation

## Status

Accepted.

## Context

Dokploy `v0.30.6` exposes application Ports through `port.create`,
`port.one`, `port.update`, and `port.delete`, and lists them through the
`ports` collection of `application.one`. The create and update responses are
small, contain no secrets, and carry no deployment behavior. Dokploy reports a
missing Port as HTTP 400 from `port.one`, which is not a safe proof of absence
without a second authoritative source.

A Port is meaningful only inside one application. Its collision key is the
published port and protocol, not its logical name, because two Ports that
publish the same port and protocol on one application cannot both be
unambiguously matched or reconciled.

## Decision

Port is an application-contained declarative resource at
`environments.<env>.applications.<app>.ports.<name>`. It owns exactly four
fields: published port, target port, publish mode, and protocol. Ports are
readable and non-sensitive, so they have no descriptor, fingerprint, or
write-only property.

Configuration rejects, at parse time, two Ports within one application that
share a published port and protocol. The planner and state snapshot seams
independently reject null, out-of-range, and unknown values for every Port
field, so malformed desired, stored, or remote observations fail closed before a
plan exists.

Fresh discovery reads the `application.one` Port collection for every
application that contains a desired, stored, or removed Port. A managed
identity is also read through `port.one`. The direct and collection records must
agree exactly on identity, containment, and every owned field. Duplicate
identities, duplicate collision keys, a direct identity under a different
application, and a collection that names a different application block
planning. Unmanaged Ports are matched by collision key. A 400 from `port.one`
proves absence only when the parent collection is authoritative and does not
contain the identity. A partial authority downgrades absence to an unavailable
observation.

Create sends the four owned fields and the containing application and never
deploys. Update first performs a fresh `port.one` read, verifies identity and
containment, and then sends one complete replacement of all four fields. Delete
removes the exact identity and treats an already-missing identity as
convergent. Changing the containing application cannot be done in place:
the executor deletes the old identity, checkpoints the removal, journals a
second create step with a recovery placeholder, and records the new identity
only after the create response proves it.

An uncertain create stays in progress and is adopted only from one exact
collision-key match in the authoritative parent collection. An uncertain update
is confirmed only when authoritative direct and collection reads agree with the
proposed complete state. Transport, decoding, and post-acceptance uncertainty
never permit a blind retry; only a definitive pre-mutation rejection may fail
the journal step.

Protected import reads one exact identity, its authoritative parent collection,
and the full containment chain, then writes canonical protected configuration
and state. The first fresh plan converges without further mutation.

The live acceptance uses the digest-pinned Dokploy `v0.30.6` image. It proves
undeployed create and no-op convergence, complete in-place update with a stable
identity, containment replacement with a new identity and an empty former
parent, deletion, direct and collection absence, protected import of an
out-of-band Port, zero deployments on every touched application, and
identity-scoped cleanup. Raw responses live in a private directory that cleanup
always discards; retained evidence is reduced to an allowlist and scanned for
the integration API key.

## Consequences

- Application Ports can be created, observed, updated, replaced, imported,
  recovered, and deleted through the declarative engine.
- Two Ports cannot silently collide on one application, in configuration or on
  the server.
- Declarative reconciliation never deploys an application when it changes a
  Port.
- A role that cannot see a complete Port collection cannot plan deletions or
  creations, because absence is never inferred from partial data.
