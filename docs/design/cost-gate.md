# Cost gate (end of M2)

The plan says to measure the cost of a kind after the first ports and compare it with the targets of
[ADR 0002](../decisions/0002-kind-specs-are-the-source-of-truth.md) and
[ADR 0015](../decisions/0015-testing-simulator-and-live-contracts.md), and to stop and re-plan if
they are badly missed. Measured on `tag` and `registry` (flat) and `redirect` (a leaf under three
ancestors), on 2026-10-03.

## Result

**The targets are met for the flat and leaf shapes. The union shape is not measured yet**, so the
gate is passed for what was tried and open for what was not (see "What this does not show").

| Measure | Target | Before (hand-written kinds) | Measured |
|---------|--------|--------------|----------|
| Rust per flat kind | none | about 950 lines (Redirect and Security: 1,900 for two) | **0**: `tag` is a 42-line spec, `registry` 66, `redirect` 41 |
| Test code per kind | none | about 1,575 lines | **0**: the conformance suite enrols a kind from its spec (21 to 26 scenarios each) |
| Hooks | fewer than a fifth of kinds | n/a | **0 of 3** |
| Adding a field | one spec entry | about ten edits across six crates | one entry under `fields:` and one name in a `write:` group; a field in no group is rejected by the lint, and a group on the wrong operation fails the suite |
| Adding a write group | not a Rust change | n/a | a `write:` entry (two lines), checked by the suite per field |
| Simulator-versus-live differences | found and fixed | n/a | see below |

The engine is a fixed cost, not a per-kind one: `dokploy-engine` and `dokploy-sim` and
`dokploy-conformance` are about 5,900 lines of Rust and 2,800 lines of their own tests. The first
engine's per-kind layers that this replaces were deleted: 91,000 lines across the CLI, the config
crate, and the SDK (the generated bindings and the golden ledger are not counted).

## What a kind still costs

A kind is a spec file plus **fixtures**: a capture script of about 100 lines of shell per kind
(`capture-tag-contract.sh`), run once per supported version. That script is the part that does not
yet come from the spec, and the largest remaining cost. Turning captures into something derived from
the spec would remove it; it is worth doing before M5, when about 40 kinds need captures.

## Differences between the specs, the simulator, and the live Dokploy

Found by capturing and by running the engine live on **0.30.6 and 0.30.7** (both behave the same
for these kinds). Every one was a wrong prototype spec or a missing fact, not an engine defect:

- `registry.update` is a **patch**, not "send the whole object", so `shape: full` and
  `resend_on_update` were wrong. Fixed.
- `registry.create` returns the whole object, so its identity comes from the response, not a diff. Fixed.
- `redirects.create` answers `true`, so a redirect's identity is learned by diffing the parent's
  collection, not from the response. Fixed.
- `application.search` is paged (`{items, total}`), so the prototype `application` spec could not be
  discovered by its collection. Its collection read was removed until M3 reads pages.
- A registry whose `docker login` Dokploy rejects is **saved anyway** (HTTP 400, the change persists).
  This is a Dokploy behavior the simulator now models (`RejectAfter`), the live test exercises, and
  recovery handles.
- `registry.all` lists the password; `registry.one` omits it. The projection drops it at the
  boundary; the suite scans for it on every scenario.
- Tag names are unique at Dokploy and registry names are not. The simulator enforces neither (the
  engine's own collision handling is what the suite tests); a live run is what shows the difference.

No difference was found between the simulator and the live Dokploy by running the same scenarios
against both, because the live run (`engine-live`) covers a lifecycle, not the whole suite. Running
every conformance scenario live is the M8 differential.

## What this does not show

- **The union shape** (an application `source`: one `save*Provider` per arm) was not exercised. It is
  the case most likely to need a hook or a grammar change, and the gate's third shape. The engine
  refuses composite values in its preflight today. Measuring it needs the application port, so it is
  the first thing to try in M3, with the decision to extend the grammar made by ADR if it needs more
  than `by_variant`.
- **Follow-up updates after a create** (a field the create operation does not accept) are refused, and
  every project kind will hit that (an application takes its source in a separate call).
