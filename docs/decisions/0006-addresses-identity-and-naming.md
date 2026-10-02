# ADR 0006: Addresses, identity, and naming

## Status

Proposed. Keeps the first engine's invariants 10 to 12 (remote ids give physical
identity; logical addresses give configuration identity; moves preserve identity)
and its documented plan to move to environment-qualified addresses.

## Context

The first engine used flat `kind.name` addresses unique per kind across a project,
forced the importer to invent environment-prefixed names on collision, and used
the logical name as the remote name, so a service called "Leganews Staging Search"
could not round-trip. Dokploy also generates an `appName` (the Docker service
name) with a random suffix; two instances built from the same YAML otherwise
differ in every service name.

## Decision

### Four different names

| Name | What it is | Where it lives |
|------|------------|----------------|
| **key** | the map key in the document; identifies the resource in configuration | document |
| **address** | the hierarchical path built from keys | state, CLI, directives |
| **name** | the remote display name Dokploy shows | spec field `name`, defaults to the key |
| **app name** | Dokploy's Docker-level name (`appName`) | spec field `app_name`, create-only, optional |

### Address

An address is a `/`-separated path of `kind.key` segments from the document root:

```
project.leganews-platform/environment.staging/compose.stack/domain.api
settings: tag.prod        server.build-1        notification.ops-slack
```

Keys match `[a-z][a-z0-9_-]*` and are unique among siblings of one kind. The CLI
accepts any unambiguous **suffix** (`compose.stack/domain.api`, or `domain.api`
when only one exists); an ambiguous suffix is an error that lists the candidates.
State, journals, plans, `moves`, `removed`, and `depends_on` use full addresses.

### Remote identity

Unchanged: the remote id recorded in state is the physical identity; an address
is only a label for it. `moves` rename an address without changing the remote id
and without calling Dokploy.

### Adoption and collisions

The collision rule for creating is the **natural key** the spec declares (usually
`(parent, name)`). If a resource with that natural key exists remotely and is not
tracked, the plan reports it as an **unmanaged collision** and stops; `import`
or an explicit `adopt` directive resolves it. Duplicate natural keys among
siblings, remotely or locally, block planning.

### Names on import

The importer derives a key from the remote name by slugging it (lower-case, runs
of other characters become `-`). Collisions are only possible among siblings of
one kind, are broken by `-2`, `-3` in remote-id order, and are reported. Because
keys are scoped to their parent there is no environment prefixing. The remote
`name` is always written when it differs from the key, so the display name
round-trips.

### `app_name`

`app_name` is optional and create-only. Import writes it so a rebuilt instance
reproduces Docker service names, which Compose files and network aliases often
depend on. When omitted, Dokploy generates one and the plan records it as a
computed value after creation.

## Consequences

- State format 5 keys resources by full address (ADR 0009); there is no automatic
  migration from format 4.
- The importer's name-allocation stage is small. Most of its complexity existed
  only to make flat names unique.
- Renaming a parent changes every descendant address; `moves` accepts a prefix
  move (`from: environment.stage`, `to: environment.staging`) that rewrites the
  subtree in one directive.
