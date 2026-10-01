# ADR 0039: Secret-safe database Backup SDK contract

## Status

Accepted. Extends the Phase 8 target and selector boundaries in ADR 0026.

## Context

Dokploy `v0.30.6` exposes database backup policies through `backup.create`,
`backup.one`, `backup.update`, and `backup.remove`. A policy targets one
PostgreSQL, MySQL, MariaDB, MongoDB, or LibSQL record and links to an external
backup destination. Compose and web-server backups use different metadata and
privilege boundaries.

The direct response embeds nested target and destination records. Those
relations can contain database passwords, destination access keys, deployment
errors, and Compose metadata. The generated API contract does not describe a
safe public projection. Create also returns no physical identity, so accepting
the response does not prove which record was created.

Each supported database `*.one` response embeds its complete `backups`
relation. That relation is the authoritative bounded collection for one exact
target. Dokploy normalizes collision prefixes by trimming surrounding
whitespace and slashes, then appending one trailing slash to a nonempty value.

## Decision

The handwritten SDK owns `BackupId`, `BackupTarget`, complete create and update
inputs, safe `BackupDetails`, and `BackupCollection`. `BackupTarget` is a closed
union of PostgreSQL, MySQL, MariaDB, MongoDB, and LibSQL identities. Compose and
web-server targets are rejected. Target changes are not serialized by update
and remain replacement work for the later declarative adapter.

Public responses retain only physical identity, target identity, destination
identity, schedule, enabled state, prefix, database, retention count, and the
encryption-key flag. Nested target, destination, deployment, user, and metadata
relations are ignored without retention. Backup mutations use the secret-aware
transport path so a remote rejection cannot echo credential-bearing nested
data into an error. The shared 16 MiB zeroizing response bound still applies.

Every read of a supported target dispatches to its exact `*.one` operation and
requires the parent identity to match. Collections accept at most 10,000 rows
and reject missing relations, contradictory targets, duplicate identities, or
duplicate collision tuples. The collision tuple is target, destination,
runtime-normalized prefix, database, and service; database targets have no
service value. A destination identity must also exist in the safe bounded
`destination.all` selector collection before mutation.

Create reads the authoritative target collection and rejects a collision. It
attempts `backup.create` once, then compares the preflight and postflight
identity sets. Every prior identity must remain and exactly one new row must
match the complete requested state. The new identity is adopted only from that
proof. Update first requires `backup.one` to agree with the authoritative
target collection, rejects a desired collision excluding itself, sends every
mutable field, and requires direct and collection agreement afterward. Remove
requires the caller's expected target, proves direct and collection agreement,
attempts `backup.remove` once, and requires authoritative collection absence.

Any transport interruption, malformed successful response, or failed proof
after Dokploy accepts a mutation is outcome-unknown and is never retried.
Validation and preflight failures remain definitive.

The live contract uses an undeployed PostgreSQL target and a disabled Backup.
It creates two inert destinations that point to a loopback tripwire without
calling `destination.testConnection`, exercises destination replacement and
all other mutable fields, removes the Backup, and proves authoritative absence.
The target remains idle with no deployments, and the tripwire proves that no
backup execution or destination connection occurred. Publication replaces the
tracked fixture tree only after a complete candidate passes the fixture,
API-key, and credential-canary checks.

## Consequences

- Database Backup reads and mutations have one typed, bounded public boundary.
- Nested database credentials, destination keys, deployment errors, and
  metadata do not enter public models, debug output, errors, or fixtures.
- Create cannot adopt a reused, ambiguous, or concurrently introduced
  identity.
- Update and remove cannot cross a caller-provided target boundary.
- External destinations are referenced and validated but not owned by the
  Backup adapter.
- Compose and web-server Backup support remains unavailable until their
  metadata and privilege contracts are designed separately.
- Uncertain mutations require recovery instead of automatic retry.
