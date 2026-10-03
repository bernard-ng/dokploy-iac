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

## Moves and removals

`moves` and `removed` (see [`document-format.md`](document-format.md)) are resolved when the plan is
made, because what an address suffix means depends on state: `Compiled::desired_for(state)` turns
them into the planner's directives. Discovery treats the resource being moved as the one it is:
the new address is not a collision with the identity it came from. A move that changes nothing else
is a state-only step; one that also changes properties is a state-only move first (the resource as
state has it, at its new address) and then an update of that resource, each journaled. A resource
that holds a secret re-sends it after a move, because its receipt is bound to the address.

## Structs and environment blocks

A `struct` with `granularity: field` is a grouping in the document only: each member is a property
(`resources.memory_limit`) and a key of its own in the request and the response (`memoryLimit`).
Reading, writing, and the write groups all work per member; a `full` group re-sends the members it
did not change from a fresh read.

An `env` field is one text of `KEY=VALUE` lines at Dokploy and one property per variable in the
document, each a secret (a receipt in state, never a value in a plan). Discovery observes the
variables the document or state owns, and each is there or not (its value is never read back); a
collection the document owns as a whole (`null` to clear, `{}` to declare empty) is observed as a
whole instead. A write reads the text fresh, sets the variables the document owns in place or at the
end, and leaves every other line exactly as it was. A recovered update that only added variables is
proven by their presence; a rotated value is not provable and is left to a person.

## Unions

A `union(tag)` is planned per member ([ADR 0018](../decisions/0018-several-parents-and-per-member-unions.md)):
the tag (`source.type`) and each member of each arm (`source.github.owner`,
`source.docker.password`) are properties. A member the document leaves out is unmanaged, and a
secret inside an arm is an ordinary secret property with a receipt. Compiling a document keeps a
member only for the arm its tag names.

Reading a union back reads the tag from the response (`sourceType`) and a member only while Dokploy
holds that arm: the members of any other arm are known to be absent.

Writing is a `by_variant` group: one request per apply, to the operation of the arm the document
names (`application.saveDockerProvider`), whichever of the tag and members changed. The request
carries every member the arm's operation takes, `full`:

1. a member the document owns is sent as it says;
2. a member it leaves out keeps what Dokploy holds, from a fresh read, while the arm does not
   change;
3. otherwise, after a change of arm, the member's `fallback` (`build_path: "/"`,
   `trigger_type: push`), then `null` where the operation takes one, and otherwise the apply is
   refused naming the member;
4. a secret the document leaves out is not sent, and an operation that requires one is refused
   with the secret's name.

The tag is implied by the operation, not sent in it (`derived` in the ledger): the simulator makes
the saved arm the held one, as Dokploy does. Two other shapes exist:

- **An operation shared by several arms** (`application.saveBuildType` writes every build type): the
  tag is sent in the body, and the columns of the other arms that the operation takes anyway keep
  what Dokploy holds, or their `fallback`, or `null` where the operation allows it.
- **A union written by the kind's own update** (`libsql.update` takes `sqldNode` and
  `sqldPrimaryUrl`; `compose.update` takes the whole `source`): the union is a field of an ordinary
  write group, expanded into its tag and the members of every arm like a struct, each under the key
  of its own. A member of an arm that a create requires and the document does not give is sent
  empty (`sqldPrimaryUrl: null` for a primary).

A struct member can itself be an environment block (`preview.environment`, `preview.build_args`,
`preview.build_secrets`): it is planned and written like the kind's own, under the path of the
member, and the entries are named below that path.

Fixed fields: an operation may state what every call sends as written (`send: { deleteVolumes: false }`
on a remove or a create) and, for the update, what the parent's kind fixes (`databaseType: postgres`
on `backup.update`), through the same `attach_by_parent` the create uses.

## Selectors

A `selector(kind)` field holds `{ name }`, or `{ local: true }` for a server, and Dokploy holds the
target's id. Discovery reads the collection of each target kind its subjects select from (one list
per kind, only for kinds that have a spec with a top-level collection), and that one read does
three jobs: it turns the id Dokploy returned into the name the document uses, it resolves the
document's name for the planner (`Resolved(id)`, `Unmatched`, `Ambiguous`, or `Unavailable`; a name
that is not exactly one resource blocks the plan with `DOKPLAN019` and a typed reason), and apply
reads it again before the first write so the id sent is the one that exists now. No id at Dokploy
reads as `{ local: true }` for a server and as absent for anything else; a document's `null` for a
server selector means the same and is compiled as `{ local: true }`. A kind with no spec yet (`server`
until its settings milestone) is not read, so its selectors stay unresolved and block the plan.

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
- **Writing files**: refused in the preflight with a message naming the property; a file waits for
  the secrets milestone (M4). Structs planned per member, `env` blocks, and unions are written
  (above).
- **Create-before-delete replacement**, moving to another parent, and a rename combined with a
  change: refused in the preflight.
- **Paged collections.** `application.search` and its siblings answer `{items, total}` and are not
  read: a project kind is found in the collection its environment's direct read embeds. A kind with
  neither is found by its recorded identity only and one that is not recorded is unavailable, never
  guessed to be absent.
- **Plain `map` collections** read as "not returned", which blocks planning for a property the
  document manages.
- **Secrets inside a struct member or a plain collection** are refused at compile time. A secret
  inside a union arm works.
- **Recovering a lost response to a union write that carries a scalar secret**: the secret cannot be
  read back, so everything observable can agree and recovery still does not call it a success; a
  person decides.

## Cost

The `registry` kind, which has no first-engine code, plans from its spec alone: create, converge,
update, drift, secret rotation, collision, and the failure modes are all tests of the generic
engine, not of the kind. Adding a flat settings kind adds a spec file.
