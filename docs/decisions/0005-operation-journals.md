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
history. Recovery first copies that fragment into an owner-only archive and
then trims it durably. A complete malformed line, a fragment after `Commit`, or
a fragment after a complete `StepFailed` is corruption.

Format version 2 records the exact expected checkpoint before every mutation.
`dokploy recover` holds the ordinary workspace writer lock, performs fresh
resource-specific reads, and classifies the unresolved step without retrying
it. A uniquely matching uncertain create may be adopted; a confirmed missing
delete may be checkpointed; and a resource still carrying its prior identity
can confirm that deletion made no change. Updates and deployments are
checkpointed only when every owned readable value matches. Write-only or
ambiguous outcomes fail closed for manual review.

## Consequences

- An uncertain remote mutation is preserved for fresh inspection instead of
  being retried automatically.
- The same write session cannot bypass an incomplete or poisoned journal.
- Journal files contain addresses, actions, revisions, IDs, digests, exact
  expected checkpoints, and constrained failure codes. Sensitive values remain
  opaque receipts, and arbitrary remote error text is never persisted.
- Explicit recovery is available through a preview and exact approval; CI may
  opt into the same verified action with `--auto-approve`.
- Crash and power-loss behavior is covered through durable-state fixtures and
  injected failure stages. Broader non-Unix durability coverage remains future
  hardening work.
