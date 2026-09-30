# ADR 0018: Adapter-projected mutation contracts

## Status

Accepted for Phase 5 planning.

## Context

Property equality cannot determine whether Dokploy can update a field, must
replace a resource, or cannot perform the transition safely. Containment
changes have the same ambiguity. Keeping these facts in the pure planner would
couple the domain model to Dokploy endpoints; guessing them in the executor
would make plans misleading.

## Decision

Every fresh remote observation has a matching value-only `MutationContract`.
The CLI adapter projects fields allowed during creation, the subset required
during creation, set and clear behavior after creation, containment behavior,
and a fixed safe replacement order. A required create field is always allowed,
but an optional create-only field is not made required merely because the
update endpoint cannot change it later. The core planner contains no SDK
callbacks or transport types.

The planner classifies supported containment changes as `reparent`, proven
replacement transitions as `replace`, and attaches `create_before_delete` or
`delete_before_create` ordering. Logical containment changes caused by a
parent address move may be explicitly state-only. Unsupported transitions and
missing create inputs produce stable blocking diagnostics. Replacement honors
durable protection because it necessarily deletes the old identity.

User replacement metadata may promote an otherwise in-place property change
to replacement. It cannot make an unsupported adapter transition safe. A move
combined with replacement metadata remains blocked until the executor has a
recoverable combined sequence.

## Consequences

- Plans state the physical strategy before Phase 6 mutation code exists.
- Set and clear contracts remain distinct, preventing an observed write API
  from being treated as proof that clearing is safe.
- Domain application changes are delete-before-create replacements; duplicate
  hosts make create-before-delete unsafe.
- Phase 6 must implement the selected order with durable journal stages and
  checkpoint the new physical identity.
