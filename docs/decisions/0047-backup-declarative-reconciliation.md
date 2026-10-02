# ADR 0047: Backup declarative reconciliation

## Status

Accepted. Implements the Backup model from ADR 0026 on the SDK contract of
ADR 0039 and the external selector boundary of ADR 0045.

## Context

Dokploy `v0.30.6` attaches a Backup policy to one database through
`backup.create`, reads it through `backup.one` and the target's embedded
`backups` relation, and changes or removes it through `backup.update` and
`backup.remove`. `backup.create` returns no identity, `backup.update` replaces
every mutable field and never the target, and a missing Backup is HTTP 404. The
embedded records can contain database passwords and destination credentials, so
the SDK keeps only a safe projection. The collision key is the target, the
destination, the runtime-normalized prefix, and the database.

A Backup references an external destination. The workspace selects it by name
but never owns its lifecycle, and its physical identity must not become durable
desired state. Creating, updating, or removing a Backup policy never runs a
backup, tests the destination, or deploys the database.

## Decision

### Configuration

Backup is an environment-level collection, `environments.<env>.backups`,
addressed `backup.<name>`. It is contained by its environment like a Mount. The
target is an owned property and inferred dependency, not containment.

```yaml
backups:
  nightly:
    target: postgres.main          # postgres|mysql|mariadb|mongo|libsql
    destination: { name: offsite } # external, selected by exact name
    schedule: "0 3 * * *"
    prefix: nightly
    database: app
    enabled: false                 # default true
    keep_latest: 7                 # optional; null keeps every backup
    include_encryption_key: false  # default false
```

The target is restricted to `ResourceKind::is_backup_target`, the closed union of
the five SDK database kinds; Compose, Redis, and application targets are
rejected. The destination is a required name selector of kind
`SelectorKind::Destination`. `{ local: true }` is meaningless for a destination
and reuses DOKCFG030; names reuse DOKCFG029; `null` and every other shape are
location-only parse errors. Parsing rejects with typed codes:

- DOKCFG060: a target outside the closed union (the target must also exist in the
  same environment, using the existing missing and cross-environment codes);
- DOKCFG061: an invalid schedule, prefix, database, or a zero retention count;
- DOKCFG062: two Backups sharing the target, destination name,
  runtime-normalized prefix, and database.

The schedule is a bounded five- or six-field cron expression of single-space
separated fields over `[A-Za-z0-9*/,?#-]`; macros such as `@daily` are rejected
so every accepted value compares textually with Dokploy's stored value. The
prefix and database are nonempty, bounded, and control-free. Only `schedule`,
`enabled`, `keep_latest`, and `include_encryption_key` may be ignored: the
target and destination are replacement or association properties, and the prefix
and database belong to the collision key, which an ignored value could hide.

### Planner and state

Property paths are `target`, `destination` (an external selector), `schedule`,
`prefix`, `database`, `enabled`, `keep_latest`, and `include_encryption_key`.
`enabled` is shared with Schedule. The adapter contract requires every path
except `keep_latest` on create. `target` is a replacement property, so a target
change plans delete-before-create replacement; every other property, including
the destination association and clearing the retention, updates in place because
`backup.update` carries them all. Replacement of a protected Backup is blocked.
Containment is state-only, and the stored dependency carries the target so a
Backup is created after and deleted before its target.

Values are validated independently at the desired, stored, and remote snapshot
seams: targets parse into the closed union, destinations are name selectors, and
the schedule grammar and retention bound are mirrored. A remote `enabled` or
retention of `null` is representable as absent, so such a Backup plans an
in-place update rather than blocking.

### Destination resolution and saved plans

The destination is wired exactly as ADR 0045 prescribes: `is_external_selector`,
a compiled selector binding, an observation through
`ExternalDirectory::observe_association`, and an executor input built from the
freshly resolved identity. Zero or multiple matches, an unreadable collection,
or a missing resolution block the Backup with `DOKPLAN019`, including for a
create. The keyed saved-plan receipt covers the resolved destination identity, so
a destination that is removed and re-created under the same name invalidates a
saved plan whose JSON is byte-identical. Neither the plan nor durable state holds
a destination name outside the selector or any identity.

### Discovery

