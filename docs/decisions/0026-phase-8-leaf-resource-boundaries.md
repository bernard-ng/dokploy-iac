# ADR 0026: Phase 8 leaf resources and external selectors

## Status

Accepted. Defines the shared model for the remaining Phase 8 adapters.

## Context

Dokploy `v0.30.6` exposes mounts, ports, redirects, basic-auth security
entries, backups, and schedules through separate mutation endpoints. The
relationships do not all have the same meaning:

- ports, redirects, and security entries belong only to an application;
- mounts can target applications, Compose services, or any supported database;
- backups can target Compose or a supported database and also reference an
  external destination;
- schedules can target applications, Compose services, or privileged server
  scopes.

The upstream OpenAPI document declares empty success objects for these
operations even when the runtime returns rows, collections, or booleans.
Several create operations do not return the new physical identity. Runtime
collection responses can also expose passwords, access keys, tokens, mount
content, and executable command text.

Server, registry, and backup-destination records are external infrastructure.
The IaC workspace selects them but does not own their lifecycle.

## Decision

Port, Redirect, and Security are standalone logical resources with Application
as their required containment parent. Mount, Backup, and Schedule are also
standalone logical resources, but their polymorphic typed target is an owned
property and inferred dependency rather than generalized containment.

The public model uses closed target unions instead of raw service-type strings:

- `ServiceTarget` for mounts;
- `BackupTarget` for database or Compose backups;
- `ScheduleTarget` for application, Compose, and explicitly supported server
  scopes.

Target changes use delete-before-create replacement. Polymorphic upstream
update inputs do not authorize in-place reparenting. Dependency edges order
leaf deletion before target deletion even when the relationship is not
containment.

Servers, registries, and destinations remain external selectors. Configuration
stores a stable local-or-named selector, discovery resolves it from a fresh
minimal collection, and zero or multiple exact matches block the plan. Raw
external IDs never become the durable desired value. Application and service
server associations that Dokploy accepts only during create are replacement
properties; nullable build-server and registry associations proven by the
application update endpoint may change in place.

The SDK owns minimal tolerant response models for every adapter. It never
deserializes a sensitive upstream collection into an unbounded JSON value.
Mount content, security passwords, and schedule command or script bodies are
descriptor-only configuration values, fingerprinted as opaque sensitive
intent. Safe reads expose only presence where needed. Debug output, errors,
plans, state, journals, and sanitized fixtures never contain their bytes.

Create identity recovery follows the strongest available contract:

- mounts, ports, and schedules validate the returned physical identity;
- redirects and security entries read the authoritative application child
  collection before and after their boolean create operation;
- backups read the authoritative target collection before and after create.

A create succeeds only when exactly one new matching identity is proven. A
missing or ambiguous post-create identity remains an outcome-unknown journaled
operation and is never retried automatically. Direct managed reads must agree
with their authoritative parent or target collection. Collection absence, not
an endpoint's inconsistent HTTP status, is the deletion proof.

Collision keys are target-scoped and adapter-specific: mount path for mounts,
published port plus protocol for ports, regular expression for redirects,
username for security entries, name for schedules, and the full stable
target/destination/prefix/database/service tuple for backups.

Privileged host schedules, web-server backups, and Compose backup metadata are
not enabled merely because the generated API accepts them. Each requires an
explicit secret and privilege design plus live contract evidence before it can
enter the declarative model.

## Consequences

- The state model preserves the semantic difference between ownership,
  containment, dependencies, and external selection.
- Adding a target kind requires extending a closed union rather than accepting
  arbitrary type and ID pairs.
- Discovery may perform an authoritative collection read per target and a
  fresh selector read, trading request volume for identity safety.
- Resources whose create endpoint omits an ID require preflight and postflight
  discovery and can legitimately stop in recovery-required state.
- Fixture capture and live E2E tests must prove create, direct or collection
  read, update, delete, authoritative absence, cleanup, and secret exclusion
  for every adapter.
