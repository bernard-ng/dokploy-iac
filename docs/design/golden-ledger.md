# Golden ledger

The first engine's tests define what "done" means for a ported kind
([ADR 0016](../decisions/0016-replace-the-engine-keep-the-kernel.md), the parity
oracle). The ledger turns that into a checked list: every legacy test is one
**scenario** in `goldens/<bucket>.json`. The first engine and its tests are deleted; the ledger
keeps what they asserted, as the definition of done for porting each kind.

```bash
cargo xtask goldens           # mine the tests, keep every classification
cargo xtask goldens --check   # verify the ledger against the tree (runs in CI)
```

## What is mined

`goldens/sources.json` lists the files that are mined, each with a **bucket** (a kind such
as `redirect`, or a concern such as `engine`, `document`, `import`, `secrets`, `sdk`) and
a **layer** (`cli-remote`, `cli-desired`, `cli-apply`, `cli-recovery`, `cli-import`,
`config`, `sdk`, `live`, `unit`). It also lists the test-bearing files that are
deliberately not mined (the kernel crates, the CLI shell, helpers), each with a reason.
A test file that is neither listed fails the check, so new tests cannot escape the ledger.

For each test function the miner records, from the syntax tree rather than by running it:

| Field | Meaning |
|-------|---------|
| `operations`, `requests` | Dokploy operations and request-shaped literals the test, or a same-file helper it calls, names explicitly |
| `fixtures` | `include_str!` fixtures the test loads |
| `data` | File-level constants it reads (canned response bodies and documents) |
| `live` | The test is `#[ignore]`d live acceptance |
| `category` | A guess from the test name, one of the conformance scenarios of ADR 0015 or a concern (`read-model`, `document`, `wire`, `state`, `plan`, `live`); refine it by hand |

Most legacy remote tests use positional canned responses, so the request order is implicit
and `operations` is empty for them; their `data` constants hold the bodies. Helpers in
other files (`tests/support/`) are not followed.

## Classifying

Edit the scenario in its bucket file:

- `pending`: not reproduced yet (the default; carries no `covered_by` or `reason`);
- `covered`: reproduced by the spec engine; `covered_by` names the conformance scenario;
- `dropped`: intentionally not carried over; `reason` says why.

A scenario's bucket is the file it sits in: move the entry to another file to re-bucket it
(the `registry` scenarios were moved out of `external` this way). Re-running
`cargo xtask goldens` refreshes the mined fields and never touches the classification.

## What the check enforces

1. Every test in a listed file has a scenario, and every scenario matches its test
   (otherwise: run `cargo xtask goldens`).
2. Every test-bearing file is listed or excluded.
3. `covered` has `covered_by`, `dropped` has `reason`, categories and layers are known.

A scenario whose test is gone is kept as it is. The first engine's tests were deleted with it, so
most scenarios are `pending` with no test behind them: each is a port target, carrying the
operations, request literals, fixtures, and data the old test used. When a kind is ported, classify
its scenarios `covered` (naming the conformance scenario or engine test that proves it) or
`dropped` (with the reason). Remove a source entry from `sources.json` when its file is deleted.
