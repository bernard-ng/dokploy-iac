# ADR 0038: LibSQL declarative reconciliation

## Status

Accepted.

## Context

Dokploy `v0.30.6` exposes LibSQL through `project.one` topology, a direct read,
and typed create, metadata update, password-change, and delete operations.
Creation returns only a Boolean. Identity must therefore be proved from a
bounded before-and-after topology comparison rather than accepted from the
mutation response.

A LibSQL record is either a primary or a replica with one nonempty primary
URL. The two shapes are mutually exclusive. Description and username are
readable. The password is write-only, and live evidence proves that the
password-change operation persists for an idle, undeployed record. External
server selection is not an owned property in the current declarative model.

## Decision

LibSQL is an environment-contained declarative resource. Fresh discovery reads
the exact project through `project.one`, selects the exact environment, and
reads every managed record directly. The topology and direct read must agree
on physical identity, logical name, environment, and description. Missing,
partial, duplicate, or contradictory evidence blocks planning. `project.all`
is used only for ancestor discovery; its LibSQL entries are too sparse to be
an identity authority.

Description, username, and password update in place. Description and username
share one metadata mutation. When password changes in the same plan, it uses a
separate journaled step so recovery can distinguish readable metadata from a
write-only credential outcome. Password values remain one-shot inputs; only
instance-bound fingerprints enter state and recovery checkpoints.

The node is one atomic managed property. Changing between primary and replica,
or changing a replica's primary URL, replaces the record in
delete-before-create order. The executor checkpoints deletion before starting
the create journal. A crash while deletion is outcome-unknown preserves the
old state. A crash after the deletion checkpoint leaves the resource absent,
and a crash during creation leaves a recoverable create step. No recovery path
blindly retries an uncertain create.

Creation uses the typed SDK's bounded `project.one` comparison and then
requires a direct read of the discovered identity to agree on ID, environment,
and name before checkpointing it. Unexpected success shapes and uncertain
transport outcomes leave the create step in progress.

Recovery adopts an uncertain create only when one authoritative topology match
and its direct read match the proposed readable state. Authoritative absence
confirms that no create occurred. An interrupted metadata update may use
exact-except-sensitive comparison only when before and proposed password
fingerprints are identical. An interrupted password rotation requires manual
intervention because the credential cannot be read back.

Import reads a direct LibSQL identity, writes canonical protected
configuration with its atomic node, adopts the physical identity, and leaves
password unmanaged. The first fresh plan must converge without reading or
inventing a credential.

The live acceptance check uses the digest-pinned Dokploy `v0.30.6` image
without deploying LibSQL. It proves create and no-op convergence, metadata
update, password rotation, node replacement with a new physical identity,
deletion, topology and direct absence, and secret exclusion.
Because `libsql.one` returns the database password, the check validates its
private raw response and atomically replaces it with an allowlisted identity,
node, and status projection before retaining or scanning evidence.

External server selectors remain deferred. They will not enter configuration,
state, discovery, or mutation requests until their ownership and replacement
semantics have separate live evidence.

## Consequences

- LibSQL can be created, observed, updated, imported, recovered, replaced, and
  deleted through the declarative engine.
- Description, username, and password drift can be corrected in place.
- Node changes intentionally destroy and recreate an undeployed record.
- Replacement recovery uses two durable steps and never guesses across an
  uncertain mutation.
- Physical reparenting and external server selection remain unsupported.
- Independent LibSQL creates may overlap other database mutations; LibSQL
  metadata, password, replacement, and delete operations remain ordered.