Discovery reads, for every target that holds a desired or stored Backup, the
exact target's complete `backups` relation, and a managed Backup also through
`backup.one`; the SDK requires the direct record to equal the collection record.
Absence is proven only by an authoritative collection that does not contain the
identity together with a direct 404; partial authority and failed reads
downgrade absence to an unavailable observation, and a direct record that its
collection omits or contradicts is unavailable, never trusted. A target that is
itself missing or unresolved makes its Backups missing or unavailable without a
read. For a Backup that is not in state the exact collision key, with the
resolved destination, finds it; an unmanaged Backup at the key is observed and
blocks as an address collision and is never adopted by planning. Duplicate
identities across target collections (DOKREM103), duplicate keys (DOKREM102),
and an update or replacement whose new key would land on another Backup fail
closed. DOKREM100 and DOKREM101 cover unusable target and identity values and
DOKREM104 a direct and collection conflict. The attached destination is mapped
back through the fresh collection into a name selector; an unknown identity is an
unknown observation.

### Execution

Create, update, delete, and replacement build and validate their complete SDK
input, including the resolved destination and the typed target identity from
durable state, before any journal step opens. Update performs a fresh
identity-and-target-checked read and sends one complete replacement: managed
fields carry the checkpoint value while ignored or unmanaged fields (and an
ignored null flag, which is refused) carry the freshly read remote value, so an
update never overwrites what the workspace does not own. Delete resolves the
stored target and treats an already-missing identity as convergent. Replacement
deletes, checkpoints the removal, journals a second create step with a recovery
placeholder, and records the new identity only after the SDK proves it by the
before-and-after identity delta. A definitive rejection fails the step; a
transport, decode, or post-acceptance uncertainty stays in progress and is never
retried. Nothing triggers a backup run, a destination test, or a deployment.

### Recovery

Recovery rediscovers through the same code. An uncertain create is adopted only
from one exact collision-key match whose observable properties, including the
destination name, equal the proposed state; a different Backup at the key needs
manual intervention. An uncertain update is confirmed only from authoritative
complete state. An uncertain delete is confirmed only by authoritative absence.
Any step whose destination no longer resolves to exactly one record requires
manual intervention, because the selected record cannot be proven.

### Import

`dokploy import backup <id> --as <address>` reads the Backup, requires direct
and target-collection agreement, and imports the typed target, its environment,
and the project into one new workspace so containment and the target dependency
are written correctly. The destination is written as a name selector and fails
closed when its identity is unknown, its collection is unreadable, its name is
shared, or its name is outside the selector grammar. A null `enabled` flag cannot
be written as a boolean and fails closed. The Backup is imported protected, a
null retention is written as `keep_latest: null`, and the first fresh plan
converges. Backups are not offered by the interactive picker.

### Live acceptance

The digest-pinned Dokploy `v0.30.6` run creates three tripwired destinations,
undeployed PostgreSQL and MySQL targets, and disabled Backups, then proves
creation, no-op convergence, in-place update of every mutable field and of the
destination under a stable identity, target replacement with absence of the old
identity, declarative deletion, saved-plan refusal after a destination is
re-created, unresolved-destination blocking, protected import, zero
deployments, zero Backup executions, zero destination contact, and
identity-scoped cleanup. Retained evidence is scanned for the API key, the
fingerprint key, database passwords, destination credentials, and every
destination identity. Backups stay disabled so Dokploy never schedules a run.

## Consequences

- Database Backups on all five SDK database kinds can be created, observed,
  updated, replaced, imported, recovered, and deleted declaratively, and cannot
  silently collide on one target and destination.
- A destination is always a stable name; a renamed, removed, duplicated, or
  re-created destination blocks planning or invalidates a saved plan.
- Compose and web-server Backups remain unsupported until their metadata and
  privilege boundaries are designed.
- Known limitation: replacing a LibSQL target while its Backup remains desired
  can leave the Backup dangling on the server, because Dokploy removes or
  orphans it with the old service; the next plan observes it as missing under
  the new target identity and recreates it.
- Known limitation: `enabled` defaults to `true`, which matches Dokploy but
  means a Backup that omits it will run on its schedule once applied; the live
  acceptance and every example that must stay inert set `enabled: false`.
- Known limitation: a declarative move of a Backup's own address is not planned
  as an in-place move, matching Domain, Port, and Mount.
- The configuration and discovery codes DOKCFG060-062 and DOKREM100-104 are
  reserved for Backup and separate from the Schedule ranges.
