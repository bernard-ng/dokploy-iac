# ADR 0001: Product goal, scope, and non-goals

## Status

Proposed.

## Context

The project began as "Docker Compose, but for Dokploy": describe a Dokploy
instance in YAML so that state lives in a repository instead of being re-created
through the dashboard. The first engine delivered a safe reconciliation core and
eleven resource kinds, but covered roughly a quarter of the project-side fields
and one of about twenty instance-settings kinds, dropped environment variables,
Compose documents, and Compose domains on import, never deployed anything, and
had no test proving that an exported instance can be rebuilt. See
[`docs/vision/`](../vision/) for the field-level map.

## Decision

**Goal.** A Dokploy instance, from its settings down to every project, service,
and setting a user can change in the dashboard, is described by YAML in a
repository. Applying that YAML to a fresh instance, after a short documented set
of manual steps, reproduces the instance. Exporting the result yields an
equivalent description. This is the **round-trip contract** (ADR 0011).

**In scope.**

- Every configurable field of every kind in the beta list (below), project side
  and instance side, including environment variables and file contents.
- Plan, apply, deploy, recover, destroy, import, state inspection, and drift
  reporting, with the safety invariants in `ARCHITECTURE.md`.
- Secrets handled without ever entering state, plans, or logs (ADR 0010).
- Dokploy versions on the supported list (ADR 0014).

**Beta list.** Project, environment; application, Compose, PostgreSQL, MySQL,
MariaDB, MongoDB, Redis, LibSQL; domain, port, redirect, security, mount,
schedule, backup, volume backup, patch; and for the instance: organization,
web server, SSH key, server, network, registry, git provider, certificate, DNS
provider, S3 destination, notification (all providers), tag, vault provider,
AI provider.

**Non-goals.**

- Identity and commercial features: users, invitations, roles, SSO, SCIM,
  licence, billing, whitelabeling, passkeys, sessions. (Revisit after beta.)
- Operational actions as state: start, stop, restart, rebuild, rollback, run a
  schedule now, clean queues, kill a build, container terminals, log reading.
- Installing Dokploy itself, creating the first administrator, creating the first
  API key, installing a GitHub App, and running server `setup`.
- Managing Docker objects Dokploy does not own.
- Supporting every historical Dokploy version.

**Manual steps that remain** (documented, never hidden): install Dokploy and
create the first administrator; create an API key; install any GitHub App
(an OAuth flow); provide secrets once through the chosen tier (ADR 0010); run
server `setup` for new hosts.

**Beta definition.** The beta is reached when the round-trip test of ADR 0015
passes against two supported Dokploy versions in CI, every beta-list kind has
import, plan, apply, recovery, and destroy with live acceptance, and every API
field of those kinds is classified (ADR 0002). Anything unsupported is listed by
the importer and never silently dropped.

**Compatibility policy (pre-release).** Nothing has been released. Until the beta
tag, the command line, the configuration format, the state format, and the Rust
APIs may break at any time without migration tooling or deprecation periods.
Breaking changes are recorded in `CHANGELOG.md`. This policy ends at the beta tag,
after which ADR 0014's compatibility rules and normal deprecation apply.

## Consequences

- Scope is defined by the kind list and the coverage ledger, not by an
  endpoint-by-endpoint wish list.
- Identity and commercial features are explicitly deferred; requests for them
  are answered by this ADR.
- The first engine's per-kind approach is replaced, not extended or kept alongside
  (ADR 0016).
