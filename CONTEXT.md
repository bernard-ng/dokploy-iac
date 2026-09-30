# Dokploy Infrastructure Reconciliation

This context describes how declared resources relate to managed Dokploy
resources while identity, ordering, and remote change semantics remain explicit.

## Language

**Containment**:
The single direct parent that determines where a nested resource belongs.
_Avoid_: Parent dependency

**Dependency**:
An ordering relationship requiring another resource to converge first; it does
not imply ownership or containment.
_Avoid_: Parent

**Reparent**:
A change of containment that preserves the resource's managed identity.
_Avoid_: Move

**Replacement**:
Convergence by exchanging one managed physical identity for another instead of
mutating the existing resource in place.
_Avoid_: Recreate

**Mutation Contract**:
The proven preconditions, remote effects, failure outcomes, and recovery rules
for changing a resource.
_Avoid_: Endpoint support

**Atomic Property**:
A structured owned value that is validated, planned, and checkpointed as one
indivisible property because its fields are not independently meaningful.
_Avoid_: Property group

**External Selector**:
A stable human reference resolved against fresh remote infrastructure whose
lifecycle is not owned by the workspace. Zero or multiple exact matches block
planning.
_Avoid_: Imported external ID
