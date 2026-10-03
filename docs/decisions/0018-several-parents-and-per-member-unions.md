# ADR 0018: Kinds with several parents, and unions planned per member

## Status

Accepted (2026-10-03), on the owner's delegation ("i trust your judgment") after the two options
below were put to them. It amends ADR 0004 (what a union is as a property) and the grammar of
[`spec-format.md`](../design/spec-format.md).

## Context

Building the project kinds (milestone M3) met two shapes the first grammar did not have.

1. **A kind that lives under several parents.** A mount belongs to an application, a Compose, or any
   of six databases; a domain to an application or a Compose; a backup to a database or a Compose;
   a schedule, a volume backup, and a patch to an application or a Compose. `parent:` named one kind.
2. **A union that holds a secret, or whose members are optional.** The application `source` is one of
   github, gitlab, bitbucket, gitea, git, or docker, written by a different operation each. ADR 0004
   plans a union as one atomic value. That cannot hold a secret (the registry password of a Docker
   image), and it contradicts "an omitted field is unmanaged": the vision documents leave
   `build_path`, `trigger_type`, and `watch_paths` out of some applications.

## Decision

### Several parents

`parent` takes a kind or a list of kinds: `parent: [application, compose, postgres]`.

- It is one kind, with one address prefix (`mount.data`) under whichever parent it is written in
  (`application.api/mount.data`), one section name, and one spec. Each parent lists it under
  `children`; the registry checks both directions.
- State registers a kind with its list of containment parents, and a resource is valid under any of
  them. A kind with no parent is a document root.
- What differs by parent is stated in the spec, not coded:
  - `api.create.attach_by_parent: { <parent kind>: { field: parent_id | <literal> } }` is laid over
    `attach` for that parent, so a mount is created with `serviceId` and a `serviceType` of the
    parent's kind, and a domain with `applicationId` or `composeId` and a `domainType`. A value
    other than `parent_id` is sent as written.
  - `api.parent_field: { <parent kind>: <column> }` names the response field that holds the parent's
    id when it is not the request field (a mount is answered with `applicationId`, `postgresId`).
  - `embedded_in` may leave out `parent_op`: it is then the parent's direct read.
- A collection scoped by a parent's id (`scope`) serves one parent kind only.
- The conformance suite runs a kind under each of its parents (`mount@postgres`).

The alternative, one near-identical kind per parent, was refused: dozens of copies of the same
fields, and addresses (`application_mount.data`) that say the parent twice.

### Unions per member

A `union(tag)` is planned like a `struct` with `granularity: field` whose members depend on the tag.

- The tag and each member of the active arm are properties: `source.type`, `source.owner`,
  `source.password`. An omitted member is unmanaged, and a secret member is an ordinary secret
  property with a receipt.
- A member is legal in a document only for the arm the tag names; the document is checked, and a
  path of a member of another arm is not a property of the resource as written.
- Changing the tag is a change of arm: the new arm's required members must be present, and the
  write is the arm's operation (`by_variant`), sent with the members the arm needs.
- Reading a union back reads the tag from the response and the members of that arm.

This is implemented after the several-parents change; until then `source` and `build` stay out of
the specs, and the vision ratchet records them.

## Consequences

- Specs for the leaf kinds stay one file each, and conformance covers every parent a kind has.
- A parent must list the child under `children` even though the child already names the parent;
  the lint reports whichever side is missing.
- ADR 0004's rule that a union is one value is superseded for unions; the rest of the property
  model is unchanged.
