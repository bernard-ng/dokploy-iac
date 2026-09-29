# ADR 0013: Instance-bound sensitive compilation

## Status

Accepted for the Phase 5 planner composition layer.

## Context

The offline configuration compiler deliberately rejected every concrete
sensitive value. Convergent planning now has durable HMAC receipts and a
per-instance key, but compilation must still resolve each configured value
without leaking raw bytes, weakening offline behavior, or allowing an
unsupported resource reference to trigger partial external access.

The future executor needs the exact bytes that produced each receipt. Re-reading
an environment variable or file during execution would allow the desired
receipt and written value to disagree.

## Decision

The CLI exposes two compiler seams:

- `compile_desired` remains offline. Concrete sensitive values fail with
  `DOKCMP004`; clear and unmanaged fields compile without credential or source
  access.
- `compile_desired_for_instance` accepts a normalized `InstanceIdentity` and
  workspace directory. It returns the same pure desired model plus a redacted,
  non-cloneable, non-serializable execution sidecar.

Instance-bound compilation first scans the complete validated resource model.
Any application environment resource reference fails with `DOKCMP004` before
the credential store, process environment, or filesystem is accessed. When no
concrete sensitive input exists, compilation delegates to the offline seam and
preserves the source digest exactly.

When concrete inputs exist, the compiler loads the per-instance fingerprint key
once and resolves inputs once in canonical resource-and-property order.
Literal text is copied directly into a zeroizing byte buffer. Environment
variables are read once and preserved as their exact UTF-8 bytes without
trimming. Missing and non-UTF-8 environment values have stable redacted
diagnostics.

File descriptors are relative to the supplied workspace. Paths are bounded and
must contain only ordinary relative segments. On Unix, the workspace and every
intermediate directory are opened handle-relative with no-follow and directory
flags. The final entry is opened no-follow and nonblocking, verified through
the opened descriptor as a regular file, and read through that descriptor with
an inclusive one-MiB bound. This rejects traversal, intermediate and final
symlinks, directories, devices, sockets, and FIFOs without a path-check/read
race. Growth during reading remains bounded. Non-Unix targets fail file sources
closed until an equivalent native handle-relative adapter is implemented.
File contents must be UTF-8 and are never trimmed.

Each resolved buffer is passed to the per-instance fingerprinter exactly once.
The resulting `SensitiveFingerprint` becomes the desired
`OwnedValue::Sensitive` and is paired with the same byte buffer in the
execution sidecar. Taking a sidecar entry transfers ownership and removes it,
so callers cannot consume the same resolved value twice.

When sensitive receipts exist, the effective `ConfigDigest` hashes a versioned
domain, the caller-supplied source digest, and canonical address, property, and
receipt-identity digests. Raw values and broadly accessible MAC getters are not
part of this interface. Source-descriptor changes therefore remain visible
through the source digest, while value or fingerprint-key rotation changes both
the receipt and effective digest.

Credential, environment, file-safety, size, and encoding failures map to stable
`DOKCMP005` through `DOKCMP010` diagnostics that contain no source names,
paths, values, key identifiers, or MACs.

## Consequences

- Planning and later execution share one exact resolved value and receipt.
- Stable intent converges; content and key rotation schedule one write.
- Clear and unmanaged sensitive fields retain their offline behavior.
- Unsupported resource references cannot cause partial key or source access.
- File secrets are currently available only on Unix. Other targets reject them
  rather than falling back to racy path-based reads.
- The checkpoint adds no executor or Dokploy mutation path.
