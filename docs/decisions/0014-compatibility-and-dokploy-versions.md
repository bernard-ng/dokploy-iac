# ADR 0014: Compatibility and Dokploy versions

## Status

Proposed.

## Context

Dokploy ships often. The vendored contract is `v0.30.6`; the maintainer's
instance runs `v0.30.7`. The first engine never checked the server version.
Responses are untyped (ADR 0007), so a silent shape change is the main risk.

## Decision

### Supported set

`specs/versions.yaml` lists the supported Dokploy versions (initially `0.30.6` and
`0.30.7`). For each, the repository holds the pinned OpenAPI document
(`openapi/<version>.json`) and the live fixtures (`fixtures/api/live/<version>/`).
Specs mark fields and operations with `since` and `until` when behavior differs;
the common case is no marker.

### Runtime check

Before any command that reads or writes through the declarative engine, the tool
reads the server version (`settings.getDokployVersion`) and applies this policy:

| Server version | Behaviour |
|----------------|-----------|
| in the supported set | proceed |
| same minor, newer patch | proceed with a warning; the CI upstream watch (below) is expected to have vetted it |
| different minor, or unreadable | reads allowed (`plan`, `import`); mutations refused unless `--allow-unsupported-version` |

The version string is recorded in the journal of every apply.

### Upstream watch

A scheduled CI job downloads the newest Dokploy release's OpenAPI document and runs
`cargo xtask specs --check --against <file>`. It reports added, removed, and
changed operations and fields against the ledger (ADR 0002) and opens an issue
when anything is unclassified. It never updates specs automatically. Adding a
version to the supported set requires fixtures captured from that version and a
passing live suite.

### Tool compatibility

After the beta tag: the document format and state format are versioned and only
change with a migration; command-line flags follow deprecation for one minor
release. Before the beta tag: ADR 0001's pre-release policy applies.

## Consequences

- Version drift becomes a CI event, not a user surprise.
- Supporting a version costs fixtures and a live run; the supported set stays small.
- The runtime check is one call and one table lookup.
