//! A path-routed double of one whole Dokploy project for the project-import suites.
//!
//! A [`World`] describes a project, its environments, every service kind, the
//! leaves below each service, and the external servers, registries, and
//! destinations. [`World::router`] turns that one description into every
//! endpoint the importer and the planner read, so the same fixture answers the
//! crawl and the first fresh plan. Secrets are embedded in every record the way
//! the real API embeds them, so any leak into configuration or state surfaces.
//!
//! Routes select by query fragment and are end-delimited, so ids may share prefixes.

#![allow(dead_code)]

use serde_json::{Value, json};

use super::{Reply, Router, ok};

/// A credential-shaped canary embedded in every secret-bearing field.
pub const SECRET_CANARY: &str = "world-secret-canary-never-leak";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Application,
    Compose,
    Postgres,
    MySql,
    MariaDb,
    Mongo,
    LibSql,
    Redis,
}

impl Kind {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Compose => "compose",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::MariaDb => "mariadb",
            Self::Mongo => "mongo",
            Self::LibSql => "libsql",
            Self::Redis => "redis",
        }
    }

    fn id_field(self) -> String {
        format!("{}Id", self.key())
    }

    /// Whether the planner lists this kind through a `.search` endpoint.
    const fn searchable(self) -> bool {
        !matches!(self, Self::LibSql)
    }

    const fn has_backups(self) -> bool {
        matches!(
            self,
            Self::Postgres | Self::MySql | Self::MariaDb | Self::Mongo | Self::LibSql
        )
    }
}

/// One service and the leaves below it.
pub struct Svc {
    kind: Kind,
    env: String,
    id: String,
    name: String,
    server: Option<String>,
    registries: [Option<String>; 3],
    build_server: Option<String>,
    domains: Vec<Value>,
    ports: Vec<Value>,
    redirects: Vec<Value>,
    security: Vec<Value>,
    mounts: Vec<Value>,
    schedules: Vec<Value>,
    backups: Vec<Value>,
    patch: Vec<(String, Value)>,
}

impl Svc {
    /// Overrides one top-level field of the direct `.one` record.
    pub fn patch(&mut self, field: &str, value: Value) -> &mut Self {
        self.patch.push((field.to_owned(), value));
        self
    }

    pub fn server(&mut self, id: &str) -> &mut Self {
        self.server = Some(id.to_owned());
        self
    }

    pub fn build_server(&mut self, id: &str) -> &mut Self {
        self.build_server = Some(id.to_owned());
        self
    }

    /// Sets the registry, build registry, and rollback registry of an application.
    pub fn registries(&mut self, registry: &str, build: &str, rollback: &str) -> &mut Self {
        self.registries = [
            Some(registry.to_owned()),
            Some(build.to_owned()),
            Some(rollback.to_owned()),
        ];
        self
    }

    pub fn domain(&mut self, id: &str, host: &str) -> &mut Self {
        self.domains
            .push(json!({"domainId": id, "host": host, "applicationId": self.id}));
        self
    }

    pub fn port(&mut self, id: &str, published: u16, target: u16) -> &mut Self {
        self.ports.push(json!({
            "portId": id, "applicationId": self.id, "publishedPort": published,
            "targetPort": target, "publishMode": "ingress", "protocol": "tcp",
        }));
        self
    }

    pub fn redirect(&mut self, id: &str, regex: &str, replacement: &str) -> &mut Self {
        self.redirects.push(json!({
            "redirectId": id, "applicationId": self.id, "regex": regex,
            "replacement": replacement, "permanent": true,
        }));
        self
    }

    pub fn security(&mut self, id: &str, username: &str) -> &mut Self {
        self.security.push(json!({
            "securityId": id, "applicationId": self.id, "username": username,
            "password": SECRET_CANARY,
        }));
        self
    }

    fn mount_base(&self, id: &str, kind: &str, path: &str) -> Value {
        json!({
            "mountId": id, "type": kind, "mountPath": path,
            "serviceType": self.kind.key(), self.kind.id_field(): self.id,
            "volumeName": null, "hostPath": null, "filePath": null,
        })
    }

