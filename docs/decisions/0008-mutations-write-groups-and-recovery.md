# ADR 0008: Mutations, write groups, and recovery

## Status

Proposed. Keeps invariants 5, 6, 15, 16, and 19 and the journal design of the
first engine's ADR 0005.

## Context

Dokploy writes one object through several endpoints (an application has
`update`, `saveEnvironment`, `saveBuildType`, and five `save*Provider`
endpoints); some updates must resend the whole object; some creates return an
identity, others do not; some changes force a replacement. Each of these was
hand-coded per kind.

## Decision

### Write groups

A spec partitions a kind's `in_place` fields into **write groups**. A group
names an operation, the fields it carries, and a body shape:

- `partial`: send the id plus only the changed fields of the group;
- `full`: send the id plus the complete group from a fresh read, overlaid with the
  changes (the "fresh-read complete replacement" used for Redirects, Vault
  providers, and Registries);
- `by_variant`: the operation depends on a union tag (application source: one
  `save*Provider` endpoint per arm).

The executor builds one request per group that has a change, in the order the spec
lists them, and journals each as its own step. A group whose body must carry a
secret (`resend_secret: true`) is refused unless the secret source is declared in
configuration; otherwise the plan reports the missing source.

### Create identity

Dokploy does not type create responses, so a spec declares how the new identity
is learned, in order of preference:

1. `from_response: <pointer>`: read it from the create response;
2. `diff_collection: {key: [...]}`: list before and after and take the one new
   item matching the natural key;
3. `none`: the outcome is **unknown** and recovery resolves it by observation.

Whichever is used, a transport failure after the request was sent is an
**outcome-unknown** result: the journal step stays in progress, the mutation is
never retried (invariant 16), and `recover` decides from fresh evidence.

### Replacement

Changing a `create_only` field plans a replacement. The spec states the order:
`delete_then_create` (name-keyed objects that cannot coexist) or
`create_then_delete`. Replacement is refused for protected resources and for
resources with dependents the engine cannot recreate (a service with Mounts,
Schedules, or Backups), exactly as before, with a diagnostic naming them.

### Ordering and concurrency

The plan orders changes by the dependency graph (containment and `ref`s); creates
run parents first, deletes children first, replacements per the spec. Independent
steps may run concurrently up to `--parallelism`; every step is journaled before it
starts and checkpointed to state when it succeeds, as before.

### Verification

After a step the engine re-reads the resource through the same projection and
checks that every property it just wrote reads back as written (except
`write_only`). A mismatch is reported as a failed verification, not retried. This
turns the "apply then plan is empty" invariant (22) into a per-step check.

### Failure

No automatic rollback (invariant 19). A definitive Dokploy rejection fails its
step; later independent steps still run; dependents of a failed step are skipped
and reported.

## Consequences

- The executor is one piece of code. Per-kind create, update, and replace bodies
  disappear from Rust.
- Kinds with unusual flows (tag membership, Compose multi-service schedules) use a
  hook for the one step that differs and the generic path for the rest.
- A spec that declares the wrong write group fails the conformance suite against
  the simulator, and the live contract tests against Dokploy (ADR 0015).
