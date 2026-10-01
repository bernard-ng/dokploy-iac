# ADR 0040: Compose declarative reconciliation

## Status

Accepted.

## Context

Dokploy `v0.30.6` exposes raw Compose resources through bounded
environment-scoped search, direct reads, and create, update, and delete
operations. A raw Compose document may contain credentials and is returned by
both read endpoints. It must not enter configuration literals, durable state,
plans, journals, diagnostics, or retained test evidence.

Compose also exposes deployment, provider, refresh-token, environment-value,
and external-server behavior. Those concerns have different lifecycle and
credential boundaries and are not part of this declarative resource.

## Decision

Compose is an environment-contained declarative resource. Its owned readable
property is nullable description. Its required opaque document comes from an
environment or bounded-file descriptor and is represented after compilation
only by an instance-bound fingerprint. Ordinary authored configuration cannot
create a Compose resource without that descriptor. Protected import may leave
the document unmanaged because no safe descriptor can be inferred from a
remote response.

Fresh discovery exhausts the bounded `compose.search` collection for each
relevant environment. A managed identity is also read through `compose.one`.
The direct and collection records must agree on identity, name, containment,
description, and raw source type. Duplicate identities, duplicate scoped
names, missing collection authority, contradictory endpoints, and physical
reparenting block planning.

Create always requests the raw source type and does not deploy the resource.
Description and document update in place through one journaled mutation.
Delete always sends the explicit preserve-volume policy. Provider selection,
deployment, refresh tokens, environment values, and external server selection
remain unmanaged and cannot be mutated through this adapter.

An uncertain create remains in progress and may be adopted only from one exact
authoritative scoped identity whose direct read agrees with the proposed
readable state. Interrupted document updates require manual review because the
document is write-only to the planner. A metadata-only update may recover from
an exact-except-sensitive observation only when the before and proposed
document fingerprints are identical. Definitive pre-mutation rejection may
fail the journal step; transport, decoding, and post-acceptance uncertainty do
not permit a blind retry.

Import reads one exact identity and writes canonical protected configuration
with description and an unmanaged document. The first fresh plan converges
without copying the remote document or claiming ownership of it.

The live acceptance uses the digest-pinned Dokploy `v0.30.6` image. It proves
undeployed create and no-op convergence, combined description and document
update, preserve-volume deletion, direct and scoped-collection absence, final
no-op convergence, and identity-scoped cleanup. Private raw responses are
validated and atomically reduced to allowlisted evidence before retention or
secret scanning. Authentication headers and raw responses live in a dedicated
private directory that cleanup always discards; retained evidence is also
checked against the actual integration API key.

## Consequences

- Raw Compose resources can be created, observed, updated, imported,
  recovered, and deleted through the declarative engine.
- The Compose document remains a one-shot input; only its fingerprint is
  durable.
- Deletion cannot destroy Compose-managed volumes through this adapter.
- Declarative reconciliation never deploys a Compose resource.
- External server selection and all provider-specific behavior remain
  deferred until their ownership and replacement semantics have separate live
  evidence.
