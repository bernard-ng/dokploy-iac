# Conformance suite

A kind that has a spec has a test suite, and nobody writes it
([ADR 0015](../decisions/0015-testing-simulator-and-live-contracts.md)). `dokploy-conformance`
derives scenarios from a kind's spec and runs them against the simulator
([`simulator.md`](simulator.md)), through the real engine: the document is parsed, compiled, planned,
applied, recovered, and read back.

```rust
let suite = Suite::new(specs, "fixtures/api/live/v0.30.6".into());
let results = suite.run_all().await;      // every top-level kind with a full spec
println!("{}", report(&results));
```

`crates/dokploy-conformance/tests/conformance.rs` runs it for every spec in `specs/`, so adding a
flat settings kind adds a spec file and nothing else: the new kind is enrolled the moment it has
`coverage: full`.

## What is derived from the spec

For each field the suite makes a sample value of its type (a second one for changes), notes the write
group that carries it, and treats a `secret` or `write_only` field as a secret read from an
environment variable. A field that defaults to the key is left out of the first document so the
default is exercised. Selectors, unions, structs, keyed collections, `env`, and `file` fields are
not exercised yet.

## Scenarios

Each runs in a world of its own: a fresh simulator, workspace, and engine.

| Scenario | Checks |
|----------|--------|
| `create` | one apply creates the resource with every field as written, with the create and one write per group for the fields the create does not accept; state records it; the next plan is empty |
| `create_minimal` | only the required fields; the contract's required nullable fields are sent as `null` |
| `converges` | applying again sends nothing |
| `update:<field>` | changing one field sends exactly its write group's operation, only fields of that group, with the new value (a secret is rotated) |
| `replace:<field>` | changing a `create_only` field removes then creates and gives a new identity |
| `drift:<field>` | changing a field behind the engine's back plans one change naming it, and apply restores it |
| `follow_up_rejected`, `follow_up_lost_before`, `follow_up_lost_after` | for a kind whose create does not carry every field: the follow-up write is rejected or lost; the resource exists and state describes what the create wrote, recovery settles the step without repeating the create, and one more apply finishes (skipped when the create carries everything) |
| `delete`, `protected_delete` | removal sends one remove; a protected resource is refused with nothing sent |
| `unmanaged_collision`, `ambiguous_collision` | an existing resource, or two, with the same identity block the plan and nothing is changed |
| `partial_authority` | absence from a `partial` collection is not proof |
| `read_failure` | an unreadable collection blocks the apply |
| `removal_not_applied` | Dokploy acknowledges a remove and does nothing: the step fails (the journal is closed by `recover`), the resource stays tracked, and the retry removes it |
| `identity_ambiguous` | another client creates the same thing at the same moment: a create whose identity is learned by diffing the collection says the outcome is unknown instead of picking one, and recovery leaves it to a person |
| `foreign_collection_item`, `foreign_direct_read` | a child collection, or a direct read, that names another parent is not this parent's child: the plan is blocked |
| `declined`, `rejected_create` | a declined plan changes nothing; a rejected create leaves nothing recorded and `recover` closes its journal so the retry works |
| `unknown_{create,update,delete}_{before,after}` | the request is dropped before, or the response after, Dokploy applies it; the mutation is sent once, recovery chooses the right action (adopt, confirm no change, checkpoint), never repeats it, and one more apply converges |

Every scenario ends by scanning the workspace and everything the scenario printed (errors, plans,
debug output) for the suite's canary secrets. A scenario that does not apply to a kind is
**skipped with its reason**, never silently: `create_only` fields, an authoritative collection, a
field with one possible value.

## The suite is tested

`tests/meta.rs` breaks a spec in the ways specs go wrong and requires the suite to notice: a
wrong identity pointer, a field mapped to a name the contract does not have, a write group on the
wrong operation. A synthetic kind with a `create_only` field passes the replacement scenarios, and
the secret scan has a test that it can fail.

## What it does not replace

The same scenarios must run against a real Dokploy (the live contract suite, ADR 0015), because
the simulator is only as true as the fixtures it copies: a scenario whose result differs between the
two is a bug in the simulator or the spec. The scenarios that need two resources (dependency
ordering) or an importer (import then plan is empty) are not generated yet.

## Nested kinds

A kind under a parent (`redirect` under `application`) is exercised with its ancestors seeded: the
suite puts a project, environment, and application in the simulator and records them in state as
if an earlier apply had made them, then runs the same scenarios on the kind. It exercises the kind,
not the kinds above it, which have their own specs and arrive with the project kinds. The document
is a project document with the kind nested under them, and a scenario that removes the kind keeps
the ancestors.
