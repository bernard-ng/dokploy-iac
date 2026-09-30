# ADR 0035: Bounded SDK response bodies

## Status

Accepted. Extends ADR 0003's owned transport contract.

## Context

Every handwritten and imperative SDK response was previously collected into an
unbounded byte buffer before JSON decoding. Collection item limits constrained
the decoded models but could not prevent a malformed, hostile, or unexpectedly
large HTTP response from exhausting client memory. `Content-Length` alone is
not sufficient because responses can omit it, use chunked transfer encoding,
or expand after transport decoding.

Response validity also affects mutation recovery. Once Dokploy accepts a
mutation request, a malformed or oversized successful response does not prove
whether the mutation completed. Treating that failure as a definitive decode
error could authorize an unsafe retry.

## Decision

The SDK accepts at most `MAX_JSON_RESPONSE_BYTES`, fixed at 16 MiB, for one
decoded JSON response. This leaves substantial headroom for topology,
pagination, Compose documents, and imperative responses while placing a clear
memory bound at the untrusted HTTP seam.

One shared reader rejects a declared length over the limit before reading a
body. It otherwise consumes `Response::chunk()` incrementally, checks each
addition for integer overflow and limit crossing before extending the buffer,
and owns the accumulator through `Zeroizing<Vec<u8>>`. The same reader serves
typed responses, imperative responses, ordinary error payloads, and successful
secret-aware responses. Secret-aware non-success responses continue to skip
server body decoding entirely.

An oversized successful read is an unexpected response; malformed successful
read JSON remains a decode error. An oversized or malformed successful mutation
response is `OutcomeUnknown` because completion cannot be proved and the SDK
must not retry. An oversized non-success response keeps only its HTTP status,
stable fallback code, and canonical status message. The oversized server body
is discarded and cannot enter diagnostics.

## Consequences

- Every buffered JSON transport path has a fixed memory ceiling independent of
  collection-level validation.
- Exactly 16 MiB is accepted; the next byte fails before buffer extension.
- Chunked and undeclared-length responses receive the same bound as declared
  responses.
- Large remote error bodies lose server-specific details but preserve a safe,
  structured status.
- Operators must use a purpose-built streaming endpoint rather than raise this
  JSON ceiling if a future contract legitimately transfers larger artifacts.
