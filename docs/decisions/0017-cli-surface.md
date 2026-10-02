# ADR 0017: CLI surface

## Status

Proposed. Keeps the first engine's stream separation, explicit-approval rules, and
generated imperative commands.

## Decision

### Declarative commands

| Command | Purpose |
|---------|---------|
| `init [--settings] [--empty]` | write a starter document or workspace |
| `validate` | parse and validate offline (no network, no credentials) |
| `schema [--document project\|settings]` | print the JSON Schema generated from the specs |
| `plan [--json] [--detailed-exitcode] [--out FILE] [--refresh-only] [--show-content]` | compare configuration, state, and fresh remote state |
| `apply [PLAN] [--auto-approve] [--parallelism N] [--deploy] [--allow-disruptive]` | reconcile |
| `deploy [--target ADDR...] [--wait] [--timeout D] [--auto-approve]` | deploy per ADR 0012 |
| `recover [--auto-approve]` | settle an interrupted apply or deploy from journal evidence |
| `destroy [--auto-approve]` | delete every tracked, unprotected resource, dependents first |
| `import settings\|project [ID]\|all [--env=auto\|inline\|file\|skip] [--dry-run] [--secrets-out DIR]` | adopt an instance into a new workspace |
| `state list\|show\|mv\|rm\|protect\|unprotect` | inspect and repair local state; addresses per ADR 0006 |
| `doctor` | check URL, key, version, rights, organization, and selector resolution |
| `coverage [KIND]` | print the effective spec and the ledger for a kind |
| `fmt` | rewrite documents canonically |
| `completions SHELL` | shell completion script |

All declarative commands take `--file` (a document) or run on the workspace
manifest with `--all`; `--target ADDR` narrows `plan`, `apply`, and `deploy` to an
address and its descendants.

### Imperative commands: everything under `api`

Every generated API command lives under one predictable prefix:

```
dokploy api <resource> <operation> [options and arguments]
dokploy api compose stop --compose-id ...
dokploy api server setup --server-id ...
dokploy api application one --application-id ...
```

The top level then holds only the declarative verbs above, with no resource names
competing with them, and a reader can tell at a glance whether a command reads or
changes declared state (top level) or makes one direct API call (`api`). There is
no un-prefixed alias: the generated resource commands (`dokploy application ...`)
are removed from the top level (pre-release policy, ADR 0001). `dokploy api --help`
lists the resources; `dokploy api <resource> --help` lists their operations. The
commands are still generated from `openapi/dokploy.json`, use the same transport as
the engine, and are the supported way to run actions that are not state
(start, stop, rebuild, rollback, run a schedule, server setup).

### Streams and exit codes

Results and JSON on standard output; plans, prompts, warnings, and diagnostics on
standard error. Non-interactive approval requires the explicit flag; closed input
is never consent. Exit codes: 0 success or no changes; 1 error or blocked plan;
2 applyable plan with changes (`--detailed-exitcode`).

### Connection

Unchanged: flag, then environment (`DOKPLOY_URL`, `DOKPLOY_API_KEY`), then the
selected context. `context set` is added so contexts and their keys can be created
from the command line.

## Consequences

- The generated resource commands move under `api`; `import all`, `deploy`, `doctor`, `coverage`, `fmt`, and `context set` are new;
  `import <kind>` forms from the first engine do not return.
- `coverage` and `schema` are generated from the same specs as everything else.
