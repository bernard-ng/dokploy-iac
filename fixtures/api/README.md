# API fixtures

Sanitized responses from a real Dokploy instance belong here. Phase 0 requires
fixtures for:

- `application.create`
- `application.one`
- `project.all`
- `project.one`
- `postgres.one`

Fixtures must not contain API keys, credentials, domain names, repository URLs,
environment values, or other deployment secrets. Synthetic fixtures may be
used for unit tests, but they must be clearly named and cannot serve as proof of
the live API contract.

Live fixtures are grouped by tested Dokploy version under `live/` and include
capture metadata. Model tests use these sanitized runtime contracts directly.

Phase 8 contract captures additionally cover MySQL, MariaDB, MongoDB, LibSQL,
and raw Compose create/read/update/delete behavior. Compose fixture publication
redacts both the opaque Compose document and the Dokploy refresh token. Its
capture metadata records that no deployment occurred and that detail, search,
and project-topology reads all proved cleanup.
