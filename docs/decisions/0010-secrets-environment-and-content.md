# ADR 0010: Secrets, environment variables, and content files

## Status

Accepted (2026-10-02). Supersedes the first engine's rule that configuration never contains
or produces secret-bearing text, and keeps its rule that secrets never enter state,
plans, logs, diagnostics, or snapshots (invariant 18; first engine ADRs 0010, 0012,
0013).

## Context

The first engine refused to read any remote secret, so import dropped passwords,
environment variables, and Compose files, and a fresh instance could not be
rebuilt from it. The goal needs those values to live somewhere in the repository
or beside it, without leaking into places they should not go.

## Decision

### Three value classes

| Class | Examples | In configuration | In state | In plans |
|-------|----------|------------------|----------|----------|
| **public** | image name, host, port | literal | literal | path only |
| **secret** | passwords, tokens, keys, secret env values | a **source** | keyed fingerprint | path only |
| **content** | Compose files, config files, scripts | a **file** source | content digest | "content changed" |

The spec assigns a class to every field; environment variables are classified per
value (below).

### Sources

A source says where a value comes from. Literals are allowed only for public
fields.

```yaml
password: { env: PG_PASSWORD }                 # process environment
password: { file: secrets/pg.password }        # a file, workspace-relative
token:    { vault: { provider: infisical, secret: API_TOKEN } }   # Dokploy resolves it
image:    registry.example.com/app:1.2         # public literal
```

`vault` sources render Dokploy's `${{vault.<provider>.<secret>}}` reference, which is what is sent
and fingerprinted; the reference is not secret and the value never reaches this tool. Only an
environment variable can be a vault reference. That the provider exists is checked once the
settings document has `vault_providers` (M5); planning now checks the named provider against fresh
settings state. `env` and `file` values are read at plan and apply time,
used for the one request that needs them, fingerprinted, and zeroised. A missing
variable or file fails the command before any change.

### Environment variables

`environment` is a map from name to value, where a value is `{value: ...}` (public),
`{secret: <source>}`, or `{vault: ...}`; a bare string is shorthand for `value`.
For long lists, `env_file: { file: secrets/staging/api.env }` expands a dotenv file
into the map; entries written explicitly win. Values that come from a file or
`env` are **secret by default**; `env_file: { file: ..., public: true }` declares
the file non-secret. Plans name the keys that change and never their values.

A spec opts a field in with `file_key: env_file` (the application's and the Compose's
`environment`). The file is read when the document is compiled, below the workspace like any
`file` source. A line is blank, a `#` comment, or `KEY=VALUE` (`export ` before the name is
accepted); one matching pair of `"` or `'` around a whole value is removed, as `docker compose`
does, and nothing inside it is unescaped; a name that is not letters, digits, and underscores, a line that is none of those, and a
name set twice are errors that give the line number and never a value. Each variable is fingerprinted
like one written in the map, so a changed value, a new variable, or one that is gone from the file
is a change of that variable alone. A document that gives an environment variables (in the map or in
a file) owns exactly the variables it has named: one that a later apply no longer names, from the map
or from the file, is removed from Dokploy's environment, and one it never named is left alone. State
records the removal as an explicit clear, which the next plan does not report again. A file with no
variables, next to a map with none, declares the environment empty and clears it, as
`environment: {}` does. This is an exception to ADR 0004's "a field left out is unmanaged": it holds
for the variables of an environment the document declares, not for an environment it leaves out.
`build_args` and `build_secrets`, preview variants, and project and environment
level `env` use the same type.

### Content files

A `file` source refers to a path under the workspace (no traversal, no symlink
escape). The engine compares a receipt of the local content with a receipt of the remote content
read through the projection and plans an update when they differ. The receipt is the keyed
fingerprint of the secrets, not a bare SHA-256: a bare digest of a short file can be guessed, and
state holds it. A property whose spec says `trim` is fingerprinted without its surrounding
whitespace on both sides, because Dokploy stores a `patch`'s content with a trailing newline added
by an update (captured on 0.30.6 and 0.30.7). A content property Dokploy does not return (the
application's Traefik file) is not compared, so a change made there is not seen. The value of each
environment variable is compared the same way, from the text Dokploy holds for the block. `plan --show-content` prints
a unified diff because the local file is already on disk. Import writes content to
`files/` by default. A secret scanner (pattern and entropy) warns when imported
content looks like it embeds credentials and offers `--content-secrets=extract` to
move the matches to environment variables.

### Where secrets live in the repository

By default: `secrets/` directory of dotenv and plain files, written mode 0600,
with `.gitignore` entries added automatically, never committed. `SecretsWriter` (in the engine)
does the writing for an importer: private directories and files, atomic, nothing replaced, a path
that leaves the directory refused, a `.gitignore` inside the directory that ignores everything in it
and an entry in the workspace's own, and dotenv text that reads back as exactly the values written. Users who keep
secrets in a manager point `env:` sources at it (CI secrets, `direnv`, `sops exec-env`).
The tool does not decrypt anything itself in the beta; a `sops` source is a candidate
for later and needs its own ADR.

### Fingerprints

Unchanged in design: an HMAC-SHA-256 key per instance in the OS credential store,
or `DOKPLOY_FINGERPRINT_KEY` (`<uuid>:<64 hex>`) for headless use, binding instance,
address, property path, and value. Rotating the key re-sends every secret once.

### Import classification

`dokploy import --env=auto` (default):

1. Remote values are read in memory.
2. A variable is **public** if its name does not match
   `(?i)(secret|token|key|pass|pwd|credential|dsn|private|auth|salt)` and its value
   is not high-entropy and not a URL with embedded credentials; otherwise **secret**.
3. Public values are written inline; secret values go to
   `secrets/<document>/<service>.env` and the document gets an `env_file`.

`--env=inline` writes everything inline (with a warning), `--env=file` puts every
variable in the dotenv file, `--env=skip` leaves environment unmanaged. The
classification is conservative and reported; reviewing it is part of the import.

### Settings secrets

Registry passwords, S3 keys, notification tokens, DNS credentials, SSH private
keys, Git provider secrets, and vault credentials follow the same path: read on
import, written to `secrets/settings/*.env`, referenced by `{env: ...}` sources.

## Consequences

- Import produces a complete, rebuildable description without leaking secrets into
  the committed YAML or into state.
- Secrets exist on the operator's disk (git-ignored) and in process memory, which is
  unavoidable for a tool that must send them; the threat model is stated in the
  security notes of the reference docs.
- The conformance suite runs every scenario with canary secrets and scans all
  outputs, state, journals, and logs (ADR 0015).
