# ADR 0003: Owned transport contract

## Status

Accepted for the initial MVP.

## Context

The generated Dokploy client exposes request types and HTTP methods, but its
response parsers are not a safe SDK seam. The pinned OpenAPI document models
success responses as empty objects, declares strict error objects, and omits
some statuses seen at runtime. A real Dokploy `401` response can contain only a
message, while the generated unauthorized type requires a code. Generated
parsers also discard bodies for unknown statuses.

The first SDK transport used the generated client's `reqwest::Client` and base
URL but assembled paths, methods, and query values by hand. That gave generated
bindings no runtime leverage and allowed imperative callers to pair an endpoint
with the wrong HTTP method.

Mutation transport failures have a separate safety concern. Once a POST has
been dispatched, losing the connection does not reveal whether Dokploy applied
the mutation. Treating that failure like an ordinary request error invites an
unsafe retry.

## Decision

The code-generation task derives endpoint metadata for every OpenAPI operation
and commits it beside the generated request types. Generation owns the metadata
file and a lookup keyed by Dokploy's wire operation; manual edits fail the
code-generation drift check.

The handwritten SDK is the transport module. Typed operations instantiate and
validate generated request types, serialize their generated query or body
types, and use generated endpoint metadata for the method and path. Imperative
operations resolve the same metadata and reject an unknown operation or method
mismatch before transport.

The SDK continues to read response bytes and decode success and error payloads
itself. It deliberately bypasses generated response parsers so sparse live
errors, unknown fields, and unlisted statuses remain available to tolerant
handwritten decoding.

The HTTP client uses reqwest 0.13's built-in, host-scoped retry policy. GET
operations may retry transient transport failures and status codes `408`,
`425`, `429`, `500`, `502`, `503`, and `504`, with at most two retries. POST
operations are never retryable. This operation-aware policy stays in the owned
transport instead of adding an older retry-middleware client layer.

A transport failure for a mutating method (`POST`, `PUT`, `PATCH`, or `DELETE`)
before a complete response is classified as `OutcomeUnknown`. The current
Dokploy contract uses POST for its mutations, but the conservative rule also
covers methods introduced by a future contract. The error retains only the
generated operation name and the transport source. It never retains the request
body, multipart contents, API key, or other secret-bearing input. A received
HTTP error remains a structured Dokploy error because the server supplied an
explicit outcome.

## Consequences

- OpenAPI method, path, query shape, and validation changes affect SDK runtime
  behavior after regeneration.
- Generated response-schema defects remain isolated behind the SDK interface.
- Callers can distinguish a safe-to-handle remote rejection from a mutation
  whose outcome must be reconciled from fresh remote state.
- Adding an owned typed operation requires choosing a tolerant response model,
  but no duplicated path or HTTP-method declaration.
