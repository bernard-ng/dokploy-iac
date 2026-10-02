# ADR 0048: Service server placement selectors

## Status

Accepted. Extends ADR 0045 from Application to Compose and the six database
kinds, on the SDK contract of ADR 0036.

## Context

ADR 0045 selects Dokploy servers by stable name and treats the primary server of
an application as create-only: `application.create` accepts `serverId`, no
`application.update` field changes it, so a changed placement replaces the
application. The pinned Dokploy `v0.30.6` OpenAPI document shows the same shape
for the remaining services:

| Kind       | Create accepts a nullable `serverId` | Update accepts `serverId` | Read exposes `serverId`        |
|------------|--------------------------------------|---------------------------|--------------------------------|
| Compose    | yes (optional)                       | no                        | `compose.one`                  |
| PostgreSQL | yes (optional)                       | no                        | `postgres.one`                 |
| MySQL      | yes (optional)                       | no                        | `mysql.one`                    |
| MariaDB    | yes (optional)                       | no                        | `mariadb.one`                  |
| MongoDB    | yes (optional)                       | no                        | `mongo.one`                    |
| LibSQL     | yes (nullable, declared required)    | no                        | `libsql.one`, `project.one`    |
| Redis      | yes (optional)                       | no                        | `redis.one`                    |

Every kind therefore supports the identical create-only contract, and the SDK of
ADR 0036 already models explicit local (`null`), external, and unmanaged
placement on all seven create inputs and the presence-aware `server_id` on all
seven reads. No kind is unsupported. The `*.move` endpoints move a service
between environments, never between servers.

Deleting a Compose or database in Dokploy removes the Mounts, Schedules, and
Backups that target it, and the planner does not cascade a replacement to them.

## Decision

### Configuration

Compose, PostgreSQL, MySQL, MariaDB, MongoDB, LibSQL, and Redis declare an
optional `server` selector with the grammar of ADR 0045: `{ local: true }` or
`{ name: "..." }`. Omission leaves placement unmanaged and the create omits
`serverId`; `null` is rejected; `local` is the only way to name Dokploy's own
host. Validation reuses `DOKCFG029` (invalid name), `DOKCFG030` (`local` outside
primary placement), and `DOKCFG031` (`server: null`), so this ADR adds no
configuration or remote diagnostic codes (`DOKCFG070`-`DOKCFG079` and
`DOKREM110`-`DOKREM119` stay unused). The canonical writer, typed import
documents, and generated schema model the same closed shape, and `server` can be
listed in `lifecycle.ignore_changes`.

### LibSQL create contract correction

`libsql.create` is the one create whose schema requires the `serverId` key
(nullable). ADR 0036 made placement optional on every create by omitting the key
when unmanaged, and the live acceptance of this ADR proved that Dokploy
`v0.30.6` rejects a LibSQL create body without it with a validation error. The
SDK now sends `serverId: null` for an unmanaged LibSQL placement, exactly as it
does for the explicit local selector. Dokploy stores a null server the same way
it stores the omitted field of the other kinds, and an unmanaged placement is
still never compared or proven, so planning and recovery are unchanged.

### Planner

`PropertyPath::Server` is valid for exactly these seven kinds in addition to
Application, with the same stable name-object values validated at the desired,
stored, and remote seams. The mutation contract of each kind allows `server` on
create and makes it a replacement property that cannot be cleared, so the planner
classifies a changed placement as `replace` with delete-before-create order and
`DOKPLAN019` blocks an unresolved selector. A protected service is never
replaced (`DOKPLAN` protected delete). Adopting a placement that already matches
the remote value is a state-only checkpoint, never a replacement.

### Observation and receipts

Discovery reads `server.all` once per run only when a desired, non-ignored
selector needs it. A direct `*.one` read maps the attached identity back to a name
selector through `ExternalDirectory::observe_association`; a duplicated name stays
visible and the desired selector reports the ambiguity, an identity missing from
the collection or an omitted `serverId` is an unknown observation that blocks
planning. Compose, MySQL, MariaDB, MongoDB, and LibSQL also read the full record
when a collection entry is found by name only. PostgreSQL and Redis collection
entries carry no placement, so a service that exists remotely but is not in
durable state observes no properties; this already made an uncertain PostgreSQL
or Redis create require an operator decision and still does. The saved-plan
receipt, the execution sidecar, and the redaction rules are unchanged from ADR
0045: resolved identities live only in non-serializable, Debug-redacted memory.

