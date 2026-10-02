# Vision maps

Two fictional documents that show **every configurable field Dokploy
`v0.30.6` exposes**, written in the shape this tool could model. They are review
aids for deciding scope and direction, not valid input: `dokploy validate` rejects
them on purpose.

- [`dokploy.full.yaml`](dokploy.full.yaml): a project, its environments, and
  everything inside them.
- [`dokploy.settings.full.yaml`](dokploy.settings.full.yaml): instance-wide
  settings, grouped like the dashboard's Settings menu.

Each field carries a trailing marker: `✔` modeled today, `✘` in the API but not
modeled, `✎` a design proposal that changes a shape, `[c]` create-only, `[s]`
secret, `[a]` adopt-only, `[x]` deliberately not configuration. Both files are
derived from `openapi/dokploy.json` and parse as YAML.

Companion: [`../beta-roadmap.md`](../beta-roadmap.md) for the gap analysis and plan.
