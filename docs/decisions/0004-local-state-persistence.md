# ADR 0004: Local state persistence

## Status

Accepted for the initial MVP.

## Context

Infrastructure state establishes resource identity and records the last
non-sensitive inputs managed by the CLI. A partial or stale write could lose a
remote identifier, reuse state against another Dokploy instance, or overwrite
newer state prepared by another process.

## Decision

Store state below `.dokploy/` and bind every `StateStore` to a canonical
workspace path and normalized Dokploy instance identity. A write session owns
one fail-fast `fs4` advisory lock for its lifetime. Every checkpoint re-reads
the primary state and compares its lineage and serial with the caller's
expected revision before changing any file.

State decoding rejects malformed documents, duplicate keys, unknown fields,
unsupported formats, nil lineages, invalid addresses, and resource-kind
mismatches. `StateFile::from_json_slice` is the supported decoding seam;
`StateFile` does not implement general-purpose deserialization.

State format version 3 stores each resource's direct containment separately
from its general dependency list. Projects require explicit null containment;
environments require a project address; applications, Postgres databases,
Redis databases, and domains require an environment address. Missing fields,
missing required containment, unexpected containment, and parent-kind
mismatches are rejected. Versions 1 and 2 are not migrated or interpreted by
inferring parents from dependencies.

For an existing state, persistence atomically replaces the backup with the
current primary bytes before atomically replacing the primary with the proposed
snapshot. Each replacement uses a temporary file in `.dokploy/`, writes a
newline-terminated document, flushes and synchronizes the file, persists it,
then synchronizes the directory where supported. Initial creation does not
create a backup. An orphaned backup or corrupt primary is preserved for
explicit recovery and never treated as absent state.

On Unix, `.dokploy/` uses mode `0700`, its artifacts use mode `0600`, and
directory identity is rechecked by device and inode while the writer lock is
held. A synchronization failure after persistence is reported as an unknown
durability outcome with the affected artifact and revision.

## Consequences

- Callers cannot omit instance, lineage, or serial checks from a durable write.
- Durable containment is explicit and cannot be reconstructed from execution
  ordering metadata.
- A stale or oversized proposal fails before changing the primary or backup.
- The backup and primary are separately atomic; their ordered replacement is
  not one cross-file transaction.
- Locks are advisory and protect cooperating processes only.
- Standard path-based filesystem calls retain a narrow replacement race.
  Closing it requires platform-specific handle-relative operations.
- Portable Rust does not provide equivalent parent-directory synchronization,
  device/inode checks, or Unix permission modes on every platform. CI must add
  platform-specific state tests as support expands.