### Execution

A create sends the freshly resolved placement (explicit null for `local`, the
identity for a name) and nothing when unmanaged; no create deploys. Placement is
proven resolvable for every planned create and replacement before the first
remote mutation, so a stale selector never strands the workspace. A selector in
`ignore_changes` is neither resolved nor read, so a create or replacement that
would need it is refused with `ExternalResolutionMissing` before any mutation
rather than placed by guess.

A changed placement replaces the service in two journaled, recoverable steps:
the old identity is deleted and checkpointed, then the replacement is created on
the selected server and recorded only after the create response proves it. The
delete uses each kind's existing delete call; Compose deletion always preserves
volumes. All inputs, including secrets, are built before the destructive step.
LibSQL node replacement shares this path.

Replacement is refused, before any mutation, while durable state contains a
resource contained by or depending on the service. The rule is the one ADR 0045
applies to Application and covers the dependency every Mount, Schedule, and
Backup adapter records on its target, so replacing a service never silently
orphans them. This also now protects LibSQL replacements caused by a node
change, which previously had no such check.

### Recovery

Recovery is generic. An uncertain create is adopted only when the observed
placement equals the selector recorded in the journal; a contradicting
placement, a selector that no longer resolves to exactly one record (removed,
renamed, duplicated, re-created, or unreadable), or a missing observation
requires manual intervention. An interrupted replacement recovers its delete step
from authoritative absence and its create step by adoption, and is never retried
by recovery.

### Import

Import writes a name selector into the generated configuration and durable
state for a service attached to a server, reads `server.all` only then, and fails
closed with the existing association diagnostic when the identity is unknown, the
collection is unreadable, the name is shared, or the name violates the selector
grammar. A `null` or omitted placement stays unmanaged. Imported services remain
protected, so the first plan converges without mutation and a later placement
change must unprotect the service first. Mount, Schedule, and Backup imports that
adopt a target service use the same builders.

### Live acceptance

`scripts/integration/test-server-placement-apply.sh` runs against the digest-pinned
`v0.30.6` instance with inert server records that have no SSH key and point at a
connection tripwire. For every kind it creates one service on a named server, one
on the local host, and one unmanaged, proves no-op convergence, replacement in
every direction (named to named, local to named, named to local) with proof that
the old identities are gone and unmanaged services are untouched, blocking of
unmatched and ambiguous names, a saved plan that applies while unchanged and is
refused after the selected record is re-created under the same name, protected
import writing name selectors, and fail-closed ambiguous import. Every service
must stay idle with zero deployments, the tripwire must record no connection,
cleanup is scoped to the run prefix and proven by absence, and retained evidence
is scanned for the API key, fingerprint key, secret canaries, and external
identities.

## Consequences

- Compose and every database kind are placed by stable name with the same
  create-only semantics, receipt binding, and fail-closed blocking as Application.
- A saved plan applies only while the selected server still resolves to the same
  external identity.
- Changing a placement deletes and re-creates the service; the data of a database
  is not migrated, so operators should treat a placement change as destructive.
  Protection and the dependent check are the guard rails.
- Replacement stays unavailable while Mounts, Schedules, Backups, Domains, Ports,
  or other dependents are recorded for the service, until a cascading replacement
  is designed.
- PostgreSQL and Redis uncertain creates keep requiring an operator decision
  because their collection entries do not prove properties.
- A create whose `server` selector is ignored cannot be placed and is refused
  before any mutation.
- The live acceptance exposed a Dokploy `v0.30.6` race unrelated to placement:
  services created concurrently in one environment can be missing from the
  `*.search` collections even though their direct reads succeed. The CLI reports
  the disagreement as conflicting topology (for example `DOKREM021`) and blocks
  planning instead of guessing, and the acceptance applies serially with
  `--parallelism 1`. The default parallelism of four database creates can
  trigger it against a real instance; this ADR neither changes the default nor
  works around the upstream behavior.
