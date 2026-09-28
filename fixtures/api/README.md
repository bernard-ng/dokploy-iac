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
