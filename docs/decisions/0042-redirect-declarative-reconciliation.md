# ADR 0042: Redirect declarative reconciliation

## Status

Accepted.

## Context

Dokploy `v0.30.6` exposes application Redirects through `redirects.create`,
`redirects.one`, `redirects.update`, and `redirects.delete`, and lists them
through the `redirects` collection of `application.one` (ADR 0030). Create
returns only a boolean, so the physical identity is discovered by comparing the
authoritative parent collection before and after the mutation. A missing
Redirect is reported by `redirects.one` as HTTP 404.

A Redirect is meaningful only inside one application. Its collision key is the
regular expression, not its logical name, because two Redirects with the same
expression on one application cannot be matched or reconciled unambiguously.
Redirect fields are routing rules, not secrets.

## Decision

Redirect is an application-contained declarative resource at
`environments.<env>.applications.<app>.redirects.<name>`. It owns exactly three
required fields: `regex`, `replacement`, and `permanent`. The two strings must
be nonempty, matching the SDK contract, and `permanent` is an explicit boolean.
No field is a descriptor, fingerprint, or write-only value.

Configuration rejects, at parse time (`DOKCFG026`), two Redirects within one
application that share a regular expression. The desired, stored, and remote
snapshot seams independently reject null, empty, and wrongly typed values for
every Redirect field, so a malformed observation fails closed before a plan
exists. `ignore_changes` may name any of the three fields.

Fresh discovery reads the `application.one` Redirect collection once for every
application that contains a desired, stored, or removed Redirect. A managed
identity is also read through `redirects.one`. The direct and collection records
must agree exactly on identity, containment, and every field. Duplicate
identities, duplicate regular expressions, a direct identity under a different
application, and a collection that names a different application block
planning. Unmanaged Redirects are matched by regular expression. A 404 from
`redirects.one` proves absence only when the parent collection is authoritative
and does not contain the identity. A partial authority downgrades absence to an
unavailable observation, which makes the plan incomplete.

Create sends the three fields and the containing application and never deploys.
Update first performs a fresh direct read, verifies identity and containment,
reads the parent collection to reject a regular-expression collision before any
journal step starts, and sends one complete replacement. Fields selected by the
plan come from the checkpoint; every other field, including an ignored one,
keeps its current remote value, so the complete replacement never overwrites a
value the configuration does not own. Delete removes the exact identity and
treats an already-missing identity as convergent. Changing the containing
application cannot happen in place: the executor deletes the old identity,
checkpoints the removal, journals a second create step with a recovery
placeholder, and records the new identity only after the SDK proves it from the
new parent collection.

An uncertain create stays in progress and is adopted only from one exact
regular-expression match in the authoritative parent collection whose fields
equal the proposed state. An uncertain update is confirmed only when the
authoritative complete state equals the proposed state, or proven not to have
happened when it equals the stored state. Transport, decoding, and
post-acceptance uncertainty never permit a blind retry; only a definitive
pre-mutation rejection may fail the journal step.

Protected import (`dokploy import redirect <id> --as redirect.<name>`) reads one
exact identity, its authoritative parent collection, and the containment chain,
rejects a duplicated expression, then writes canonical protected configuration
and state. The first fresh plan converges without further mutation.

The live acceptance uses the digest-pinned Dokploy `v0.30.6` image. It proves
undeployed create and no-op convergence, complete in-place update with a stable
identity, containment replacement with a new identity and an empty former
parent, deletion, direct and collection absence, protected import of an
out-of-band Redirect, zero deployments on every touched application, and
identity-scoped cleanup. Raw responses live in a private directory that cleanup
always discards; retained evidence is an allowlist scanned for the integration
API key.

## Consequences

- Application Redirects can be created, observed, updated, replaced, imported,
  recovered, and deleted through the declarative engine without deployments.
- Two Redirects cannot silently collide on one application, in configuration or
  on the server.
- A role that cannot see a complete collection cannot plan creations or
  deletions, because absence is never inferred from partial data.
- Moving a Redirect between logical names while also changing its fields is not
  supported by the move executor and fails closed.
