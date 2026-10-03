# Kind spec format

The grammar of the files in `specs/`, decided by
[ADR 0002](../decisions/0002-kind-specs-are-the-source-of-truth.md). **Grammar v0 is
implemented** by the `dokploy-spec` crate (`crates/dokploy-spec`), and the specs under
`specs/` are the working reference: `registry` (flat), `redirect` (leaf),
`application` (union and `env`, partial), plus the partial `project` and
`environment` that complete the hierarchy. `cargo xtask specs --check` lints them and
checks the request side of the coverage ledger against `openapi/dokploy.json`. The
rest of the engine does not read specs yet (milestone M1 onwards); changes to the
grammar after M0 go through an ADR.

## Files

One file per kind (or per family of near-identical kinds, such as the notification
providers), named `specs/<scope>/<kind>.yaml`. `specs/versions.yaml` lists the
supported Dokploy versions (ADR 0014). `specs/types.yaml` holds shared types
(`env`, `swarm`, `resources`, `domain-tls`, `notification-events`).

## Top level

```yaml
kind: registry                 # address prefix and spec id
scope: settings                # settings | project
parent: null                   # containing kind (environment, application, ...) or null
section: registries            # key in the parent's YAML mapping
since: "0.26.0"                # optional Dokploy range
title: Container registry      # for docs and diagnostics
class: managed                 # managed | adopt_only  (ADR 0013)
coverage: full                 # full (default) | partial: a prototype whose
                               # unclassified request fields are counted, not failed
```

## Identity

```yaml
identity:
  key: name                    # field that is the natural key; `null` when the key is
                               # purely logical (a redirect has no name)
  collision: [name]            # fields that must be unique among siblings, remotely
  address: "registry.{key}"
```

## API

Operations are OpenAPI `operationId`s as `resource.operation`. Parameters name the
request field that carries the identity.

```yaml
api:
  id: registryId               # id field in requests and responses
  create:   { op: registry.create }
  update:   { op: registry.update }
  remove:   { op: registry.remove }
  read:
    list:   { op: registry.all, authority: authoritative }
    one:    { op: registry.one, id_param: registryId, agree: true }
  create_identity:             # ADR 0008: how the new id is learned
    diff_collection: { key: [name] }      # or: from_response: /registryId   or: none
  deploy: null                 # { op: compose.deploy, wait: deployment } for deployable kinds
```

A collection embedded in a parent's response is declared on the child:

```yaml
  read:
    list: { embedded_in: { parent_op: application.one, pointer: "/redirects" } }
```

