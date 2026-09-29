# ADR 0010: Opaque sensitive intent comparison

## Status

Accepted for the Phase 5 planner.

## Context

Dokploy does not return comparable password or environment-secret values.
Treating every write-only observation as unknown blocks convergence, while
assuming equality hides requested rotations. Durable state now stores a keyed
HMAC receipt for each non-null sensitive intent, so the planner can compare
configuration intent without reading secret bytes.

## Decision

The core planner wraps a durable `SensitiveFingerprint` in an opaque
`SensitiveIntent`. Its interface accepts a fingerprint, supports equality, and
redacts debug output. It has no accessor for the MAC or key identifier.
`OwnedValue::Sensitive` carries this wrapper through desired state, stored
state, and immutable in-memory checkpoints.

A remote `Unknown(Sensitive)` observation is conclusive evidence that a
sensitive property is write-only, not evidence that its value drifted. The
planner therefore compares desired and stored receipts:

- Equal receipts require no write.
- A new or changed receipt requires a write.
- `KnownAbsent` requires restoration and is reported as drift when a receipt
  was stored.
- Explicit null is a separate clear intent and requires a write when it
  differs from stored ownership.
- Omission relinquishes ownership through a state-only checkpoint and never
  writes the remote property.

Missing property observations, unavailable resources, and physical identity
mismatches continue to fail closed. Unknown reasons other than the sensitive
write-only contract remain blocking.

Receipt material is excluded from canonical plan JSON, field-change payloads,
diagnostics, drift records, and debug output. Plan metadata exposes only the
redaction-safe `sensitive` or `unknown` value states. The checkpoint accessor
reports only that a value is sensitive; it does not expose its receipt.

## Consequences

- Stable sensitive configuration converges even though Dokploy cannot return
  the current value.
- Key rotation changes receipt identity and deliberately plans a write.
- Remote mutation outside the tool cannot be detected for write-only values;
  only conclusive absence is reportable drift.
- The offline compiler continues to reject concrete sensitive input with
  `DOKCMP004`. ADR 0013 adds an instance-bound compiler that resolves values
  once and returns a redacted one-shot execution sidecar.
