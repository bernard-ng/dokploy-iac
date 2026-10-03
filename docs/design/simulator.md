# Simulator

`dokploy-sim` is an in-memory Dokploy
([ADR 0015](../decisions/0015-testing-simulator-and-live-contracts.md)). It implements
`Transport`, so the engine runs against it in-process with no socket, and it is a development
dependency that is never shipped.

```rust
let sim = Sim::new(Arc::new(specs)).with_fixtures(Path::new("fixtures/api/live/v0.30.6"));
let engine = Engine::new(specs, &sim)?;
sim.seed("tag", &json!({ "name": "blue" }));       // exists remotely, unmanaged
sim.inject(Fault::new("tag.create", FaultKind::DropAfter));
```

## It knows no kind

The simulator reads the same specs as the engine and answers exactly the operations they
reference (`create`, `update`, each write group's operation, `remove`, the list, the direct
read). Adding a flat kind adds a spec file; nothing in the simulator changes.

| Operation | Behavior |
|-----------|----------|
| every request | checked against the generated request contract of its operation (`dokploy_api::request_contract`): an unknown or missing field is a 400 |
| create | stores an object under a generated id; fields the request did not carry are `null`, server-owned fields (`ledger.readonly`) are filled; a missing parent is a 404 |
| update | merges the fields the request carries (Dokploy's updates are patches; this is captured) |
| remove | deletes the object and what is attached below it, as the database cascade does |
| list | the collection; a query parameter that names a field filters by it (`byProjectId`); an object can be hidden to model a role filter |
| one | the object, with the child collections the specs say are embedded in it (`application.one` carries `redirects`) |

## Response shapes come from fixtures

Dokploy's OpenAPI describes every response as empty, so shapes are copied from the recorded
fixtures (`fixtures/api/live/<version>`, ADR 0007). By the capture scripts' naming
(`<resource>-<action>.owner.json`, with the resource singular) a fixture is a template: an object
template says which keys a response has, each value coming from the stored object (so
`registry.one` omits the password and `registry.all` lists it, as live); `true` and
`{"success": true}` are returned as they are. A kind without fixtures gets the stored object, and
`true` when its create does not return an identity.

## Faults

| Fault | What happens |
|-------|--------------|
| `DropBefore` | nothing is applied; a mutation reports an unknown outcome, a read a request failure |
| `DropAfter` | the change is applied and the response is lost (unknown outcome) |
| `Reject { status }` | a definitive rejection, nothing applied |
| `RejectAfter { status }` | the change is applied and a failure is reported anyway: a registry whose `docker login` Dokploy rejects still saves the change |
| `Unavailable` | a read is answered with a 503 |

A fault fires once; `on_call(n)` targets the nth call of that operation.

## What it does not do

It does not replace live acceptance: the same scenarios run against a real Dokploy in CI, and any
scenario whose result differs fails and is fixed in the simulator or the spec. It does not
enforce unique names (Dokploy does for tags and not for registries; the engine's collision
handling is what the conformance suite tests), and it does not model deploy, logs, or background
work.
