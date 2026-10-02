# ADR 0005: Document model v2 and workspace layout

## Status

Accepted (2026-10-02). Replaces the first engine's configuration language (its ADRs 0006 and
0007) and its two-document design.

## Context

The first format nested some children under their service (ports, redirects,
security) and others at environment level with a `target:` address (domains,
mounts, schedules, backups). That asymmetry forced name uniqueness per kind
across a whole project and made Compose domains impossible. It also modeled one
project per file and had no notion of an instance made of settings plus several
projects.

## Decision

### Documents

A **document** is one YAML file with `version: 2` and exactly one root:
`project:` or `settings:`. Unknown fields are rejected everywhere; YAML anchors,
aliases, merge keys, tags, and multiple documents per file are rejected; size,
node, and depth budgets apply, as before.

```yaml
version: 2
project:
  slug: leganews-platform        # logical key
  name: Leganews Platform        # remote display name (optional; defaults to slug)
  environments:
    staging:
      applications:
        api:
          domains:               # children nest under their service
            main: { host: api.example.com, https: true }
```

The shape of every kind is exactly what [`docs/vision/`](../vision/) shows,
adjusted by the review of those files. The nesting rule: **a resource is written
inside the resource that contains it**; references that cross containment use
`ref(kind)` (inside the document) or `selector(kind)` (outside it).

### Directives

`moves`, `removed`, `lifecycle.protect`, `lifecycle.ignore_changes`,
`lifecycle.deploy` (ADR 0012), and `depends_on` keep their meaning. Directives take
hierarchical addresses (ADR 0006).

### Workspace

A **workspace** is a directory:

```
dokploy.workspace.yaml     optional manifest: documents and their order
settings.yaml              the settings document
projects/<slug>.yaml       one project document each
files/                     content referenced by `file` sources (compose, scripts, configs)
secrets/                   dotenv files for secret sources; git-ignored
.dokploy/                  state, journals, locks; one lineage per document
```

With no manifest, the tool treats a single `dokploy.yaml` or `dokploy.settings.yaml`
as before, so a one-project repository stays one file. With a manifest, documents
are applied in the order **settings, then projects**, and a project may depend on
settings objects through selectors. `dokploy import all` writes the manifest.

### Version

The new format is `version: 2` so that a document written for the first engine
fails loudly instead of being half-understood; this is not a compatibility
promise. The first engine's `version: 1` documents are rejected with a message
that says to re-import. There is no converter (pre-release policy, ADR 0001):
`import` regenerates a faithful document from the live instance.

## Consequences

- Name uniqueness is per parent, so `domain.main` can exist under several services.
- The generated JSON Schema is one file per document root, built from the specs.
- Hand-written v1 documents must be re-imported or rewritten.
- A multi-document workspace has several state lineages; operations that span
  documents (`apply --all`) run them sequentially and stop at the first failure.
