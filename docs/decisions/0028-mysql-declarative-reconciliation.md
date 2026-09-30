# ADR 0028: MySQL declarative reconciliation

## Status

Accepted.

## Context

Dokploy `v0.30.6` exposes MySQL through an environment-scoped collection, a
direct read, and typed create, metadata update, password-change, and delete
operations. Creation requires both a database-user password and a root
password. Reads do not safely reveal either value, and durable IaC artifacts
must never contain their plaintext.

The password-change endpoints reject an idle, undeployed MySQL record because
there is no running database container. Deploying a database solely to test a
credential mutation would cross the current integration boundary and would not
prove that the new value persists safely across all relevant Dokploy state.

Interrupted mutation transport is another safety boundary. An absent response
does not prove that a create, update, or delete failed remotely. Recording such
an outcome as a definitive failed journal step would discard the evidence that
recovery needs.

## Decision

MySQL is an environment-contained declarative resource. Fresh discovery reads
the bounded environment-scoped collection and then the direct record. The two
views must agree on physical identity and containment. Database and username
are readable managed properties. User-password and root-password observations
remain explicitly unknown and sensitive.

Creation requires two distinct one-shot secret bindings. Configuration stores
only their descriptors, while desired snapshots, plans, state, checkpoints,
and journals store separate instance-bound fingerprints. Required create input
is modeled separately from post-create mutation support: database and username
may update in place, but changing or clearing either credential is unsupported.

Create, metadata update, and delete use the typed SDK operations. Independent
MySQL changes participate in the bounded database executor, while containment
and dependency ordering remain serial where required. Every successful remote
step is checkpointed before dependent work continues.

Definitive API rejections close a journal step as failed. Outcome-unknown
transport, decode, or unexpected-response errors leave the started step in
progress so explicit recovery can inspect fresh remote evidence. Recovery does
not retry an uncertain create. It adopts only one environment-scoped name
match whose direct readable properties agree with the expected checkpoint;
both sensitive properties use exact-except-sensitive comparison.

Import accepts a direct MySQL identity or an interactive environment-scoped
selection. It writes canonical protected configuration, adopts the physical
identity into state, and leaves both secrets unmanaged. The first fresh plan
must converge without reading or inventing credential values.

The live acceptance check runs against the digest-pinned Dokploy `v0.30.6`
image without deployment. It proves create and no-op convergence, rejects a
changed password source during read-only planning, restores the source and
reconverges, updates database and username in place, deletes the record, proves
authoritative absence, and scans all workspace artifacts for three secret
canaries.

## Consequences

- MySQL can be created, observed, updated, imported, recovered, and deleted
  through the same declarative safety model as the earlier database adapters.
- Database and username drift can be corrected in place.
- User-password and root-password changes after creation remain deliberately
  blocked, including attempts to clear either value.
- Physical reparenting remains unsupported.
- The live proof does not deploy MySQL and therefore does not claim that
  password rotation persists safely for a running database.
- An outcome-unknown mutation requires explicit recovery instead of appearing
  as a definitively failed operation.
