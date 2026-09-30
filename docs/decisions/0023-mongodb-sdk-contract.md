# ADR 0023: MongoDB SDK mutation contract

## Status

Accepted. Extends Phase 8 resource breadth.

## Context

Dokploy's MongoDB transport requires a database user and password at creation,
supports optional replica-set configuration, and rotates its single database
password through a dedicated operation. Runtime detail responses expose the
plaintext password and other fields that reconciliation callers must not
retain.

## Decision

The public SDK exposes a strong `MongoId`, safe detail and search models, and a
bounded environment-scoped collection. Unknown response fields are tolerated
while the database password, environment variables, mounts, and other unowned
runtime data are absent from safe models.

`CreateMongo` requires a zeroizing password and offers explicit replica-set
selection. `UpdateMongo` owns the database username and replica-set setting.
Password rotation is a separate write-only `ChangeMongoPassword` operation.
Secret-bearing request models validate the pinned character contract and
redact their debug output.

The adapter uses pinned generated route and request metadata with narrow owned
mutation bodies. Structured remote rejections remain available to callers,
and transport failures after dispatch remain outcome-unknown. Environment
search follows stable authoritative totals and rejects incomplete, oversized,
or contradictory pagination.

## Consequences

- Callers cannot obtain the MongoDB password through safe reads or confuse a
  MongoDB identity with another resource type.
- Replica-set intent is explicit on create and update without exposing broader
  runtime configuration.
- Password rotation remains reviewable and isolated from non-secret updates.
- Live contract capture never deploys the database. Dokploy `v0.30.6` therefore
  returns a structured idle-database rejection for password changes; mock
  transport tests cover the successful wire contract.
