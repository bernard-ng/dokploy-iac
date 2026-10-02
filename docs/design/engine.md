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

## What exists (the M1 skeleton)

| Stage | Does |
|-------|------|
| **compile** | Gives every resource its hierarchical address and containment; turns field values into canonical comparable values; reads each secret source once, fingerprints it, and forgets it (`SecretReader`, `Fingerprinter`); applies `default: key`; resolves `depends_on` by address suffix; computes the digest of the canonical text |
| **discover** | Reads the minimum: one list per kind, and a direct read only for a resource that matched. A kind nobody mentions is not read. A failed read makes exactly its addresses unavailable. A direct read that disagrees with the collection is unavailable, not guessed |
| **project** | Tolerant and presence-aware: a field not returned is "not returned", `null` is a value, a value outside its type or enum is "invalid response" for that property, and secret and write-only values are dropped at the boundary |
| **plan** | The planner, unchanged: deterministic, value-free |

Fingerprints use the first engine's construction (HMAC-SHA-256 over instance, address, path, and
value), checked against its canonical test vector, so receipts stay comparable.

## What it does not do yet

- **Apply, recovery, deploy** (M2 onward). Nothing here writes to Dokploy.
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
