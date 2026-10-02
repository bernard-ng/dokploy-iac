# ADR 0013: Instance settings and bootstrap

## Status

Proposed. Keeps the first engine's settings-document, state-scope, and
external-selector decisions.

## Context

A fresh Dokploy needs servers, registries, destinations, certificates, git
providers, notifications, a web-server domain, and more before any project can
deploy. Some of these exist on every instance (the web server, the organization);
some cannot be created over the API (a GitHub App); some change Traefik and can
cut the connection that is applying them.

## Decision

### The settings document

`settings:` holds the kinds in the beta list that are instance-wide
(`docs/vision/dokploy.settings.full.yaml`). Each is flat-keyed and uses the same spec
machinery as project kinds. Apply order within the document follows `ref`s
(a server references its SSH key; a registry may reference a server).

### Kind classes

| Class | Kinds | Behaviour |
|-------|-------|-----------|
| **managed** | tag, ssh key, server, network, registry, certificate, DNS provider, S3 destination, notification, vault provider, AI provider | created, updated, deleted; deletion honours protection; imported protected |
| **adopt-only** | organization, web server, GitHub provider | exist already; configuration updates them; removing the block forgets them (`removed: destroy: false`); `create` and `destroy` are validation errors |
| **excluded** | users, roles, SSO, SCIM, licence, whitelabeling, billing, profile | not in the beta (ADR 0001) |

### Disruptive changes

A spec marks a field or write group `disruptive: true` when Dokploy reloads or
restarts Traefik or the server (web-server domain, Traefik ports, Traefik config,
environment, dashboard). The plan flags these, requires explicit approval even
with `--auto-approve` unless `--allow-disruptive` is also given, applies them
last, and treats a lost connection as an **outcome-unknown** result that recovery
settles by a fresh read, never by retry.

### References from projects

Projects select servers, registries, destinations, tags, vault providers, git
providers, and networks by name (`selector`). A name that matches nothing or more
than one blocks planning (ADR 0007). The workspace applies settings before
projects, so a first apply on a fresh instance resolves them.

### Manual bootstrap

Five steps remain manual and are verified by `dokploy doctor` (ADR 0017):
install Dokploy; create the first administrator; create an API key with
administrator rights; install any GitHub App through the dashboard; run server
`setup` on new hosts (`dokploy api server setup`, an imperative action, never
part of `apply`). `doctor` checks reachability, version, key rights, that the
organization resolves, and that every selector in the workspace could resolve.

### Organization scoping

Calls that need an `organizationId` take it from a fresh `organization.active`
read, never from configuration.

## Consequences

- Settings and projects are independent state lineages applied in a fixed order.
- Kinds that cannot be created are expressible without special cases: the spec
  says `adopt_only`.
- Disruptive handling is a spec attribute plus one executor rule, not per-kind code.
