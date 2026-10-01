# ADR 0044: Mount declarative reconciliation

## Status

Accepted. Implements the Mount model from ADR 0026 on the SDK contract of
ADR 0027.

## Context

Dokploy `v0.30.6` attaches a Mount to one of eight service kinds through
`mounts.create`, reads it through `mounts.one` and the unpaginated
`mounts.listByServiceId`, and changes or removes it through `mounts.update` and
`mounts.remove`. A missing Mount is HTTP 404. File Mount content is returned by
the server but is write-only for this project: it can contain credentials and is
never modeled by the SDK. Creating, updating, or removing a Mount does not
deploy the target.

A Mount is meaningful only on one target, and its collision key is the mount
path within that target, not its logical name or source.

## Decision

### Configuration

Mount is an environment-level collection, `environments.<env>.mounts.<name>`,
addressed `mount.<name>`. It is contained by its environment like a Domain. The
target is an owned property and inferred dependency, not containment.

```yaml
mounts:
  data:
    target: application.api          # application|compose|postgres|mysql|mariadb|mongo|libsql|redis
    mount_path: /data
    source:
      type: volume                   # bind | volume | file
      volume_name: api-data
```

The target is a logical address restricted to the closed union
`ResourceKind::is_mount_target`; there are no raw service-type strings. The
source is a tagged union: `bind` with `host_path`, `volume` with `volume_name`,
and `file` with `file_path` and `content`. File `content` is a secret
descriptor (`env` or `file`) only. It is fingerprinted as opaque sensitive
intent, held only in the redacted one-shot execution sidecar, and never appears
in configuration literals, state, plans, journals, diagnostics, or debug output.

Parsing rejects with typed codes:

- DOKCFG040: a target outside the closed union (the target must also exist in
  the same environment, using the existing missing and cross-environment
  codes);
- DOKCFG041: unsafe values, namely a relative or root `mount_path` or bind
  `host_path`, control characters, an invalid volume name, or an absolute,
  traversing, backslashed, or empty-segment `file_path`;
- DOKCFG042: two Mounts sharing a target and `mount_path`;
- DOKCFG043: `content: null`, because file content cannot be cleared;
- DOKCFG044: omitted (unmanaged) file content without `lifecycle.protect: true`.

Only `mount_path`, `host_path`, `volume_name`, and `file_path` may be ignored.

### Planner and state

Property paths are `target`, `mount_type`, `mount_path`, `host_path`,
`volume_name`, `file_path`, and the sensitive `content`. The adapter contract
requires `target`, `mount_type`, and `mount_path` on create. `target` and
`mount_type` use replace mode, so a target or storage-type change plans
delete-before-create replacement. Path and source edits update in place.
Containment is state-only. Replacement of a protected Mount is blocked.

Values are validated independently at the desired, stored, and remote snapshot
seams: targets must parse into the closed union, types are closed, strings are
nonempty, bounded, and control-free, and file content may only be an opaque
receipt (never null or a value). Content is stored only as a sensitive receipt;
managed state rejects a `content` key outright.

Stored dependencies carry the target, so a Mount is created after and deleted
before its target, including when both are removed together.

### Discovery

Discovery reads, for every target that holds a desired or stored Mount, the
exact target's complete `mounts.listByServiceId` collection, and a managed
Mount also through `mounts.one`. The direct record must agree exactly with the
collection and with the stored target identity. Absence is proven only by an
authoritative collection that does not contain the identity (a direct 404 alone
is never sufficient); partial authority and failed collection reads downgrade
absence to an unavailable observation. Duplicate identities, duplicate
target-scoped mount paths, a direct record on a different target, and any path
or target change that would land on another Mount fail closed. An unmanaged
Mount at the collision key is observed and blocks as an address collision; it
is never adopted by planning.

When the desired target resolves to the same physical service as the stored one
(for example after a declarative target move), the remote already satisfies the
desired target and only state is checkpointed.

### Execution

Create, update, and replacement build and validate their complete SDK input
before any journal step opens, so a pre-mutation rejection leaves no open step.
Update performs a fresh `mounts.one` read, verifies identity, target, and type,
and sends one complete replacement of the path and source. A file Mount with
unmanaged content is never created as an empty file, and its `file_path` is never
changed, because the content cannot be resent. Delete removes the exact identity
and treats an already-missing identity as convergent. Replacement deletes,
checkpoints the removal, journals a second create step with a recovery
placeholder, and records the new identity only after the SDK proves the response.

A definitive pre-mutation rejection fails the step. A transport, decode, or
post-acceptance uncertainty stays in progress and is never retried. No Mount
operation deploys anything.

### Recovery

An uncertain create is adopted only from one exact collision-key match in the
authoritative target collection whose observable properties equal the proposed
state; file content cannot be compared and is excluded. A different Mount at the
key requires manual intervention. An uncertain update is confirmed only from
authoritative complete state, and an uncertain content rotation, whose bytes
are write-only, always requires manual intervention. An uncertain delete is
confirmed only by authoritative absence.

### Import

`dokploy import mount <id> --as <address>` reads the Mount, requires direct and
target-collection agreement, and imports the typed target (all eight kinds), its
environment, and the project into one new workspace so the Mount's containment
and target dependency are written correctly. The Mount is imported protected,
its file content is left unmanaged, and the first fresh plan converges. Mounts
are not offered by the interactive picker because listing them would need one
collection read per target; the explicit ID form is the supported path.

### Live acceptance

The digest-pinned Dokploy `v0.30.6` acceptance proves undeployed creation on an
application and a Compose target including file content, no-op convergence, an
in-place path edit with file content rotation under stable identities, target
and storage-type replacement with identity absence, declarative deletion with
identity and collection absence, protected import of an out-of-band Mount, zero
deployments on every touched service, and identity-scoped cleanup. Generated
content canaries and the API key are scanned across all retained evidence.

## Consequences

- Mounts on applications, Compose services, and databases can be created,
  observed, updated, replaced, imported, recovered, and deleted declaratively.
- Two Mounts cannot silently collide on one target and path.
- File content never leaves the descriptor and mutation seams; a rotation that
  cannot be proven applied is never guessed.
- Known limitation: replacing a target (for example a LibSQL node replacement)
  while its Mount remains desired can leave the Mount dangling on the server,
  because Dokploy removes or orphans it with the old service. The next plan
  observes the Mount as missing under the new target identity and recreates it;
  the replacement itself does not reorder or re-plan dependents.
- Known limitation: a declarative move of a Mount's own address is not planned as
  an in-place move, matching Domain and Port, because the unmanaged-address
  lookup observes the existing Mount at the destination address.
- The configuration and discovery codes DOKCFG040-044 and DOKREM080-084 are
  reserved for Mount and deliberately separate from the Port, Redirect, and
  Security ranges.
