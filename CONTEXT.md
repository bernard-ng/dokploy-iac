# Dokploy infrastructure as code: domain language

Terms used in the code, the ADRs, and the specs. Prefer these words; the "avoid"
entries name the ambiguous alternatives.

**Workspace**:
A directory holding documents, content files, secret files, and local state.
_Avoid_: project (a Dokploy project is one kind inside a workspace)

**Document**:
One YAML file with a single root, `project:` or `settings:`. Each has its own state.
_Avoid_: config file, manifest (the manifest only lists documents)

**Scope**:
Whether a document is `project` or `settings`. Decides where its state lives.

**Kind**:
A type of Dokploy resource (application, registry, domain).
_Avoid_: resource type

**Kind spec**:
The data file that describes a kind completely: identity, operations, fields, write
groups, children. The engine reads specs; it contains no per-kind logic.
_Avoid_: schema, adapter

**Hook**:
Rust code a spec names for the one step it cannot express. Needs a written reason.

**Field**:
A configurable property of a kind, with a type, a class, and a mutability.
_Avoid_: attribute, setting (a *setting* is an instance-wide kind)

**Property**:
A field at planning granularity, addressed by a path (`environment.LOG_LEVEL`).

**Mutability**:
How a field may change: in place, create-only, reparent, write-only, computed.

**Class**:
Whether a value is public, secret, or content.

**Source**:
Where a value comes from: a literal, `env`, `file`, or `vault`.

**Key**:
The name a resource has in its document. Unique among siblings of its kind.
**Address**: the path of keys from the document root. **Name**: the remote display
name. **App name**: Dokploy's Docker-level name.
_Avoid_: id (an id is the remote identity)

**Containment**:
The single direct parent that determines where a resource lives.
_Avoid_: parent dependency

**Dependency**:
An ordering requirement between resources. It implies neither ownership nor containment.

**Reference** (`ref`): an in-document link that creates a dependency.
**Selector** (`selector`): a name that is resolved against fresh remote state for
something outside the document (a server, a registry). Zero or several matches block
planning.

**Reparent**:
A change of containment that keeps the remote identity.
_Avoid_: move (a move is a rename of an address)

**Replacement**:
Convergence by exchanging one remote identity for another.
_Avoid_: recreate

**Write group**:
The fields one update operation carries. The executor sends one request per group.

**Atomic property**:
A structured value planned as one unit (a Swarm health check).

**Projection**:
The tolerant mapping from a raw response to a property map.

**Observation**:
What discovery learned about one resource: present, missing, or unavailable.

**Mutation contract**:
What the planner may assume about changing a kind: which changes are in place, which
replace, in what order.

**Coverage ledger**:
The CI check that every API field of a spec's operations is classified.

**Simulator**:
The in-memory Dokploy used by tests. Never shipped.

**Round trip**:
`import`, apply to a fresh instance, `import` again, and get an equal document.

**Adopt-only**:
A kind that exists on every instance (the web server); configuration updates it but
never creates or destroys it.

**Disruptive**:
A change that makes Dokploy reload Traefik or restart, and can cut the connection.
