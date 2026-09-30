# ADR 0032: MariaDB declarative reconciliation

## Status

Accepted.

## Context

Dokploy `v0.30.6` exposes MariaDB through an environment-scoped collection, a
direct read, and typed create, metadata update, password-change, and delete
operations. Creation requires the database-user password but permits Dokploy to
generate the root password. Reads do not safely reveal either credential, and
durable IaC artifacts must never contain their plaintext.

The password-change endpoints reject an idle, undeployed MariaDB record because
there is no running database container. That result does not prove safe
post-create rotation or persistence. External server selection is also not an
owned MariaDB property in the current declarative model.

## Decision

MariaDB is an environment-contained declarative resource. Fresh discovery
reads each bounded environment-scoped collection required by desired or stored
state and then reads every managed record directly. The two views must agree
on physical identity, logical name, and containment. Duplicate scoped names,
duplicate physical identities, contradictory reads, and physical reparenting
fail closed.

Database and username are readable managed properties and may update in place.
The user password is required on create. The root password is allowed on create
but optional, so omitting it delegates generation to Dokploy. Both values are
descriptor-only configuration inputs whose desired, plan, state, checkpoint,
and journal representations contain separate instance-bound fingerprints.
Changing, clearing, or adding either credential after creation is unsupported.

Create, metadata update, and delete use the typed SDK operations and participate
in the bounded database executor. Each successful mutation is journaled and
checkpointed. Definitive API rejections close a journal step as failed, while
outcome-unknown transport, decoding, or unexpected-response failures leave the
step in progress for explicit recovery.

Recovery never retries an uncertain create. It adopts exactly one
environment-scoped name match only when the direct read agrees with the
collection and the readable properties match the proposed checkpoint. An
interrupted metadata update may use exact-except-sensitive comparison only
when the before and proposed sensitive fingerprints are identical.

Import accepts a direct identity or an interactive environment-scoped
selection. It writes canonical protected configuration, adopts the physical
identity, records the readable database and username, and leaves both password
properties unmanaged. The first fresh plan must converge without reading or
inventing credentials.

The live acceptance check uses the digest-pinned Dokploy `v0.30.6` image
without deploying MariaDB. It proves creation without a supplied root password,
no-op convergence, fail-closed user and root password changes, in-place database
and username updates, deletion, authoritative absence, and secret exclusion.

External server selectors remain deferred. They will not enter configuration,
state, discovery, or mutation requests until their ownership and replacement
semantics have separate live evidence.

## Consequences

- MariaDB can be created, observed, updated, imported, recovered, and deleted
  through the declarative engine.
- Dokploy may generate the root password when configuration omits it; the IaC
  workspace does not claim to know or recover that generated value.
- Database and username drift can be corrected in place.
- User-password and root-password changes after creation remain deliberately
  blocked, including attempts to add a root password that was initially
  omitted.
- Physical reparenting and external server selection remain unsupported.
- Outcome-unknown mutations require explicit recovery instead of appearing as
  definitive failures.
