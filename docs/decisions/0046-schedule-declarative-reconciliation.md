# ADR 0046: Schedule declarative reconciliation

## Status

Accepted. Implements the Schedule model from ADR 0026 on the SDK contract of
ADR 0033.

## Context

Dokploy `v0.30.6` runs a Schedule's executable command (and optional script)
on a cron expression against an application, a Compose service, a remote
server, or the Dokploy host. It exposes `schedule.create`, `schedule.one`,
`schedule.update`, `schedule.delete`, and the target-scoped `schedule.list`
collection. Reads return executable text, so the SDK reduces command and script
to presence flags and compares exact bytes only inside zeroizing private
proofs. Host and Dokploy-server scopes are rejected by the SDK and are not
modeled here, so no external selector is needed. Creating, updating, or
deleting a Schedule neither runs it nor deploys its target. A Schedule is
independent of any target deployment state.

A Schedule is meaningful only on one target, and its collision key is its name
within that target. For a Compose target the SDK's target identity includes the
service name, and its collection read rejects any record whose service name
differs from the requested one.

## Decision

### Configuration

Schedule is an environment-level collection,
`environments.<env>.schedules.<name>`, addressed `schedule.<name>`. It is
contained by its environment like a Mount. The target is an owned property and
inferred dependency, not containment. The Dokploy Schedule name is a separate
required property: the logical resource name never reaches Dokploy, which keeps
the target-scoped collision key explicit and makes duplicate detection a real
parse-time check.

```yaml
schedules:
  nightly:
    name: nightly                  # collision key within one target
    target: application.api        # application.<n> | compose.<n>
    cron_expression: "0 3 * * *"
    shell_type: bash               # bash | sh
    enabled: false                 # required: never defaulted
    description: Nightly cleanup   # optional
    timezone: UTC                  # optional
    command:                       # secret descriptor only
      file: secrets/nightly-command
    script:                        # optional secret descriptor only
      env: NIGHTLY_SCRIPT
  worker-job:
    name: worker-job
    target: compose.stack
    service_name: worker           # required exactly for Compose targets
    cron_expression: "@daily"
    shell_type: sh
    enabled: false
    command: { env: WORKER_COMMAND }
```

`enabled` is required rather than defaulted so a Schedule can never begin to run
because a key was omitted. The target is the closed union
`ResourceKind::is_schedule_target` (application, Compose); there are no raw
type strings, and server and Dokploy-server scopes are unrepresentable.
`command` and `script` are `env` or `file` descriptors only. They are
fingerprinted as opaque sensitive intent, held only in the redacted one-shot
execution sidecar, and never appear in configuration literals, state, plans,
journals, diagnostics, or debug output.

Parsing rejects with typed codes:

- DOKCFG050: a target outside the closed union, or a `service_name` that is
  missing for Compose or present for an application (the target must also exist
  in the same environment, using the existing missing and cross-environment
  codes);
- DOKCFG051: an invalid name, cron expression, service name, timezone, or
  description (all bounded, control-free, and lexical only: the cron check never
  interprets scheduling semantics);
- DOKCFG052: two Schedules sharing a target, service name, and name;
- DOKCFG053: `command: null` or `script: null`, because executable text cannot
  be cleared;
- DOKCFG054: an omitted (unmanaged) command without `lifecycle.protect: true`;
- DOKCFG055: Schedules on one Compose naming different services, because the
  SDK's target-scoped collection cannot describe more than one service of one
  Compose at a time;
- DOKCFG056: `description: null` or `timezone: null`, because Dokploy has no
  proven clear for them.

Only `name`, `cron_expression`, `shell_type`, `enabled`, `description`, and
`timezone` may be ignored. `target` and `service_name` are replacement keys and
`command` and `script` are sensitive, so none of them can be ignored.

An omitted `script` is accepted without protection. For application and Compose
Schedules the script is optional and normally absent, so requiring protection
would make the usual configuration impossible. It means that the configuration
does not manage a script: create sends none, and any update that would have to
resend or drop a script that exists remotely is refused (see Execution).

### Planner and state

Property paths are `target`, `service_name`, `name`, `cron_expression`,
`shell_type`, `enabled`, `description`, `timezone`, and the sensitive `command`
and `script`. The adapter contract requires `target`, `name`,
`cron_expression`, `shell_type`, `enabled`, and `command` on create, and allows
`service_name`, `description`, `timezone`, and `script`. `target` and
`service_name` use replace mode, so a target or Compose-service change plans
delete-before-create replacement. Name, cron, shell, and enabled update in
place. Description, timezone, command, and script are set-only. Containment is
state-only. Replacement of a protected Schedule is blocked.

Values are validated independently at the desired, stored, and remote snapshot
seams: targets must parse into the closed union, the shell is closed, `enabled`
is a boolean, strings are nonempty, bounded, trimmed, and control-free, and
command and script may only be opaque receipts (never null or a value). They are
stored only as sensitive receipts; managed state rejects `command` and `script`
keys outright (including nested ones).

Stored dependencies carry the target, so a Schedule is created after and deleted
before its target, including when both are removed together.

### Discovery