    pub fn volume_mount(&mut self, id: &str, path: &str, volume: &str) -> &mut Self {
        let mut mount = self.mount_base(id, "volume", path);
        mount["volumeName"] = json!(volume);
        self.mounts.push(mount);
        self
    }

    /// Adds a Mount record exactly as given, for malformed-record cases.
    pub fn raw_mount(&mut self, mount: Value) -> &mut Self {
        self.mounts.push(mount);
        self
    }

    pub fn bind_mount(&mut self, id: &str, path: &str, host: &str) -> &mut Self {
        let mut mount = self.mount_base(id, "bind", path);
        mount["hostPath"] = json!(host);
        self.mounts.push(mount);
        self
    }

    pub fn file_mount(&mut self, id: &str, path: &str, file: &str) -> &mut Self {
        let mut mount = self.mount_base(id, "file", path);
        mount["filePath"] = json!(file);
        mount["content"] = json!(SECRET_CANARY);
        self.mounts.push(mount);
        self
    }

    /// Adds a Schedule. For a Compose, `service` names the Compose service.
    pub fn schedule(
        &mut self,
        id: &str,
        name: &str,
        cron: &str,
        service: Option<&str>,
    ) -> &mut Self {
        let compose = self.kind == Kind::Compose;
        self.schedules.push(json!({
            "scheduleId": id, "name": name, "description": null,
            "cronExpression": cron, "shellType": "bash",
            "scheduleType": if compose { "compose" } else { "application" },
            "command": SECRET_CANARY, "script": null,
            "applicationId": if compose { Value::Null } else { json!(self.id) },
            "composeId": if compose { json!(self.id) } else { Value::Null },
            "serverId": null, "serviceName": service, "enabled": true,
            "timezone": null, "appName": "runtime-name", "deployments": [],
        }));
        self
    }

    /// Sets the optional description and timezone of the last added Schedule.
    pub fn schedule_details(
        &mut self,
        description: Option<&str>,
        timezone: Option<&str>,
    ) -> &mut Self {
        let schedule = self.schedules.last_mut().expect("a Schedule was added");
        schedule["description"] = json!(description);
        schedule["timezone"] = json!(timezone);
        self
    }

    /// Sets the retention of the last added Backup (`None` clears it).
    pub fn backup_keep(&mut self, keep: Option<u32>) -> &mut Self {
        let backup = self.backups.last_mut().expect("a Backup was added");
        backup["keepLatestCount"] = json!(keep);
        self
    }

    /// Adds a Backup of a database. The destination is the destination id.
    pub fn backup(&mut self, id: &str, destination: &str, prefix: &str) -> &mut Self {
        self.backup_enabled(id, destination, prefix, Some(true))
    }

    /// Adds a Backup whose `enabled` flag may be absent (`null`).
    pub fn backup_enabled(
        &mut self,
        id: &str,
        destination: &str,
        prefix: &str,
        enabled: Option<bool>,
    ) -> &mut Self {
        assert!(self.kind.has_backups(), "only databases have Backups");
        let mut backup = json!({
            "backupId": id, "schedule": "0 3 * * *", "enabled": enabled,
            "database": "app", "prefix": prefix, "destinationId": destination,
            "keepLatestCount": 7, "includeEncryptionKey": false,
            "backupType": "database", "databaseType": self.kind.key(),
            "composeId": null, "postgresId": null, "mysqlId": null,
            "mariadbId": null, "mongoId": null, "libsqlId": null,
            "serviceName": null, "metadata": null,
            "destination": { "accessKey": SECRET_CANARY },
        });
        backup[self.kind.id_field()] = json!(self.id);
        self.backups.push(backup);
        self
    }

    fn summary(&self) -> Value {
        let mut summary = json!({ self.kind.id_field(): self.id, "name": self.name });
        if self.kind == Kind::LibSql {
            // The planner checks this projection against the direct record.
            summary["appName"] = json!(slug(&self.name));
            summary["description"] = json!("Edge");
        }
        summary
    }

