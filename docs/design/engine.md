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
| **discover** | Reads the minimum: one list per kind, and a direct read only for a resource that matched. A kind nobody mentions is not read. A failed read makes exactly its addresses unavailable. A direct read that disagrees with the collection is unavailable, not guessed. A collection that lists one identity twice, holds more than 10,000 items, or names another parent's items contradicts itself, and so does a direct read that names another parent: nothing is concluded from them. Parents are read before children; a child's collection is read once per parent, either **scoped** by it (`environment.byProjectId?projectId=…`) or **embedded** in the parent's direct read (`application.one` carries `redirects`), which costs no extra request. A child of a missing parent is missing, and of an unavailable parent is unavailable |
| **project** | Tolerant and presence-aware: a field not returned is "not returned", `null` is a value, a value outside its type or enum is "invalid response" for that property, and secret and write-only values are dropped at the boundary |
| **plan** | The planner, unchanged: deterministic, value-free |
| **apply** | Executes the plan under the writer lock, one journaled step per change (below) |

Fingerprints are an HMAC-SHA-256 over instance, address, path, and value, checked against an
independently computed vector, so a change to the framing cannot pass by agreeing with itself.

## Apply

`Engine::apply(&compiled, &store, approve)` takes the writer lock, reads the state, makes the plan
from it, and lets the caller decline it. Nothing changes until `approve` returns `true`, and a plan
that is incomplete or has a blocking diagnostic is never applied. A **preflight** then builds every
request without sending it, so a change the executor cannot make is reported before the first byte
goes out.

For each change, in the planner's dependency-safe order:

| Change | Requests |
|--------|----------|
| create | one `create` carrying every managed property the create operation accepts and the attachment to the parent; a field the contract requires and the document omits is sent as `null` if the spec allows it; the new identity is learned as the spec says (`from_response`, or `diff_collection` over the list read before and after). What the operation does not accept is written right after by the spec's write groups, as a second journaled step (an update from the state the create recorded), so an interruption between the two leaves a resource that state describes exactly and the next plan finishes |
| update | one request per write group that has a change, in the spec's order: `partial` sends the id and the changed fields, `full` re-sends the group from a fresh read overlaid with the changes (a secret in it must be declared in the document) |
| remove | one `remove`; a 404 counts as removed; Dokploy acknowledging it while the resource can still be read fails the step and keeps the resource tracked |
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

## Recovery

`Engine::recover(&compiled, &store, approve)` settles the one interrupted apply of a document. It
never repeats a mutation: it reads the resource again, compares it with the state the open step was
meant to produce, and decides.

| Open step | The remote shows | Recovery |
|-----------|------------------|----------|
| create | the resource, matching the target (a secret that cannot be read back is the one thing not compared) | adopts it: records its identity and checkpoints |
| create | nothing | confirms no change |
| update | the target | checkpoints the success |
| update | the previous state | confirms no change |
| delete | gone | checkpoints the removal |
| delete | still there | confirms no change |
| rename, forget | (state only) | checkpoints |
| anything | something else, unreadable, or a secret rotation that cannot be proven | **a person decides**: nothing is recorded on a guess |

A secret cannot be read back, so an update that rotates one is never proven by observation alone.
`approve` sees the value-free action before anything is recorded.

## What it does not do yet

- **Deploy.**
- **Writing composite values** (unions, structs, keyed collections, `env`, `file`): refused in the
  preflight with a message naming the property. They arrive with the project kinds (M3).
- **Create-before-delete replacement**, moving to another parent, and a rename combined with a
  change: refused in the preflight.
- **Paged collections.** `application.search` answers `{items, total}`; reading pages arrives
  with the project kinds (M3). Until then the prototype `application` spec has no collection read,
  so an application is found by its recorded identity only and one that is not recorded is
  unavailable, never guessed to be absent.
- **Selectors** (`server`, `registry`): compiled, but not resolved, so a desired selector blocks the
  plan with `UnresolvedExternalSelector`.
- **Composite values on the remote side**: unions, structs, and keyed collections are compiled but
  read back as "not returned", which blocks planning for a property the document manages.
- **Secrets inside a union, struct, or collection** are refused at compile time.

## Cost

The `registry` kind, which has no first-engine code, plans from its spec alone: create, converge,
update, drift, secret rotation, collision, and the failure modes are all tests of the generic
engine, not of the kind. Adding a flat settings kind adds a spec file.