Discovery reads, for every target and Compose service that holds a desired or
stored Schedule, the exact target's complete `schedule.list` collection, and a
managed Schedule also through `schedule.one`. The direct record must agree
exactly with the collection and with the stored typed target (the SDK already
requires direct and collection agreement). Absence is proven only by an
authoritative collection that does not contain the identity (a direct 404 alone
is never sufficient); partial authority and failed collection reads downgrade
absence to an unavailable observation. Duplicate identities, duplicate
target-scoped names, privileged (server or Dokploy-server) records, a direct
record on a different target, and any rename or target change that would land on
another Schedule fail closed. An unmanaged Schedule at the collision key is
observed and blocks as an address collision; it is never adopted by planning.

The command is always reported as an unreadable sensitive observation. The
script is reported unreadable when the remote has one and as known-absent when
it does not, so a script that disappeared out of band is drift that plans an
update, while a script the configuration does not manage is never compared.

### Execution

Create, update, and replacement build and validate their complete SDK input
before any journal step opens, so a pre-mutation rejection leaves no open step.
Command and script move from the sensitive sidecar into zeroizing inputs, and an
empty command is rejected before the journal. Update performs a fresh
`schedule.one` read, verifies identity and target, and sends one complete
replacement: every safe field comes from the checkpoint, except an ignored or
unmanaged field, which is preserved from the fresh read, and the executable text
can only come from the sidecar. Therefore:

- a Schedule whose command is unmanaged (the protected import shape) can be
  observed but never updated, because the command cannot be resent; such an
  update is refused as unsupported before any mutation;
- a Schedule whose remote script exists but is not managed cannot be updated,
  because the script cannot be resent and omitting it would silently drop it.

Delete removes the exact identity through the exact stored typed target and
treats an already-missing identity as convergent. Replacement deletes,
checkpoints the removal, journals a second create step with a recovery
placeholder, and records the new identity only after the SDK proves the response.

A definitive pre-mutation rejection fails the step. A transport, decode, or
post-acceptance uncertainty stays in progress and is never retried. The SDK
reports a name collision found by its own create preflight as unproven, so such
a create also stays in progress and is never adopted. No Schedule operation runs
a Schedule or deploys anything.

### Recovery

An uncertain create is adopted only from one exact collision-key match in the
authoritative target collection whose observable properties equal the proposed
state; command and script bytes cannot be compared and are excluded, but a
script that is proposed and missing remotely is a difference. A different
Schedule at the key requires manual intervention. An uncertain update is
confirmed only from authoritative complete state, and an uncertain command or
script rotation, whose bytes are write-only, always requires manual
intervention. Because executable text can never be compared, an uncertain update
that still looks unapplied is not assumed unapplied either; it also needs an
operator. An uncertain delete is confirmed only by authoritative absence.

### Import

`dokploy import schedule <id> --as <address>` reads the Schedule, requires direct
and target-collection agreement, and imports the typed target (application or
Compose service), its environment, and the project into one new workspace so the
Schedule's containment, service name, and target dependency are written
correctly. The Schedule is imported protected and its command and script are
left unmanaged. Description and timezone are owned only when the remote returns
them, so the first fresh plan converges for both present and absent values.
Schedules are not offered by the interactive picker because listing them would
need one collection read per target; the explicit ID form is the supported path.
A remote name or cron expression outside the configuration grammar fails the
import closed instead of being written.

### Live acceptance

The digest-pinned Dokploy `v0.30.6` acceptance proves undeployed, disabled
creation on an application and a Compose service with descriptor-supplied
executable text, no-op convergence, in-place cron, description, and timezone
edits with command and script rotation under stable identities (exact remote
bytes verified privately), target replacement with identity absence, declarative
deletion with identity and collection absence, protected import of an
out-of-band Schedule, zero deployments on every touched service, zero
deployments on every Schedule (proving none executed), and identity-scoped
cleanup. Generated executable-text canaries and the API key are scanned across
all retained evidence.

## Consequences

- Schedules on applications and Compose services can be created, observed,
  updated, replaced, imported, recovered, and deleted declaratively without ever
  running one.
- Two Schedules cannot silently collide on one target and name.
- Executable text never leaves the descriptor and mutation seams; a rotation
  that cannot be proven applied is never guessed.
- Known limitation: the SDK reads one Compose service collection per target, so
  every Schedule on one Compose must use one service name. Moving a Schedule to
  a different service of the same Compose cannot be observed while Schedules for
  the old service exist, and is blocked until the operator removes and re-adds
  it across two applies.
- Known limitation: an imported (command-unmanaged) Schedule is observe-only; to
  change it the operator must take ownership of its command by declaring one.
- Known limitation: description, timezone, command, and script cannot be
  cleared declaratively, and a script added out of band to a Schedule whose
  configuration omits a script is not detected until an update is refused.
- Known limitation: a declarative move of a Schedule's own address is not planned
  as an in-place move, matching Domain, Port, and Mount.
- The configuration and discovery codes DOKCFG050-056 and DOKREM090-094 are
  reserved for Schedule and deliberately separate from the Port, Redirect,
  Security, and Mount ranges.