    fn search_item(&self) -> Value {
        let mut item =
            json!({ self.kind.id_field(): self.id, "environmentId": self.env, "name": self.name });
        if self.kind == Kind::Compose {
            // The Compose collection must agree with the direct read.
            item["appName"] = json!(slug(&self.name));
            item["description"] = json!("Stack");
            item["sourceType"] = json!("raw");
        }
        item
    }

    /// The direct `.one` record, with the embedded collections the SDK reads from it.
    fn record(&self) -> Value {
        let mut record = json!({
            self.kind.id_field(): self.id,
            "environmentId": self.env,
            "name": self.name,
            "appName": slug(&self.name),
            "serverId": self.server,
        });
        match self.kind {
            Kind::Application => {
                record["description"] = json!("Service");
                record["replicas"] = json!(1);
                record["env"] = json!(format!("SECRET={SECRET_CANARY}"));
                record["buildServerId"] = json!(self.build_server);
                record["registryId"] = json!(self.registries[0]);
                record["buildRegistryId"] = json!(self.registries[1]);
                record["rollbackRegistryId"] = json!(self.registries[2]);
                record["ports"] = json!(self.ports);
                record["redirects"] = json!(self.redirects);
                record["security"] = json!(self.security);
            }
            Kind::Compose => {
                record["description"] = json!("Stack");
                record["sourceType"] = json!("raw");
                record["composeType"] = json!("docker-compose");
                record["composeFile"] = json!(SECRET_CANARY);
            }
            Kind::Postgres => {
                record["dockerImage"] = json!("postgres:18");
                record["databaseName"] = json!("app");
                record["databaseUser"] = json!("app");
                record["databasePassword"] = json!(SECRET_CANARY);
            }
            Kind::MySql | Kind::MariaDb => {
                record["dockerImage"] = json!("mysql:8");
                record["databaseName"] = json!("app");
                record["databaseUser"] = json!("app");
                record["databasePassword"] = json!(SECRET_CANARY);
                record["databaseRootPassword"] = json!(SECRET_CANARY);
            }
            Kind::Mongo => {
                record["dockerImage"] = json!("mongo:8");
                record["databaseUser"] = json!("app");
                record["databasePassword"] = json!(SECRET_CANARY);
                record["replicaSets"] = json!(false);
            }
            Kind::LibSql => {
                record["dockerImage"] = json!("ghcr.io/tursodatabase/libsql-server:v0.24.32");
                record["description"] = json!("Edge");
                record["databaseUser"] = json!("app");
                record["databasePassword"] = json!(SECRET_CANARY);
                record["sqldNode"] = json!("primary");
                record["sqldPrimaryUrl"] = Value::Null;
            }
            Kind::Redis => {
                record["dockerImage"] = json!("redis:8");
                record["databasePassword"] = json!(SECRET_CANARY);
            }
        }
        if self.kind.has_backups() {
            record["backups"] = json!(self.backups);
        }
        for (field, value) in &self.patch {
            record[field] = value.clone();
        }
        record
    }
}

struct Env {
    id: String,
    name: String,
    description: Option<String>,
}

pub struct World {
    project_id: String,
    project_name: String,
    project_description: Option<String>,
    envs: Vec<Env>,
    services: Vec<Svc>,
    servers: Vec<(String, String)>,
    registries: Vec<(String, String)>,
    destinations: Vec<(String, String)>,
    tags: Vec<Value>,
    project_tags: Vec<String>,
}

fn slug(name: &str) -> String {
    name.to_ascii_lowercase().replace(' ', "-")
}

impl World {
    pub fn new(project_id: &str, name: &str) -> Self {
        Self {
            project_id: project_id.to_owned(),
            project_name: name.to_owned(),
            project_description: Some("Platform project".to_owned()),
            envs: Vec::new(),
            services: Vec::new(),
            servers: Vec::new(),
            registries: Vec::new(),
            destinations: Vec::new(),
            tags: Vec::new(),
            project_tags: Vec::new(),
        }
    }

