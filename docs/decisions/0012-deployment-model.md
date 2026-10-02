# ADR 0012: Deployment model

## Status

Proposed.

## Context

Dokploy creates applications, Compose services, and databases undeployed. A
configuration that is applied but never deployed leaves an instance of stopped
services, so "the same state as before" is not reached. The first engine had no
deploy at all, deliberately, and asserted that no deploy call was ever made.

## Decision

**Applying and deploying are different things.** `apply` reconciles configuration
and never deploys unless told to. `deploy` is its own verb with its own plan.

### Policy

Each deployable resource carries `lifecycle.deploy`:

| Value | Meaning |
|-------|---------|
| `never` (default) | the tool never deploys it |
| `on_create` | deploy once after it is created |
| `on_change` | deploy after it is created and after any change to a property marked `needs_deploy` |

The spec marks each field `needs_deploy: true|false`. Image, command, environment,
source, build, resources, Swarm settings, networks, domains, mounts, and Compose
document need a deploy; description, tags, and schedules do not.

### Commands

- `dokploy deploy [--target <address>...] [--wait] [--timeout 20m]` plans and
  performs deploys for resources whose policy and pending changes call for one, or
  for the named targets regardless of policy.
- `dokploy apply --deploy` applies, then deploys what the apply made necessary.
- `--wait` (default on in a terminal, off with `--json`) polls the deployment record
  until it reports `done` or `error`, or the timeout elapses. A timeout is reported,
  not an error of the tool.

### Ordering

Deploys follow the dependency graph (databases before the applications and Compose
services that use them, by `depends_on` and `ref`), run up to `--parallelism` at a
time, and a failed deploy skips the deploys that depend on it. A failure is not
rolled back (invariant 19).

### Journal and recovery

A deploy is journaled as an *action* step. It is idempotent from the tool's point of
view, so an outcome-unknown deploy is resolved by reading the latest deployment
record; if none started, it is retried only after confirming none is queued.

### Operations that stay out

Start, stop, restart, rebuild, rollback, cancel, kill, and queue cleanup are not
state. They remain available as imperative commands (`dokploy api application stop`).

## Consequences

- A rebuild is `apply` then `deploy`, or `apply --deploy`.
- Plans gain a separate *Deploy* section; saved plans bind to it.
- Compose-based services deploy through `compose.deploy`, applications through
  `application.deploy`, and databases through `<kind>.deploy`; the spec names the
  operation per kind.
