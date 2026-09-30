# ADR 0034: MongoDB declarative reconciliation

## Status

Accepted.

## Context

Dokploy `v0.30.6` exposes MongoDB through an environment-scoped collection, a
direct read, and typed create, update, password-change, and delete operations.
Creation requires a database user and password. Safe reads expose the user and
replica-set mode but never return a credential that the declarative engine may
persist.

The password-change operation rejects an idle, undeployed MongoDB record
because there is no running database container. That response does not prove
that post-create rotation is durable. External server selection is also not an
owned MongoDB property in the current declarative model.

## Decision

MongoDB is an environment-contained declarative resource. Fresh discovery
reads each bounded environment-scoped collection required by desired or stored
state and then reads every managed record directly. The two views must agree
on physical identity, logical name, and containment. A failed required
collection read, duplicate scoped name, duplicate physical identity,
contradictory read, or physical reparent blocks planning.

Username and replica-set mode are readable managed properties and may update
in place. Password is required on create and represented only by an
instance-bound fingerprint outside the mutation boundary. Changing or clearing
it after creation is unsupported.

Create, metadata update, and delete use the typed SDK operations and
participate in the bounded database executor. Every successful mutation is
journaled and checkpointed. Definitive API rejections close a journal step as
failed. Outcome-unknown transport, decoding, or unexpected-response failures
leave the step in progress for explicit recovery.

Recovery never retries an uncertain create. It adopts exactly one
environment-scoped name match only when the direct read agrees with the
collection and readable properties match the proposed checkpoint. An
interrupted username or replica-set update may use exact-except-sensitive
comparison only when the before and proposed password fingerprints are
identical.

Import accepts a direct identity or an interactive environment-scoped
selection. It writes canonical protected configuration, adopts the physical
identity, records username and replica-set mode, and leaves password unmanaged.
The first fresh plan must converge without reading or inventing a credential.

The live acceptance check uses the digest-pinned Dokploy `v0.30.6` image
without deploying MongoDB. It proves creation, no-op convergence, a
fail-closed password change, in-place username and replica-set updates,
deletion, authoritative absence, and secret exclusion.

External server selectors remain deferred. They will not enter configuration,
state, discovery, or mutation requests until their ownership and replacement
semantics have separate live evidence.

## Consequences

- MongoDB can be created, observed, updated, imported, recovered, and deleted
  through the declarative engine.
- Username and replica-set drift can be corrected in place.
- Password changes after creation remain blocked.
- Physical reparenting and external server selection remain unsupported.
- Outcome-unknown mutations require explicit recovery instead of appearing as
  definitive failures.
- Independent MongoDB mutations may overlap other database mutations while
  preserving per-resource journal and checkpoint guarantees.
