# ADR 0012: Per-instance sensitive fingerprint keys

## Status

Accepted for the Phase 5 sensitive-intent foundation.

## Context

Durable state contains HMAC-SHA-256 receipts for sensitive configuration
intent. Computing those receipts requires a stable secret key, but the key
must not enter configuration, state, plans, journals, logs, or ordinary files.
The same Dokploy instance can be selected through different CLI contexts, so a
context name is not a stable key identity.

## Decision

The CLI owns one deep sensitive-fingerprinting module. Its crate-private
interface loads a fingerprinter for a normalized `InstanceIdentity` and
returns opaque `SensitiveFingerprint` receipts. It exposes neither raw key
bytes nor MAC bytes.

Each Dokploy instance has one random 32-byte HMAC key in the operating-system
credential store. The entry uses the separate service
`dokploy-sensitive-intent`. Its account name contains the fingerprint format
and a lowercase SHA-256 digest of the normalized instance identity, never the
instance URL itself. This avoids collisions with stored Dokploy API keys and
keeps account names bounded across credential-store implementations. Linux
and BSD Secret Service sessions use the backend's encrypted Rust-crypto
transport rather than sending credential contents in plain session messages.

The binary credential value has one strict representation: the eight-byte
`DOKHMAC1` marker, a canonical non-nil 16-byte UUID key identifier, and the
32-byte HMAC key. Any other length, marker, or identifier is malformed. The
key and transient envelope buffers are zeroized when dropped.

Headless automation may instead set `DOKPLOY_FINGERPRINT_KEY` explicitly. Its
strict value is `<non-nil UUID>:<64 lowercase hexadecimal characters>`. This
source takes precedence over the credential store and never writes to it. A
malformed value fails closed without being echoed. CI must keep the value in a
masked secret and reuse it with the same protected local state; changing its
UUID or key bytes intentionally rotates sensitive intent.

Without that explicit source, the CLI obtains 48 bytes from the
operating-system random source when an entry is absent, constructs a
version-four UUID and a 32-byte key, stores the envelope, and immediately reads
it back. Fingerprinting uses only the value read back from the store. A
malformed entry, unavailable credential store, failed write, missing readback,
or unavailable entropy fails closed. There is no implicit filesystem or state
file fallback.

Receipts use HMAC-SHA-256 with an unambiguous length-prefixed message. The
message includes a fixed versioned domain plus the normalized instance,
canonical resource address, canonical sensitive property path, and exact
value bytes. Equal values at different instances, resources, or properties
therefore have unrelated receipts. The receipt also includes the key
identifier, so key rotation is an intentional configuration change even in
the theoretical case where two keys produce the same MAC.

The instance-bound compiler now consumes this module through the design in
[ADR 0013](0013-instance-bound-sensitive-compilation.md). The offline
`compile_desired` seam continues to reject concrete sensitive input with
`DOKCMP004` so offline validation never opens the credential store.

## Consequences

- Copying state to another machine requires either the same explicit CI key or
  the corresponding credential-store entry. A newly generated key causes
  configured sensitive values to be rewritten once resolution is available.
- Replacing the credential intentionally rotates every configured sensitive
  receipt for that instance.
- Locked or unavailable OS credential storage prevents sensitive planning;
  automation must supply the explicit key rather than relying on a fallback.
- Concurrent first use relies on immediate readback. A later competing writer
  can cause one additional future rewrite, but cannot produce a false no-op or
  disclose key material. Cross-process creation locking can be added if this
  becomes a practical source of churn.
- The normal test suite uses injected in-memory stores and deterministic key
  generators, so it does not open or modify a developer's credential store.