    /// Adds an instance tag, optionally attached to the project.
    pub fn tag(&mut self, id: &str, name: &str, color: Option<&str>, attached: bool) -> &mut Self {
        self.tags
            .push(json!({ "tagId": id, "name": name, "color": color }));
        if attached {
            self.project_tags.push(id.to_owned());
        }
        self
    }

    pub fn environment(&mut self, id: &str, name: &str) -> &mut Self {
        self.envs.push(Env {
            id: id.to_owned(),
            name: name.to_owned(),
            description: Some(format!("{name} environment")),
        });
        self
    }

    pub fn service(&mut self, env: &str, kind: Kind, id: &str, name: &str) -> &mut Svc {
        assert!(self.envs.iter().any(|candidate| candidate.id == env));
        self.services.push(Svc {
            kind,
            env: env.to_owned(),
            id: id.to_owned(),
            name: name.to_owned(),
            server: None,
            registries: [None, None, None],
            build_server: None,
            domains: Vec::new(),
            ports: Vec::new(),
            redirects: Vec::new(),
            security: Vec::new(),
            mounts: Vec::new(),
            schedules: Vec::new(),
            backups: Vec::new(),
            patch: Vec::new(),
        });
        self.services.last_mut().expect("a service was just added")
    }

    pub fn svc(&mut self, id: &str) -> &mut Svc {
        self.services
            .iter_mut()
            .find(|service| service.id == id)
            .expect("the service exists")
    }

    pub fn external_server(&mut self, id: &str, name: &str) -> &mut Self {
        self.servers.push((id.to_owned(), name.to_owned()));
        self
    }

    pub fn registry(&mut self, id: &str, name: &str) -> &mut Self {
        self.registries.push((id.to_owned(), name.to_owned()));
        self
    }

    pub fn destination(&mut self, id: &str, name: &str) -> &mut Self {
        self.destinations.push((id.to_owned(), name.to_owned()));
        self
    }

