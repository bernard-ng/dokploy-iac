# ADR 0037: Sensitive SDK error bodies

## Status

Accepted. Extends ADR 0003's owned transport contract and ADR 0035's bounded
response-body contract.

## Context

Dokploy can include submitted or stored values in a non-success response. The
general SDK decoder preserves remote error codes, messages, and issues as owned
strings. That behavior is useful for ordinary validation errors, but it can
retain credentials, Compose documents, mount file content, environment values,
commands, or scripts when an affected endpoint echoes them.

The risk is endpoint-wide. It includes safe response models backed by a richer
Dokploy endpoint, shared reads such as `application.one`, and deletes whose
remote errors can include the deleted resource's secret-bearing state.

## Decision

The SDK keeps one fail-closed endpoint policy for non-success response bodies.
An explicit allowlist identifies endpoints whose structured error text is safe
to preserve. Every other endpoint, including generated operations not used by
the handwritten client, is sensitive by default. Sensitive endpoints collect
the remote body only through the bounded zeroizing reader, discard it before
error decoding, and return only the HTTP status, the stable status-derived
code, the canonical status message, and an empty issues list.

The sensitive inventory covers:

- project, environment, and application reads that can expose environment
  values, plus deletion of those secret-bearing resources;
- Compose and mount reads and mutations;
- Security reads and mutations;
- Schedule reads and mutations;
- server, registry, and backup-destination selector reads;
- Postgres, LibSQL, MySQL, MariaDB, MongoDB, and Redis credential-bearing reads
  and mutations, including their deletes.

Typed secret-aware helpers assert that their endpoint is in the policy and then
use the shared decoder. The imperative API uses the same policy, so it cannot
bypass sanitization by calling an owned sensitive operation directly. The test
registry partitions every operation used by the handwritten client, including
`application.deploy`, and separately proves that an unregistered generated
operation fails closed.

The bounded zeroizing response reader remains the only response-body buffering
path. Mutation transport and decode failures retain the existing
`OutcomeUnknown` classification.

## Residual transport limitation

Sensitive input models zeroize owned values and redact diagnostics, but the SDK
does not claim that serialized request bytes are zeroized. Reqwest's JSON body
construction serializes through an ordinary byte buffer and retains immutable
request bytes for transport and retry handling. The current transport does not
offer a supported way to scrub every copy after send.

TLS and short request lifetimes reduce exposure but do not provide memory
zeroization. A stronger guarantee would require a transport abstraction that
owns serialization and request-body buffers end to end.

## Consequences

- Remote text from sensitive endpoint failures cannot enter `DokployError`
  strings, displays, or debug output.
- Callers retain stable HTTP classification without receiving server-specific
  failure details for those endpoints.
- Ordinary validation endpoints keep actionable structured errors.
- Adding an owned endpoint is safe by default. Preserving its remote error text
  requires an explicit allowlist entry and corresponding registry coverage.
