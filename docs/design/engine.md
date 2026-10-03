# Engine

`dokploy-engine` turns a document into a plan
([ADR 0003](../decisions/0003-layered-architecture-and-kept-kernel.md),
[ADR 0007](../decisions/0007-remote-reads-and-projection.md)). It contains no knowledge of any
kind: what a kind means comes from its spec, the document from `dokploy-model`, the plan from the
pure planner in `dokploy-core`, and every byte sent to Dokploy goes through a `Transport`
(`dokploy-sdk`), so the same code runs over HTTP or in memory.

```rust
let engine = Engine::new(specs, transport)?;           // registers the spec kinds
let document = engine.parse(text)?;                    // dokploy-model: validated, with positions
let compiled = engine.compile(&document, &fingerprinter, &secrets)?;
let plan = engine.plan(&compiled, state.as_ref()).await?;
```

## What exists

| Stage | Does |
|-------|------|
| **compile** | Gives every resource its hierarchical address and containment; turns field values into canonical comparable values; reads each secret source once, fingerprints it, and forgets it (`SecretReader`, `Fingerprinter`); applies `default: key`; resolves `depends_on` by address suffix; computes the digest of the canonical text |
| **discover** | Reads the minimum: one list per kind, and a direct read only for a resource that matched. A kind nobody mentions is not read. A failed read makes exactly its addresses unavailable. A direct read that disagrees with the collection is unavailable, not guessed |
| **project** | Tolerant and presence-aware: a field not returned is "not returned", `null` is a value, a value outside its type or enum is "invalid response" for that property, and secret and write-only values are dropped at the boundary |
| **plan** | The planner, unchanged: deterministic, value-free |
| **apply** | Executes the plan under the writer lock, one journaled step per change (below) |

Fingerprints use the first engine's construction (HMAC-SHA-256 over instance, address, path, and
value), checked against its canonical test vector, so receipts stay comparable.

## Apply

`Engine::apply(&compiled, &store, approve)` takes the writer lock, reads the state, makes the plan
from it, and lets the caller decline it. Nothing changes until `approve` returns `true`, and a plan
that is incomplete or has a blocking diagnostic is never applied. A **preflight** then builds every
request without sending it, so a change the executor cannot make is reported before the first byte
goes out.

For each change, in the planner's dependency-safe order:

| Change | Requests |
|--------|----------|
| create | one `create` carrying every managed property and the attachment to the parent; a field the contract requires and the document omits is sent as `null` if the spec allows it; the new identity is learned as the spec says (`from_response`, or `diff_collection` over the list read before and after) |
| update | one request per write group that has a change, in the spec's order: `partial` sends the id and the changed fields, `full` re-sends the group from a fresh read overlaid with the changes (a secret in it must be declared in the document) |
| remove | one `remove`; a 404 counts as removed |
| replace | remove, then create (the order the planner proved) |
| rename, adopt, forget | state only |

Every step is written to the journal before it is sent, with the exact state it expects, and the
state is checkpointed when it succeeds. A mutation is sent **once**. If the transport cannot say
whether it happened, or the identity of a created resource cannot be learned, the step stays open
and `recover` decides from fresh evidence. A definitive rejection fails the step and stops the run
(the journal is terminal after a failure, as in the first engine). After a step succeeds the
resource is read back and every public property just written must read as written; a mismatch is
reported after it is recorded, so the next plan shows it as drift.

Secret values live in zeroizing memory inside `Compiled`, only so an apply can send them; they are
never printed, planned, journaled, or checkpointed (state holds receipts).

## What it does not do yet

- **Recovery and deploy.** Recovery is the next slice.
- **Writing composite values** (unions, structs, keyed collections, `env`, `file`): refused in the
  preflight with a message naming the property. They arrive with the project kinds (M3).
- **Follow-up updates after a create** for a field the create operation does not accept: refused
  in the preflight for now.
- **Create-before-delete replacement**, moving to another parent, and a rename combined with a
  change: refused in the preflight.
- **Nested kinds** (M3). Discovery of a kind with a parent, an embedded collection, or a scoped
  list returns `UnsupportedDiscovery` instead of guessing.
- **Selectors** (`server`, `registry`): compiled, but not resolved, so a desired selector blocks the
  plan with `UnresolvedExternalSelector`.
- **Composite values on the remote side**: unions, structs, and keyed collections are compiled but
  read back as "not returned", which blocks planning for a property the document manages.
- **Secrets inside a union, struct, or collection** are refused at compile time.
- **The CLI.** The thin shell keeps using the first engine until its kinds are ported (ADR 0016).

## Cost

The `registry` kind, which has no first-engine code, plans from its spec alone: create, converge,
update, drift, secret rotation, collision, and the failure modes are all tests of the generic
engine, not of the kind. Adding a flat settings kind adds a spec file.
