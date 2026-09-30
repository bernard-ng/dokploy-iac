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
debug output. Public direct and collection models keep only command and script
presence. Create and update decode their immediate response into a private
zeroizing proof model, compare the exact executable bytes with the request,
then discard them before returning. Secret-aware transport paths zeroize the
bounded response buffer. Remote rejections preserve the status code while
dropping server-provided messages and issues that could echo executable text.

Create reads the authoritative target collection before mutation and rejects a
name collision. It attempts `schedule.create` once, validates the returned
nonempty identity, target, name, every safe mutable field, and exact command and
script bytes. It then compares the preflight and postflight identity sets. All
preexisting identities must remain and exactly one new identity must equal the
returned identity and record. Reused identities, unrelated concurrent
additions, and mismatched executable bodies are outcome-unknown. Direct reads
require the same safe record in `schedule.list`. Collections reject more than
10,000 records, invalid or contradictory targets, and duplicate identities or
names.

Update first reads `schedule.one` and requires agreement with the authoritative
supported-target collection. The physical identity must belong to the caller's
expected target, and the desired name must not collide with another identity
in that collection. It then sends every mutable field without serializing the
target. The returned row must match the target, safe fields, and exact command
and script bytes, then agree with the authoritative collection. Target or
service changes remain replacement operations for later declarative work.

Delete requires both the physical identity and a supported `ScheduleTarget`.
It performs the same direct and collection proof before POST, preventing a bare
server or Dokploy-server Schedule identity from crossing the mutation seam.
After an accepted delete, target-collection absence is required; a failed
postflight proof is outcome-unknown.

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
- Bare privileged Schedule identities cannot reach update or delete.
- Target changes cannot pass through the in-place update seam.
- Uncertain mutations require recovery; the SDK never guesses or retries.
