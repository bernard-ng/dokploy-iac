# ADR 0007: Offline configuration commands

## Status

Accepted for the initial MVP.

## Context

Configuration authoring and validation must work before a user configures a
Dokploy endpoint or stores an API key. These commands also run in CI, where no
terminal is available and configuration paths may be untrusted.

Initialization must never replace an existing filesystem entry. Validation
must not wait indefinitely on special files, read an unbounded input, or reveal
source values in diagnostics. On Unix, it must also reject a symbolic-link
leaf without following it.

## Decision

`dokploy init`, `dokploy schema`, and `dokploy validate` dispatch before
connection-context, credential-store, SDK, or network setup. Global connection
overrides are accepted by the parser but ignored by these offline commands.

`dokploy init --empty` is the explicit non-interactive form. Initialization
writes the canonical starter document to an owner-only temporary file in the
destination directory, flushes and synchronizes it, installs it without
clobbering an existing path, and synchronizes the parent directory on Unix.
Missing parent directories are not created implicitly.

On Unix, validation opens one file with no-follow and nonblocking flags, checks
that opened handle is regular, and reads at most one byte beyond the one-MiB
configuration limit before parsing. Other platforms use the strongest portable
standard-library fallback: a symbolic-link metadata precheck followed by
opened-handle regular-file and size checks. Semantic failures are rendered in
source order with stable issue codes and locations, but never with source
snippets or scalar values.

Schema output is deterministic pretty-printed JSON with one trailing newline.
Runtime parsing and semantic validation remain authoritative.

## Consequences

- Editors and CI can initialize, inspect, and validate configuration without
  Dokploy access.
- Existing files, directories, and symbolic links are never replaced by
  initialization.
- Offline commands cannot accidentally query a credential store or contact a
  server.
- Interactive import choices remain a later workflow; the MVP initializes the
  canonical empty configuration.
- Non-Unix platforms retain the standard-library regular-file checks but
  cannot request Unix no-follow and nonblocking open flags.
