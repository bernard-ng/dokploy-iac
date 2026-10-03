# State format 5

How durable state is keyed and laid out
([ADR 0006](../decisions/0006-addresses-identity-and-naming.md),
[ADR 0009](../decisions/0009-state-v5-and-scopes.md)). Implemented by `dokploy-state`.

## Addresses

An address is a `/`-separated path of `kind.key` segments from the document root, the path of
keys through the containment tree:

```
project.shop/environment.staging/application.api/redirect.www
registry.main                                              (settings document)
```

A resource's containment is the resource its address is nested under, and that resource must
exist in the same state; both rules are checked when state is written, read, moved, and planned.
A first-engine address such as `application.api` is a path of one segment, with the same text as
before, and has no path parent.

`ResourceAddress::resolve_suffix` resolves what a user types: any suffix that matches exactly one
known address (`domain.api`, or `compose.stack/domain.api`). Several matches are an error that
lists the candidates. `StateFile::move_resource` is a prefix move: everything below the moved
address moves with it, containment and dependencies are rewritten, and remote identities do not
change. Moving a service under another environment is the same operation.

## Kinds

`ResourceKind` is a name. Every kind comes from a spec and is registered, with its scope and
containment parent, before it can be parsed or deserialized
(`dokploy_core::register_spec_kinds`), so state and journals can only name kinds the running tool
knows. Registration is bounded and idempotent.

## The file

```json
{
  "formatVersion": 5,
  "cliVersion": "0.1.0",
  "document": "project.shop",
  "lineage": "...",
  "serial": 12,
  "instance": "https://deploy.example.com/",
  "resources": { "project.shop": { "kind": "project", "remoteId": "...", ... } }
}
```

`document` is `settings` or `project.<slug>`. Only format 5 is read; older files are refused with a
message to re-import, since there is no migration (ADR 0001). A store refuses a file recorded
under another document, and refuses to write one.

## Layout

One directory per document under `.dokploy/`, each with its own lock, journal, and backup, so a
crash while applying one project never blocks another:

| Document | Directory |
|----------|-----------|
| `settings` | `.dokploy/settings/` |
| `project.<slug>` | `.dokploy/projects/<slug>/` |

## What state holds

The same as before: last-applied non-sensitive inputs, and for sensitive properties only keyed
fingerprints, by dotted path. State validates the **syntax** of a sensitive path (a lower snake
case field and up to three keys, such as `password` or `environment.LOG_LEVEL`); the kind's spec
decides which paths exist and are sensitive. Managed inputs follow one structural rule: a key that
looks secret, or the environment collection, may only be *cleared* (`null`, or an object of
`null`s). A value under such a key is rejected wherever it appears, so a secret cannot hide in
nested JSON. Whether a field may be cleared is the spec's `nullable`.
