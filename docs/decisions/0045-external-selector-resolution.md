# ADR 0045: External selector resolution and saved-plan binding

## Status

Accepted. Implements the selector boundary of ADR 0026 on top of the read-only
SDK contract of ADR 0036 and the saved-plan machinery of ADR 0019.

## Context

Servers, container registries, and backup destinations are external
infrastructure. The workspace selects them but never owns their lifecycle, and
their physical identities must never become durable desired state. Dokploy
associates them with managed resources in two different ways:

- `application.create` accepts the primary `serverId` only at creation, so a
  changed placement can only be converged by replacing the application;
- `application.update` accepts nullable `buildServerId`, `registryId`,
  `buildRegistryId`, and `rollbackRegistryId`, so those associations change in
  place and can be cleared.

A name is only a usable selector when exactly one fresh record carries it. The
SDK already exposes bounded, secret-free `server.all`, `registry.all`, and
`destination.all` reads that reject duplicate physical IDs and preserve
duplicate names. A saved plan must also bind the identity a selector resolved
to, because a record that is deleted and re-created under the same name yields a
byte-identical plan that now targets a different external record.

## Decision

### Selector vocabulary

Configuration selects an external record with a closed selector object:
`{ local: true }` or `{ name: "..." }`. The object form keeps a record that is
literally named `local` unambiguous. `SelectorKind` (server, registry,
destination) states which families exist and whether the local form is
meaningful: only primary server placement accepts `local`, which selects
Dokploy's own host and is sent to Dokploy as JSON `null`. A name is exact and
case sensitive, non-empty, at most 256 bytes, trimmed, and free of control
characters.

Application declares five selector fields: `server` (create-only placement),
`build_server`, `registry`, `build_registry`, and `rollback_registry` (nullable
associations). Omission leaves an association unmanaged and `null` clears one.
`server` cannot be `null` because local placement is the explicit `local`
selector. Strict parsing rejects every other shape through location-only parse
errors. Semantic validation adds three typed diagnostics:

- `DOKCFG029` the name is empty, padded, over-long, or contains control
  characters;
- `DOKCFG030` `local` is used outside primary server placement;
- `DOKCFG031` `server` is `null`.

The canonical writer, typed import documents, and generated JSON schema model
the same closed shapes. All five paths can appear in `lifecycle.ignore_changes`;
an ignored selector is neither resolved nor read.

### Planner vocabulary

Selector values are stable name objects (`{"local": true}` or `{"name": "..."}`),
stored that way in desired state, plan checkpoints, durable state, and the
journal. The planner validates them independently at the desired, stored, and
remote seams. Server placement accepts only a concrete selector. The mutation
contract makes `server` a replacement property (delete before create) and every
other association an in-place property that can also be cleared, so a changed
association is classified exactly as Dokploy's endpoints allow.

### Resolution during discovery

Discovery reads each required external collection at most once per run, only
for the kinds that desired, non-ignored selectors need, immediately after
environment topology and before application observation. A configuration
without selectors performs no extra request. The collections are the complete
bounded `*.all` reads, so they are treated as authoritative; a read failure or a
contract violation (including duplicate physical IDs) makes the selector
unavailable rather than guessed.

Resolution is exact-name matching with four closed outcomes: `Local`,
`Resolved(identity)`, `Unmatched`, `Ambiguous`, plus `Unavailable(reason)` for a
failed collection. Response order never selects a record. An observed
association is mapped back through the same collection into a name selector, so
the planner compares like with like. A duplicated name stays observable; the
desired selector's own resolution reports the ambiguity. An attached identity
that is absent from the collection or whose name violates the selector grammar
is an unknown observation.

Zero matches, multiple matches, an unavailable collection, or a missing
resolution block planning of the affected resource with `DOKPLAN019`, carrying
the address, the selector property, and a closed `selectorFailure` of
`unmatched`, `ambiguous`, `unavailable`, or `unobserved`. Diagnostics never
contain a name or an identity.

### Where identities live

Resolved identities exist only in non-serializable, Debug-redacted memory:
`RemoteState` carries the fresh resolutions for the keyed receipt, and the
compiled execution sidecar receives a copy of every uniquely resolved
identity just before execution. They never appear in plans, saved-plan
envelopes, durable state, journals, errors, or Debug output.

