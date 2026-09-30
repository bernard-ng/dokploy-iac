# ADR 0020: Journaled bounded execution

## Status

Accepted. Completes Phase 6.

## Context

Apply must make useful progress across independent resources without allowing
concurrency to outrun durable recovery evidence. A remote mutation can succeed
after a sibling has already failed, and interruption can occur between a remote
response and its state checkpoint. Automatic rollback would introduce more
remote mutations with equally uncertain outcomes.

## Decision

`dokploy apply` compiles and discovers under the exclusive workspace writer
lock, renders that fresh plan, and requires exact interactive confirmation.
Unsupported, incomplete, or non-applyable plans are rejected before a journal
is opened.

Every remote mutation receives a durable `stepStarted` record before dispatch.
The journal may contain several open steps with unique increasing sequences.
State checkpoints remain serialized and advance exactly one state serial per
successful step. Once any step fails, the journal refuses new work but accepts
results from steps that were already open. Those successful siblings are
checkpointed; the operation cannot commit and remains recovery-required.

The Phase 6 scheduler overlaps adjacent independent Postgres and Redis
mutations up to the validated `--parallelism` bound, which defaults to four and
accepts values from one through 64. Project and environment containment,
application configuration and deployment, and domain attachment remain ordered
because they carry hierarchy or multi-step dependencies.

No automatic rollback is attempted. The durable journal and checkpointed state
are the source of truth for the recovery workflow delivered in Phase 7.

## Consequences

- Successful remote work is never hidden merely because a sibling failed.
- A failure stops later scheduling while allowing in-flight calls to settle.
- Recovery scanning distinguishes open, failed, checkpointed, and committed
  outcomes even when step completion order differs from start order.
- Apply can converge all MVP resources, while destructive changes, replacements,
  moves, and explicit recovery remain unavailable until their later phases.