A collection read once per parent says so: `list: { op: environment.byProjectId, scope: { param: projectId } }`
(the parent's id is sent as that query parameter). A kind can instead be found in a collection
embedded in its parent's direct read (`embedded_in`), and a kind with neither is found by its
recorded identity only.

## Fields

```yaml
fields:
  name:
    api: registryName          # JSON pointer in request and response; default = field name
    type: text
    min_len: 1
    class: public              # public | secret | content   (ADR 0010)
    mutability: in_place       # in_place | create_only | reparent | write_only | computed
    nullable: false
    default: key               # `key` = remote name defaults to the document key
    needs_deploy: false
  password:
    api: password
    type: text
    class: secret
    mutability: write_only
    resend_on_update: true     # update endpoint replaces the whole object (ADR 0008)
  server:
    api: serverId
    type: selector(server)
    nullable: true
    mutability: in_place
```

Attributes: `api`, `type`, `class`, `mutability`, `nullable`, `default`,
`needs_deploy`, `disruptive`, `granularity` (`field` or `key`), `trim`, `since`,
`until`, `doc` (one line for the generated reference), `example`.

### Types

`text`, `int`, `number`, `bool`; `enum[a, b]`; `list<T>`, `set<T>`, `map<K, V>`;
`struct{...}`; `union(tag){arm: {...}}`; `blob(<json-schema file>)`; `env`; `file`;
`ref(kind)`; `selector(kind)`; shared types by name (`type: swarm`). Semantics are in
[ADR 0004](../decisions/0004-property-model-and-mutability.md).

## Property paths

A *property* is a field at planning granularity; its dotted path is what plans, state, and
`ignore_changes` name. `KindSpec::property(path)` and `SpecRegistry::property(kind, path)` in
`dokploy-spec` decide which paths are legal and what each means, from the spec alone
([ADR 0004](../decisions/0004-property-model-and-mutability.md)):

| Field | Properties |
|-------|------------|
| scalar, enum, list, set, file, ref, selector, blob, shared type, union | one: the field name (a union is planned as one value; its arm members are not paths) |
| `env` or `map` with `granularity: key` | the root (`environment`: owned only to clear it or declare it empty) and one entry per key (`environment.LOG_LEVEL`) |
| `struct` with `granularity: field` | one per member (`limits.cpu`); the struct itself is not a path |
| `struct` without it | one: the field name |

A path carries what the planner needs: its type and value rules, `class`, `mutability`,
`nullable`, the selector kind, and whether it is required on create. Environment entries are
always sensitive (secret by default, ADR 0010), so a plan names the variables that change and
never their values. A member inherits a non-default `mutability` from its struct.
`MutationContract::from_spec` (in `dokploy-core`) follows from the same facts: `in_place` and `write_only` change in place,
`create_only` forces replacement, `computed` is never configurable, and only a `nullable`
property can be cleared. The spec does not yet state a replacement order, so replacement
deletes before it creates.

## Write groups

Every `in_place`, `reparent`, and `write_only` field belongs to exactly one group.
The executor issues one request per group with a change.

```yaml
write:
  - { op: registry.update, fields: [name, type, url, username, password, image_prefix, server],
      shape: full }
```

```yaml
# compose, excerpt
write:
  - { op: compose.update,          fields: [name, description, compose_type, auto_deploy, ...], shape: partial }
  - { op: compose.saveEnvironment, fields: [environment, create_env_file], shape: partial }
```

`shape` is `partial` or `full` (ADR 0008). `by_variant: source` selects the operation
by a union tag.

## Children

```yaml
children:
  - { kind: domain,   section: domains }
  - { kind: mount,    section: mounts }
  - { kind: schedule, section: schedules }
```

A child spec names `parent:` and how it attaches (`attach: { field: composeId, from: parent_id }`
plus any fixed values such as `domainType: compose`).

## Hooks

```yaml
hook:
  name: tag_membership
  reason: project tag association is a relation, not a field; see the hook's tests
```

A hook is a Rust function registered under `name`. It may replace `discover`,
`create_body`, `after_create`, or `diff`, never the document model. A spec without
`hook` is fully generic.

## Ledger section

Fields of the referenced operations that are deliberately not configuration:

```yaml
ledger:
  readonly: [createdAt, organizationId]
  derived:  [sourceType]       # request fields implied by another field (a union tag)
  ignored:
    - { field: registryType, reason: only value is "cloud"; kept as a constant }
```

`cargo xtask specs --check` fails if any request or response field of the kind's
operations is neither in `fields`, nor in `ledger`, nor `derived`.

## Worked examples

### A flat settings kind: `registry`

The registry spec above, completed: eight fields, one write group, no hook, no
children. This is the cost target for ADR 0002: a file this size plus a fixture
capture, and no Rust.

### A leaf with a fresh-read update: `redirect`

(The working file is `specs/project/redirect.yaml`.)

```yaml
kind: redirect
scope: project
parent: application
section: redirects
identity: { key: null, collision: [regex], address: "redirect.{key}" }
api:
  id: redirectId
  create: { op: redirects.create, attach: { applicationId: parent_id } }
  update: { op: redirects.update }
  remove: { op: redirects.delete, id_param: redirectId }
  read:
    list: { embedded_in: { parent_op: application.one, pointer: "/redirects" } }
    one:  { op: redirects.one, id_param: redirectId, agree: true }
  create_identity: { from_response: "/redirectId" }
fields:
  regex:       { api: regex,       type: text, min_len: 1, mutability: in_place }
  replacement: { api: replacement, type: text, min_len: 1, mutability: in_place }
  permanent:   { api: permanent,   type: bool,             mutability: in_place }
write:
  - { op: redirects.update, fields: [regex, replacement, permanent], shape: full }
```

### A union and env block: application source and environment

```yaml
fields:
  source:
    type: union(type)
    mutability: in_place
    arms:
      github:
        provider:   { api: githubId, type: selector(git_provider) }
        owner:      { api: owner,      type: text }
        repository: { api: repository, type: text }
        branch:     { api: branch,     type: text }
        build_path: { api: buildPath,  type: text }
        trigger_type: { api: triggerType, type: "enum[push, tag]" }
      docker:
        image:        { api: dockerImage, type: text }
        registry_url: { api: registryUrl, type: text }
        username:     { api: username,    type: text }
        password:     { api: password,    type: text, class: secret, mutability: write_only }
      # gitlab, bitbucket, gitea, git arms follow the same pattern
  environment:
    api: env
    type: env
    granularity: key
    needs_deploy: true
write:
  - { op: application.update,          fields: [name, description, replicas, command, args], shape: partial }
  - { op: application.saveEnvironment, fields: [environment, build_args, build_secrets, create_env_file], shape: partial }
  - { by_variant: source, ops: { github: application.saveGithubProvider, docker: application.saveDockerProvider,
                                 gitlab: application.saveGitlabProvider, git: application.saveGitProvider } }
```

## What the engine derives from a spec

JSON Schema; parser and canonical writer; spans for diagnostics; desired-state
compilation; discovery reads; projection; planner `MutationContract`; create,
update, replace, and remove requests; journal steps; verification reads; importer
and convergence proof; `dokploy coverage` and the generated reference page; the
conformance scenarios and simulator behavior (ADR 0015).
