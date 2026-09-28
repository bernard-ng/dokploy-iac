# Architecture invariants

These rules define the safety boundary of the Dokploy CLI and IaC engine.

1. Every reconciliation reads fresh Dokploy state.
2. There is no persistent API cache.
3. Generated API files are never edited manually.
4. IaC-critical read models do not blindly trust incomplete OpenAPI responses.
5. The planner cannot mutate remote infrastructure.
6. The executor follows a plan and does not invent changes.
7. Only resources recorded as managed can be destroyed.
8. Omitted configuration fields are unmanaged.
9. A null field and an omitted field have different meanings.
10. Remote IDs establish physical identity after import or creation.
11. Logical addresses establish configuration identity.
12. Resource moves preserve remote identity.
13. State lineage and serial protect against stale state.
14. State is bound to one Dokploy instance.
15. Every successful mutation is journaled and checkpointed.
16. An uncertain non-idempotent mutation is never blindly retried.
17. Protected resources cannot be destroyed.
18. Secrets never appear in state, plans, logs, diagnostics, or snapshots.
19. Partial failure does not trigger automatic infrastructure rollback.
20. Plan output is deterministic.
21. Unmanaged Dokploy resources are untouchable.
22. A successful apply followed immediately by a plan converges to no changes.
