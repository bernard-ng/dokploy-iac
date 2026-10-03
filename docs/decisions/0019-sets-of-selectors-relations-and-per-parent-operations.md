# ADR 0019: Sets of selectors, relations, and per-parent operations

## Status

Proposed (2026-10-03). Decided while finishing milestone M3 on the owner's instruction to finish it,
and put to review with the pull request that carries it. It amends [ADR 0018](0018-several-parents-and-per-member-unions.md)
(a scoped list may serve several parents) and the grammar of [`spec-format.md`](../design/spec-format.md).

## Context

The last project kinds and fields met shapes the grammar still lacked.

1. **A service attaches to several networks, a project has several tags.** Each is a set of resources
   outside the document, written by name and held by id. ADR 0004 has one selector per property, so
   the planner, which resolves a selector per property, could not plan a set.
2. **A relation Dokploy changes one member at a time.** A tag is added to a project with
   `tag.assignToProject` and removed with `tag.removeFromProject`; no field of `project.update`
   carries it, and the direct read holds it as an array of objects (`projectTags`).
3. **A per-service array.** A Compose stack holds `serviceNetworks`, one element per service with its
   networks and a flag.
4. **One operation for several parents, or with fixed fields.** `schedule.list`,
   `volumeBackups.list`, and `patch.byEntityId` serve every parent and are told which kind the id
   names; `backup.update` requires a `databaseType` that depends on the parent; `compose.delete`
   requires `deleteVolumes`.

## Decision

### A set of selectors is planned per member

`type: "set<selector(network)>"` with `granularity: key` is a collection like an `env` block, whose
entries are the members: `networks.backend`. Each entry is a selector property, so the planner
resolves, compares, and reports each one, and a member that names nothing blocks the plan with the
existing diagnostic.

- **Reading.** Dokploy holds an array of ids. Each id is turned into the name of the resource it
  belongs to through the collection of the target kind, which discovery already reads for selectors.
  An id that cannot be named is not an absence of any member, and a set that holds one is never taken
  for empty.
- **What the document owns.** A document that names a set owns the members it names, and the members
  it used to name: one it stops naming is cleared, which detaches it. An attachment made elsewhere is
  not the document's, and stays. `[]` owns the whole set and detaches every member. A field left out is
  unmanaged.
- **Writing.** One array, assembled from what Dokploy holds now: the ids it holds, less the members
  the document cleared, plus the ones it names. A member that was cleared is not stored.

### A relation is written one member at a time

A keyed set of selectors may declare `membership: { add: { op, member }, remove: { op, member } }`.
Such a field is in no write group. Each added member is one request with the kind's id and the
member's id, each removed one is another, in the step that changes the resource, and `[]` removes every
member Dokploy holds. The `api` of the field says where the held ids are: `/projectTags/*/tagId`, the id
inside each object of an array.

The shape of an assigned tag has never been captured (`projectTags` has always been empty), so a read
that does not look like that is an invalid response, which blocks planning for the field alone. Nothing
is written on a guess, and the live suite verifies the shape.

### A map from a key to a set of selectors

`type: "map<text, set<selector(network)>>"` is a collection whose entries are `key.member`
(`service_networks.api.backend`). Its `api` is a pointer to a keyed array,
`/serviceNetworks/*[serviceName=$key]/networkIds`: the element whose `serviceName` is the key, and the
array of ids it holds. The write keeps every element and every other key of an element the document does
not own (the per-service `detachDokployNetwork` flag stays as Dokploy holds it, and a service the
document adds gets the field's `fallback` as a template), and a key cannot hold a dot.

### Operations

- `send: { field: value }` on an operation sends typed fixed fields on every call of a create or a
  remove (`deleteVolumes: false`, which keeps the volumes of a removed stack).
- `attach_by_parent` on the **update** operation fixes fields for the parent's kind, as on the create
  (`databaseType: postgres`).
- A scoped list read carries `query_by_parent: { <parent kind>: { param: value } }`, so one list serves
  several parents (`scheduleType: application`). This amends ADR 0018, which said a scoped collection
  serves one parent kind only.

## Consequences

- `networks` is one line in each of the application and the six databases, and `tags` one in the
  project; a new set is a spec entry.
- The conformance suite generates, writes, changes, and tampers with sets; relations and maps of sets
  are covered by engine tests, since only the project and the Compose stack have them.
- The simulator models a relation as an array of objects and a keyed array as stored, which is what
  the spec assumes. It cannot prove the assumption: the live suite has to.
- A set a document names is not the whole truth about Dokploy: an attachment made elsewhere is left
  alone, which is what a document that owns what it names should do, and `[]` is how to own all of it.
- A per-service flag is not managed. Managing it needs per-service members in a map entry, which this
  grammar does not have.
