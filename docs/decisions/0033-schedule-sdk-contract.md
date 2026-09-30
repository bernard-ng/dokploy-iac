# ADR 0033: Secret-safe target-scoped Schedule contract

## Status

Accepted. Extends Phase 8 resource breadth.

## Context

Dokploy `v0.30.6` exposes Schedules through `schedule.create`,
`schedule.one`, `schedule.update`, `schedule.delete`, and `schedule.list`.
Schedules can address applications, Compose services, servers, or the Dokploy
server itself. Reads return executable command and script text. The generated
OpenAPI surface permits privileged target strings that have not been designed
or proven safe for declarative management.

`schedule.list` is the authoritative target-scoped collection. A Schedule name
is the collision key within one target. Create returns a complete record with
the new physical identity. Create, update, and delete are non-idempotent POST
operations whose outcome is unknown when the accepted mutation cannot be
proved.

## Decision

The handwritten SDK owns `ScheduleId`, `ShellType`, complete create and update
inputs, safe `ScheduleDetails`, and a bounded `ScheduleCollection`.
`ScheduleTarget` is a closed union containing Application and Compose targets.
A Compose target requires both its `ComposeId` and service name. Host and
Dokploy-server targets are rejected rather than represented as raw strings.

Command and script inputs are owned by `Zeroizing<String>` and redacted from
debug output. Secret-aware read and mutation paths discard response bytes after
decoding and keep only command and script presence. Remote rejections preserve
the status code while dropping server-provided messages and issues that could
echo executable text.

Create reads the authoritative target collection before mutation and rejects a
name collision. It attempts `schedule.create` once, validates the returned
nonempty identity, target, name, and every safe mutable field, then requires
the same record in a postflight `schedule.list`. Direct reads require the same
record in that collection. Collections reject more than 10,000 records,
invalid or contradictory targets, and duplicate identities or names.

Update sends every mutable field while retaining the expected target only for
validation. It cannot change an application, Compose owner, or Compose service
in place. The returned row must match the expected target and fields, then
agree with the authoritative collection. Target or service changes remain
replacement operations for later declarative work. Collection absence is the
authoritative deletion proof.

Any transport interruption, malformed successful mutation response, or failed
post-mutation proof is outcome-unknown and is never retried automatically.
Definitive preflight validation and non-success HTTP responses remain ordinary
request failures.

The pinned live capture and SDK test use undeployed disposable Application and
Compose targets. Both lifecycles keep the Schedule disabled and prove create,
direct and collection agreement, complete safe-field update, delete,
authoritative absence, zero executions and deployments, and project cleanup.
Fixtures are published only after a sanitized whole-tree candidate passes API
key, command, script, and contract checks.

## Consequences

- Executable command and script text does not enter safe models, debug output,
  structured errors, or tracked fixtures.
- A Compose service name is part of target identity rather than an optional
  descriptive field.
- Privileged Schedule targets remain unavailable until their authority and
  secret boundaries receive separate evidence.
- Concurrent or contradictory collection changes prevent identity adoption.
- Target changes cannot pass through the in-place update seam.
- Uncertain mutations require recovery; the SDK never guesses or retries.
