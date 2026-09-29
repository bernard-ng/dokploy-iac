# ADR 0005: Durable operation journals

## Status

Accepted for the initial MVP.

## Context

A remote mutation can succeed before its resource identity reaches local
state. Blindly repeating an uncertain create can duplicate infrastructure, and
writing state before recording the remote result can lose the only durable
evidence needed for recovery.

## Decision

Write one owner-only JSONL journal per operation under `.dokploy/journal/`.
Every record is serialized as one strict JSON object followed by a newline,
then flushed and synchronized before the operation continues.

The journal API exposes semantic operations instead of raw record appends:

1. `Begin` records the format version, operation ID, starting state revision,
   and plan digest.
2. `StepStarted` is durable before the remote mutation begins.
3. `StepSucceeded` is durable before the corresponding state checkpoint.
4. `StepFailed` stores only a constrained failure code and makes the journal
   terminal.
5. `Commit` is durable only after the exact current state revision is verified.

A successful step must match the exact journaled state transition. Creates add
only the addressed resource with the recorded remote ID, deletes remove only
that resource, and updates or deployments preserve its remote identity while
leaving other resources unchanged. A mismatch leaves the success record as
recovery evidence and does not checkpoint state.

The scanner rejects duplicate or unknown fields, invalid filenames, operation
ID mismatches, sequence gaps, multiple open steps, action mismatches, records
after terminal records, incompatible state revisions, and configured size or
record-limit violations. One incomplete journal requires recovery; multiple
incomplete journals are ambiguous and fail closed. Ordinary writer acquisition
refuses both states.

A trailing non-empty fragment without a newline is treated as a recoverable
crash tail only when all preceding complete records form a valid non-terminal
history. The fragment is never interpreted or truncated automatically. A
complete malformed line, a fragment after `Commit`, or a fragment after a
complete `StepFailed` is corruption.

## Consequences

- An uncertain remote mutation is preserved for fresh inspection instead of
  being retried automatically.
- The same write session cannot bypass an incomplete or poisoned journal.
- Journal files contain addresses, actions, revisions, IDs, digests, and
  constrained failure codes, but no managed inputs or arbitrary error text.
- Detection is implemented; an interactive `recover` command remains a later
  phase because it requires resource-specific fresh-remote reconciliation.
- Crash and power-loss behavior is covered through durable-state fixtures and
  injected failure stages. Subprocess power-loss simulation and broader
  non-Unix durability coverage remain future hardening work.