    /// The `project.one` / `project.all` record.
    pub fn project_record(&self) -> Value {
        let environments = self
            .envs
            .iter()
            .enumerate()
            .map(|(index, env)| {
                let of = |kind| {
                    self.services
                        .iter()
                        .filter(|service| service.env == env.id && service.kind == kind)
                        .map(Svc::summary)
                        .collect::<Vec<_>>()
                };
                json!({
                    "environmentId": env.id, "name": env.name, "isDefault": index == 0,
                    "applications": of(Kind::Application), "postgres": of(Kind::Postgres),
                    "redis": of(Kind::Redis), "libsql": of(Kind::LibSql),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "projectId": self.project_id, "name": self.project_name,
            "description": self.project_description, "environments": environments,
            "projectTags": self
                .project_tags
                .iter()
                .map(|id| json!({ "tagId": id }))
                .collect::<Vec<_>>(),
        })
    }

    pub fn routes(&self) -> Vec<(String, Vec<Reply>)> {
        let one = |path: &str, field: &str, id: &str| format!("GET /api/{path}|{field}={id}");
        let mut routes = Vec::new();
        let mut push = |key: String, body: Value| routes.push((key, vec![ok(body.to_string())]));

        let project = self.project_record();
        push(
            one("project.one", "projectId", &self.project_id),
            project.clone(),
        );
        push("GET /api/project.all".to_owned(), json!([project]));
        push("GET /api/tag.all".to_owned(), json!(self.tags));
        push(
            one("environment.byProjectId", "projectId", &self.project_id),
            json!(
                self.envs
                    .iter()
                    .map(|env| json!({
                        "environmentId": env.id, "name": env.name, "description": env.description,
                    }))
                    .collect::<Vec<_>>()
            ),
        );
        for env in &self.envs {
            push(
                one("environment.one", "environmentId", &env.id),
                json!({
                    "environmentId": env.id, "name": env.name,
                    "description": env.description, "projectId": self.project_id,
                }),
            );
            for kind in [
                Kind::Application,
                Kind::Compose,
                Kind::Postgres,
                Kind::MySql,
                Kind::MariaDb,
                Kind::Mongo,
                Kind::Redis,
            ] {
                let items = self
                    .services
                    .iter()
                    .filter(|service| service.env == env.id && service.kind == kind)
                    .map(Svc::search_item)
                    .collect::<Vec<_>>();
                debug_assert!(kind.searchable());
                push(
                    one(&format!("{}.search", kind.key()), "environmentId", &env.id),
                    json!({ "items": items, "total": items.len() }),
                );
            }
        }
        for service in &self.services {
            push(
                one(
                    &format!("{}.one", service.kind.key()),
                    &service.kind.id_field(),
                    &service.id,
                ),
                service.record(),
            );
            push(
                one("mounts.listByServiceId", "serviceId", &service.id),
                json!(service.mounts),
            );
            // Reconciliation also re-reads every leaf directly by identity.
            for (path, field, leaves) in [
                ("domain.one", "domainId", &service.domains),
                ("port.one", "portId", &service.ports),
                ("redirects.one", "redirectId", &service.redirects),
                ("security.one", "securityId", &service.security),
                ("mounts.one", "mountId", &service.mounts),
                ("schedule.one", "scheduleId", &service.schedules),
                ("backup.one", "backupId", &service.backups),
            ] {
                for leaf in leaves {
                    let id = leaf[field].as_str().expect("a leaf has an identity");
                    push(one(path, field, id), leaf.clone());
                }
            }
            if service.kind == Kind::Application {
                push(
                    one("domain.byApplicationId", "applicationId", &service.id),
                    json!(service.domains),
                );
            }
            if matches!(service.kind, Kind::Application | Kind::Compose) {
                push(
                    one("schedule.list", "id", &service.id),
                    json!(service.schedules),
                );
            }
        }
        push(
            "GET /api/server.all".to_owned(),
            json!(
                self.servers
                    .iter()
                    .map(
                        |(id, name)| json!({"serverId": id, "name": name, "serverType": "deploy", "command": SECRET_CANARY})
                    )
                    .collect::<Vec<_>>()
            ),
        );
        push(
            "GET /api/registry.all".to_owned(),
            json!(
                self.registries
                    .iter()
                    .map(|(id, name)| json!({
                        "registryId": id, "registryName": name, "password": SECRET_CANARY,
                    }))
                    .collect::<Vec<_>>()
            ),
        );
        push(
            "GET /api/destination.all".to_owned(),
            json!(
                self.destinations
                    .iter()
                    .map(|(id, name)| json!({
                        "destinationId": id, "name": name, "accessKey": SECRET_CANARY,
                    }))
                    .collect::<Vec<_>>()
            ),
        );

        routes
    }

    pub fn router(&self) -> Router {
        self.router_with(Vec::new())
    }

    /// Splits each fragment route into end-delimited variants, so `id=app-1`
    /// never answers a request for `id=app-10`.
    fn delimited(routes: Vec<(String, Vec<Reply>)>) -> Vec<(String, Vec<Reply>)> {
        routes
            .into_iter()
            .flat_map(|(key, replies)| {
                if key.contains('|') {
                    vec![
                        (format!("{key} "), replies.clone()),
                        (format!("{key}&"), replies),
                    ]
                } else {
                    vec![(key, replies)]
                }
            })
            .collect()
    }

    /// Starts a router whose `overrides` replace the generated routes with the same key.
    pub fn router_with(&self, overrides: Vec<(String, Vec<Reply>)>) -> Router {
        let mut routes = self.routes();
        routes.retain(|(key, _)| overrides.iter().all(|(other, _)| other != key));
        routes.extend(overrides);
        let routes = Self::delimited(routes);
        Router::start(
            routes
                .iter()
                .map(|(key, replies)| (key.as_str(), replies.clone()))
                .collect(),
        )
    }

    /// The generated route key for a record read, for building overrides.
    pub fn key(path: &str, field: &str, id: &str) -> String {
        format!("GET /api/{path}|{field}={id}")
    }
}
