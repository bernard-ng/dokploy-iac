# Architecture decision records

The decisions behind the v2 engine. Numbering restarted at 0001 when the project
re-centred on its original goal (see [ADR 0001](0001-product-goal-and-scope.md)).
The 50 records of the first engine remain in git history at commit `96cab73`
(`docs/decisions/`); they are superseded, not deleted knowledge, and individual
ones are cited where a v2 decision keeps their conclusion.

Status values: **Proposed** (written, awaiting review), **Accepted**, **Superseded**.
ADRs 0001, 0005, 0006, 0010, 0012, and 0016 are Accepted; the rest follow from them
and are confirmed by the milestone that first implements them.

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