### Saved-plan binding

The keyed remote binding receipt covers each desired selector's resolution
(address, selector property, and resolved identity or failure) in addition to
every observation. A receipt without selectors is byte-identical to the
previous format, so saved plans for configurations that do not use selectors
remain valid. Apply rebuilds the plan, re-resolves every selector, and refuses a
saved plan whose resolution changed: a record that was removed, renamed,
duplicated, or deleted and re-created under the same name changes the receipt
without changing the plan JSON. The envelope contains neither identities nor
selector names.

### Execution

A create sends the resolved server placement in `application.create` (explicit
null for `local`) and the nullable associations in the follow-up
`application.update`; neither deploys. A registry or build-server change is one
in-place update that sends the freshly resolved identity or an explicit null.
Association changes never request a deployment.

A changed server placement replaces the application: the old application is
deleted and checkpointed, a second recoverable journal step creates the
replacement, and the new identity is recorded only after the create response
proves it. Replacement is refused, before any mutation, while durable state
contains a resource contained by or depending on the application, because
Dokploy removes an application's Ports and detaches its Domains while the
planner does not yet cascade a replacement to them. Placement is also resolved
before the destructive step so a stale selector cannot strand the workspace.
A protected application cannot be replaced.

The recoverable create target stays minimal, so an uncertain create is adopted
from the authoritative collection identity alone and the owned associations
follow in a journaled checkpoint step. Recovery confirms an uncertain update
only when the freshly observed association equals the proposed selector, and
requires manual intervention when any selector of the resource no longer
resolves to exactly one record.

### Import

Import writes each existing association as a name selector in the generated
configuration and durable state; application import keeps its existing
unprotected default. It reads the minimal collections
only when the application has an association, and fails closed with one closed
diagnostic when an identity is unknown, its collection is unreadable, its name
is shared by another record, or its name is outside the selector grammar. An
association that is null or not returned stays unmanaged. The first fresh plan
converges without mutation.

### Live acceptance

The digest-pinned Dokploy `v0.30.6` run creates disposable servers, registries,
and a destination, resolves them through the real selector seam, and proves
creation, in-place association updates, create-only replacement, the local
selector, unmatched and ambiguous blocking, saved-plan acceptance and refusal,
and import, with zero deployments. Servers and the destination point
at a connection tripwire. `registry.create` runs `docker login` before it stores
a registry, so registries point at a loopback `/v2/` responder; the run asserts
that it saw only login probes and that no further traffic occurred after the
records existed. Cleanup is scoped to the run's name prefix and proven by
authoritative absence, and retained evidence is scanned for the API key, the
fingerprint key, and every secret and identity canary.

## Consequences

- Application server, build-server, and registry associations are configured by
  stable name and converge with the correct in-place or replacement semantics.
- A saved plan applies only while every selector still resolves to the same
  external identity.
- A renamed, removed, duplicated, or re-created external record blocks planning
  or invalidates a saved plan instead of silently changing the target.
- The resolver seam is kind-agnostic and already supports backup destinations.
  The Backup and Schedule adapters add a selector path, a `SelectorKind`
  binding in the desired compiler, and an association observation; resolution,
  the diagnostic, the receipt, and the sidecar need no change.
- Application replacement is conservative: it is unavailable while durable
  dependents exist, until a cascading replacement is designed.

## Wiring for Backup destinations and Schedule server scopes

Each new consumer adds, and nothing else changes:

1. a `PropertyPath` variant that `is_external_selector`, valid for its kind,
   plus its stable string, checkpoint arm, and selector-shape validation (the
   shared `selector_json_valid` accepts `name`, and `local` only for server
   placement);
2. a typed selector field in configuration with a `SelectorKind` (`Destination`
   or `Server`), plus the `DOKCFG` checks for `local` and clearing;
3. a `compile_selector_field` call and a selector binding in the desired
   compiler, so `external_selectors()` lists it;
4. an observation arm that maps the attached identity through
   `ExternalDirectory::observe_association`, and the kind in
   `required_external_kinds` (already driven by the bindings);
5. the mutation contract (replacement when the endpoint is create-only, in place
   otherwise) and the executor input built from `external_id`.

`ExternalDirectory::load` already reads `destination.all`; the live acceptance
resolves a destination through it.
