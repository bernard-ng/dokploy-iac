# Vision maps

Two documents that show **every configurable field Dokploy `v0.30.6` exposes**,
written in the shape this tool is built to model. They are version 2 documents that the
parser does not accept yet: each field the specs do not know is a **gap**, and the
milestones close them until both files validate (M3 for the project document, M5 for the
settings one). Values are invented.

- [`dokploy.full.yaml`](dokploy.full.yaml): a project, its environments, and
  everything inside them.
- [`dokploy.settings.full.yaml`](dokploy.settings.full.yaml): instance-wide
  settings, grouped like the dashboard's Settings menu.

Each field carries a trailing marker: `✔` modeled today, `✘` in the API but not
modeled, `✎` a design proposal that changes a shape, `[c]` create-only, `[s]`
secret, `[a]` adopt-only, `[x]` deliberately not configuration. Both files are
derived from `openapi/dokploy.json` and parse as YAML.

## Measuring progress

`crates/dokploy-model/tests/vision.rs` parses both files against the repository's specs and
compares what the parser rejects with the checked-in lists in [`gaps/`](gaps/): one line per
problem, `CODE dotted.path`. The test fails when the two differ, in either direction, so a
spec that learns a field and a spec that loses one are both visible in review.

```bash
UPDATE_VISION_GAPS=1 cargo test -p dokploy-model --test vision   # rewrite the lists
```

The lists are a **frontier**, not a total: the parser stops at a section it does not know, so
the fields inside it show up only once the section exists. A list can therefore grow while
the work advances; the goal is that both are empty.

Companion: [`../roadmap.md`](../roadmap.md) for the plan.
