# Architecture decision records

These records explain the structure and behavior of the current engine. All
decisions listed here are accepted and implemented in the beta. The superseded
records for the first engine remain available in Git history at commit `96cab73`.

| ADR | Decision |
|-----|----------|
| [0001](0001-product-goal-and-scope.md) | Product goal, scope, and non-goals |
| [0002](0002-kind-specs-are-the-source-of-truth.md) | Kind specs are the single source of truth |
| [0003](0003-layered-architecture-and-kept-kernel.md) | Layered architecture and the kept kernel |
| [0004](0004-property-model-and-mutability.md) | Property model, field types, and mutability |
| [0005](0005-document-model-and-workspace.md) | Document model v2 and workspace layout |
| [0006](0006-addresses-identity-and-naming.md) | Addresses, identity, and naming |
| [0007](0007-remote-reads-and-projection.md) | Remote reads and tolerant projection |
| [0008](0008-mutations-write-groups-and-recovery.md) | Mutations, write groups, and recovery |
| [0009](0009-state-v5-and-scopes.md) | State format 5 and scopes |
| [0010](0010-secrets-environment-and-content.md) | Secrets, environment variables, and content files |
| [0011](0011-import-and-round-trip-contract.md) | Import and the round-trip contract |
| [0012](0012-deployment-model.md) | Deployment model |
| [0013](0013-instance-settings-and-bootstrap.md) | Instance settings and bootstrap |
| [0014](0014-compatibility-and-dokploy-versions.md) | Compatibility and Dokploy versions |
| [0015](0015-testing-simulator-and-live-contracts.md) | Testing: simulator, conformance, live contracts |
| [0016](0016-replace-the-engine-keep-the-kernel.md) | Replace the engine, keep the kernel |
| [0017](0017-cli-surface.md) | CLI surface |
| [0018](0018-several-parents-and-per-member-unions.md) | Kinds with several parents, and unions planned per member |
| [0019](0019-sets-of-selectors-relations-and-per-parent-operations.md) | Sets of selectors, relations, and per-parent operations |
