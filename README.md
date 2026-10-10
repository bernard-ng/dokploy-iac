# Dokploy IaC

Manage [Dokploy](https://dokploy.com/) infrastructure from reviewable YAML.
Dokploy IaC can adopt an existing instance, show proposed changes, reconcile its
configuration, and deploy its workloads. The workspace can then rebuild the same
setup on a fresh Dokploy installation.

> [!IMPORTANT]
> This project is in beta. Dokploy 0.30.6 and 0.30.7 are supported. SSO,
> licensing, SCIM, user management, roles, billing, and whitelabeling are outside
> the current scope.

## Use cases

- **Bring an existing instance under version control.** Import settings, projects,
  services, domains, environment variables, and content into one workspace.
- **Review infrastructure changes before applying them.** `dokploy plan` produces
  a deterministic preview without secret values, suitable for local review or CI.
- **Rebuild or migrate an instance.** Apply the same workspace to a fresh Dokploy
  installation and deploy the resulting services.
- **Detect and repair drift.** Compare YAML and local state with a fresh read from
  Dokploy, then restore the declared configuration.
- **Manage settings and projects together.** Reconcile instance settings first,
  then projects and deployments in dependency order.

## Get started

Install the CLI from the repository:

```bash
cargo install --git https://github.com/bernard-ng/dokploy-iac --locked dokploy-cli
```

Set the connection for the current shell and check it:

```bash
export DOKPLOY_URL="https://dokploy.example.com"
export DOKPLOY_API_KEY="your-api-key"
dokploy doctor
```

In an empty directory, adopt the current instance:

```bash
dokploy import all
```

The import reads the instance in visible steps, writes a canonical workspace,
and proves that the first plan is empty. Commit the public configuration and keep
the generated secret files out of Git.

Use the normal review and apply loop from the workspace root:

```bash
dokploy plan
dokploy apply --deploy
```

`apply` asks for confirmation before making changes. Use `dokploy deploy` when
configuration is already converged and only deployment is required.

## Workspace

The root contains one public entry point:

```text
dokploy.workspace.yaml
.dokploy/
  settings.yaml
  projects/
    shop.yaml
  files/
  secrets/
  state/
```

`dokploy.workspace.yaml` lists the settings and project documents in apply order:

```yaml
version: 1
documents:
  - .dokploy/settings.yaml
  - .dokploy/projects/shop.yaml
```

To define a new workspace instead of importing one, create the documents listed
by the manifest. A small settings document can define shared tags and a container
registry:

```yaml
# .dokploy/settings.yaml
version: 2
settings:
  tags:
    production:
      color: "#16a34a"
  registries:
    ghcr:
      type: cloud
      url: ghcr.io
      username: your-github-user
      password:
        env: GHCR_TOKEN
```

A project document can create an application from a Docker image, provide its
environment, attach the shared tag, expose a domain, and deploy after a relevant
configuration change:

```yaml
# .dokploy/projects/shop.yaml
version: 2
project:
  slug: shop
  description: Storefront services
  tags:
    - name: production
  environments:
    production:
      applications:
        web:
          source:
            type: docker
            image: nginx:1.27-alpine
          environment:
            APP_ENV: production
            SESSION_SECRET:
              secret:
                env: SHOP_SESSION_SECRET
          lifecycle:
            deploy: on_change
          domains:
            public:
              host: shop.example.com
              port: 80
              https: true
              certificate_type: letsencrypt
```

Set `GHCR_TOKEN` and `SHOP_SESSION_SECRET` in the environment that runs
`dokploy plan` or `dokploy apply`. Their values are fingerprinted for change
detection and are never copied into the YAML or state.

Files in `.dokploy/files/` hold managed content such as Compose files and
scripts. `.dokploy/secrets/` is git-ignored and stores imported secret values.
`.dokploy/state/` holds the state, journals, and locks that make reconciliation
and recovery safe.

## Safety model

- Every plan reads the current Dokploy state.
- Resources outside the workspace stay untouched.
- Secrets are referenced by source and never written to plans, state, logs, or
  diagnostics.
- Each remote change is journaled and verified before state is updated.
- Interrupted operations are settled with `dokploy recover` from fresh remote
  evidence.
- Applying configuration deploys workloads only when `--deploy` or
  `dokploy deploy` requests it.

## Documentation

- [CLI usage](docs/generated/cli-usage.md)
- [Resource reference](docs/generated/resource-reference.md)
- [Shell completions](docs/shell-completions.md)
- [Architecture](ARCHITECTURE.md) and [architecture decisions](docs/decisions/)
- [Continuous integration](docs/ci.md) and [release process](docs/releasing.md)

The CLI can also print its effective schema and resource coverage:

```bash
dokploy schema --document project
dokploy coverage
```
