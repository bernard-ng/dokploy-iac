# ADR 0019: Public read-only plan workflow

## Status

Accepted. Completes Phase 5.

## Context

The pure planner was previously reachable only through library tests. A public
workflow must bind the exact configuration bytes, local state lineage, fresh
remote observations, and connection identity without creating a mutation path
or silently planning across concurrent state changes.

## Decision

`dokploy plan` performs one bounded configuration read and derives its source
SHA-256 digest from those exact bytes. It canonicalizes the configuration
directory as the workspace, binds state to the client's normalized instance,
rejects pending recovery before remote reads, compiles instance-bound sensitive
intent, performs authoritative fresh discovery, and rechecks recovery and the
state revision before returning the pure plan.

An absent workspace uses a deterministic nil-lineage, serial-zero planner
baseline. This sentinel is never serialized as durable state. Planning does
not create `.dokploy`, write state, or call mutation endpoints. When concrete
sensitive inputs are configured, first use may initialize the per-instance
fingerprint key in the operating-system credential store; this is local
credential bootstrap, not infrastructure mutation.

`--json` emits the canonical redaction-safe plan document. Human output exposes
only addresses, action and origin categories, property paths, metadata kinds,
and diagnostic codes. `--detailed-exitcode` returns 2 for a complete applyable
plan with changes, 0 for success without that condition, and 1 for errors or a
non-applyable/incomplete rendered plan.

Authoritative reconciliation requires credentials with full instance-wide
visibility. The current API cannot prove collection completeness from a role,
so callers make this operational assertion by invoking the public workflow.

## Consequences

- Initial and existing workspaces use the same public planning command.
- Repeated initial plans are byte-deterministic and leave no state artifacts.
- Concurrent state or recovery changes invalidate the plan instead of mixing
  snapshots.
- Saved plans and execution remain out of scope until later phases.
