# ADR 0043: Security declarative reconciliation

## Status

Accepted.

## Context

Dokploy `v0.30.6` exposes application basic-auth entries through
`security.create`, `security.one`, `security.update`, and `security.delete`, and
lists them through the `security` collection of `application.one` (ADR 0031).
Create returns only a boolean. Direct and collection reads return the plaintext
password, update requires a complete username and password, and the API has no
password-clear or password-unchanged operation. A missing entry is reported by
`security.one` as HTTP 404.

A username is the collision key within one application. The password is a
write-only secret.

## Decision

Security is an application-contained declarative resource at
`environments.<env>.applications.<app>.security.<name>`. It owns a required
`username` and an optional `password`. The password is a descriptor only
(`env` or `file`), is fingerprinted as opaque sensitive intent exactly like the
database passwords, and is never a literal in configuration, state, plans,
journals, diagnostics, or Debug output. Leaving the password out leaves it
unmanaged. `password: null` is rejected (`DOKCFG027`) because Dokploy cannot
clear a password, and the planner seams reject a null Security password and a
null or empty username in desired, stored, and remote data. The password is
never listed in `ignore_changes`; the username may be.

Configuration rejects, at parse time (`DOKCFG028`), two entries within one
application that share a username.

Fresh discovery reads the `application.one` Security collection once for every
application that contains a desired, stored, or removed entry, then reads each
managed identity directly and requires exact agreement, with the same duplicate,
containment, collision, and 404-absence rules as Redirect (ADR 0042). Plaintext
passwords are consumed by the SDK decoder and reduced to presence. A present
remote password is observed only as unknown-sensitive and an absent one as
absent, so remote bytes never reach a plan, a receipt comparison, or state.

Create requires a declared password descriptor, resolves it once, and sends the
username and password with the containing application; it never deploys. Update
sends the complete username and password every time, because Dokploy cannot
update one without the other. The password therefore must be available for any
update: a username-only change with an unmanaged password fails closed before
any remote mutation or journal step. The update performs a fresh direct read,
verifies identity and containment, and rejects a username collision before the
journal step starts. The SDK's own post-update proof compares the exact password
privately. Changing the containing application is delete-before-create
replacement with a recovery placeholder.

An uncertain create is adopted only from one exact username match in the
authoritative parent collection. The remote password cannot be proven from
write-only data, so adoption is by exact collision key and equal non-sensitive
fields; the declared password is re-applied by the next plan only if its receipt
differs. An uncertain update that did not change the password receipt is
confirmed from authoritative state like any other. An uncertain update that
rotates the password has no proof available and requires manual intervention,
the same rule as other write-only secrets.

Protected import (`dokploy import security <id> --as security.<name>`) reads one
exact identity and its authoritative collection, rejects a duplicated username,
and writes the username only. The password stays unmanaged, the state carries no
sensitive receipt, and the first fresh plan converges.

The live acceptance mirrors Redirect and additionally scans every retained
artifact, the journal, state, and plan output for the generated password
canaries. A sanitization self-test proves the scanner detects a seeded canary and
the integration API key.

## Consequences

- Basic-auth entries are managed without plaintext passwords in any durable
  artifact.
- A password can be changed only by declaring its descriptor; an imported entry
  must have its password declared before it can be renamed or rotated.
- Password rotation confirmation after an uncertain mutation is manual.
- Username collisions are rejected in configuration and on the server before
  mutation.
