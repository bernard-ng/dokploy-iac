//! Append-style construction of one imported workspace.
//!
//! An [`ImportContext`] accumulates the typed configuration document and the
//! durable resource state together, one resource at a time. Every `add_*`
//! method writes the document entry and its state entry from the same remote
//! record so the two can never disagree, and every method takes explicit
//! containment (an [`EnvScope`] and, for leaves, the parent address) instead of
//! rebuilding the project and environment around each resource.
//!
//! Per-kind semantics are unchanged from the single-resource importers: services
//! and leaves are protected, secrets and executable text stay unmanaged, and
//! external servers, registries, and destinations are written as name selectors.

use super::*;

/// Names an environment that already exists in the context.
pub(super) struct EnvScope {
    address: ResourceAddress,
}

impl EnvScope {
    pub(super) const fn address(&self) -> &ResourceAddress {
        &self.address
    }
}

/// The configuration document and durable state accumulated so far.
pub(super) struct ImportContext {
    pub(super) document: ConfigDocument,
    pub(super) resources: Vec<ImportedResource>,
    project: ResourceAddress,
}

fn protected() -> LifecycleDocument {
    LifecycleDocument {
        protect: Field::Set(true),
        ..LifecycleDocument::default()
    }
}

impl ImportContext {
    /// Starts a workspace for one project under an explicit project address.
    pub(super) fn new(
        project: &ProjectDetails,
        address: &ResourceAddress,
    ) -> Result<Self, ImportError> {
        let mut document = ConfigDocument::new(address.name().clone());
        document.project_mut().description = response_field(&project.description);
        let state = resource_state(
            address,
            project.project_id.as_str(),
            false,
            description_inputs(&project.description),
            None,
        )?;

        Ok(Self {
            document,
            resources: vec![ImportedResource {
                address: address.clone(),
                state,
            }],
            project: address.clone(),
        })
    }

    /// Adds one environment beneath the project.
    pub(super) fn add_environment(
        &mut self,
        environment: &EnvironmentDetails,
        address: ResourceAddress,
    ) -> Result<EnvScope, ImportError> {
        let mut config = EnvironmentDocument::default();
        config.description = response_field(&environment.description);
        self.document
            .add_environment(address.name().clone(), config)?;
        self.resources.push(ImportedResource {
            state: resource_state(
                &address,
                environment.environment_id.as_str(),
                false,
                description_inputs(&environment.description),
                Some(self.project.clone()),
            )?,
            address: address.clone(),
        });

        Ok(EnvScope { address })
    }

    pub(super) fn environment(
        &mut self,
        env: &EnvScope,
    ) -> Result<&mut EnvironmentDocument, ImportError> {
        self.document
            .environment_mut(env.address.name())
            .ok_or(ImportError::MissingContainment)
    }

    fn application(
        &mut self,
        env: &EnvScope,
        application: &ResourceAddress,
    ) -> Result<&mut ApplicationDocument, ImportError> {
        self.environment(env)?
            .application_mut(application.name())
            .ok_or(ImportError::MissingContainment)
    }

    pub(super) fn push(
        &mut self,
        address: &ResourceAddress,
        remote_id: &str,
        protect: bool,
        inputs: serde_json::Map<String, serde_json::Value>,
        containment: &ResourceAddress,
        dependencies: Vec<ResourceAddress>,
    ) -> Result<(), ImportError> {
        self.resources.push(ImportedResource {
            address: address.clone(),
            state: resource_state_with_dependencies(
                address,
                remote_id,
                protect,
                inputs,
                Some(containment.clone()),
                dependencies,
            )?,
        });

        Ok(())
    }

    pub(super) fn add_application(
        &mut self,
        env: &EnvScope,
        application: &ApplicationDetails,
        associations: &ImportedAssociations,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        let (config, inputs) = application_config(application, associations);
        self.environment(env)?
            .add_application(address.name().clone(), config)?;
        self.push(
            address,
            application.application_id.as_str(),
            false,
            inputs,
            &env.address,
            Vec::new(),
        )
    }

    pub(super) fn add_port(
        &mut self,
        env: &EnvScope,
        application: &ResourceAddress,
        port: &PortDetails,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.application(env, application)?.add_port(
            address.name().clone(),
            PortDocument {
                published_port: PortNumber::new(port.published_port.get())
                    .expect("the SDK guarantees nonzero Port numbers"),
                target_port: PortNumber::new(port.target_port.get())
                    .expect("the SDK guarantees nonzero Port numbers"),
                publish_mode: match port.publish_mode {
                    PublishMode::Ingress => PortPublishModeConfig::Ingress,
                    PublishMode::Host => PortPublishModeConfig::Host,
                },
                protocol: match port.protocol {
                    PortProtocol::Tcp => PortProtocolConfig::Tcp,
                    PortProtocol::Udp => PortProtocolConfig::Udp,
                },
                depends_on: Vec::new(),
                lifecycle: protected(),
            },
        )?;
        self.push(
            address,
            port.port_id.as_str(),
            true,
            serde_json::Map::from_iter([
                (
                    "published_port".to_owned(),
                    serde_json::json!(port.published_port.get()),
                ),
                (
                    "target_port".to_owned(),
                    serde_json::json!(port.target_port.get()),
                ),
                (
                    "publish_mode".to_owned(),
                    serde_json::json!(port_publish_mode_label(port.publish_mode)),
                ),
                (
                    "protocol".to_owned(),
                    serde_json::json!(port_protocol_label(port.protocol)),
                ),
            ]),
            application,
            Vec::new(),
        )
    }

    pub(super) fn add_redirect(
        &mut self,
        env: &EnvScope,
        application: &ResourceAddress,
        redirect: &RedirectDetails,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.application(env, application)?.add_redirect(
            address.name().clone(),
            RedirectDocument {
                regex: NonEmptyText::new(redirect.regex.clone())
                    .ok_or(ImportError::InvalidRemoteTopology)?,
                replacement: NonEmptyText::new(redirect.replacement.clone())
                    .ok_or(ImportError::InvalidRemoteTopology)?,
                permanent: redirect.permanent,
                depends_on: Vec::new(),
                lifecycle: protected(),
            },
        )?;
        self.push(
            address,
            redirect.redirect_id.as_str(),
            true,
            serde_json::Map::from_iter([
                ("regex".to_owned(), serde_json::json!(redirect.regex)),
                (
                    "replacement".to_owned(),
                    serde_json::json!(redirect.replacement),
                ),
                (
                    "permanent".to_owned(),
                    serde_json::json!(redirect.permanent),
                ),
            ]),
            application,
            Vec::new(),
        )
    }

    /// Imports the username and identity only. The password is never read into the
    /// document or state; it stays unmanaged until the operator declares a descriptor.
    pub(super) fn add_security(
        &mut self,
        env: &EnvScope,
        application: &ResourceAddress,
        entry: &SecurityDetails,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.application(env, application)?.add_security(
            address.name().clone(),
            SecurityDocument {
                username: NonEmptyText::new(entry.username.clone())
                    .ok_or(ImportError::InvalidRemoteTopology)?,
                password: Field::Unmanaged,
                depends_on: Vec::new(),
                lifecycle: protected(),
            },
        )?;
        self.push(
            address,
            entry.security_id.as_str(),
            true,
            serde_json::Map::from_iter([(
                "username".to_owned(),
                serde_json::json!(entry.username),
            )]),
            application,
            Vec::new(),
        )
    }

    pub(super) fn add_domain(
        &mut self,
        env: &EnvScope,
        application: &ResourceAddress,
        domain: &dokploy_sdk::DomainDetails,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.environment(env)?.add_domain(
            address.name().clone(),
            DomainDocument {
                host: Field::Set(domain.host.clone()),
                application: Field::Set(application.clone()),
                ..DomainDocument::default()
            },
        )?;
        let mut inputs = serde_json::Map::new();
        inputs.insert("host".to_owned(), serde_json::json!(domain.host));
        inputs.insert(
            "application".to_owned(),
            serde_json::json!(application.to_string()),
        );
        self.push(
            address,
            domain.domain_id.as_str(),
            false,
            inputs,
            &env.address,
            vec![application.clone()],
        )
    }

    pub(super) fn add_compose(
        &mut self,
        env: &EnvScope,
        compose: &dokploy_sdk::ComposeDetails,
        server: Option<&str>,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.environment(env)?.add_compose(
            address.name().clone(),
            ComposeDocument {
                description: response_field(&compose.description),
                server: imported_server_selector(server),
                lifecycle: protected(),
                ..ComposeDocument::default()
            },
        )?;
        self.push(
            address,
            compose.compose_id.as_str(),
            true,
            service_inputs(description_inputs(&compose.description), server),
            &env.address,
            Vec::new(),
        )
    }

    pub(super) fn add_postgres(
        &mut self,
        env: &EnvScope,
        database: &dokploy_sdk::PostgresDetails,
        server: Option<&str>,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.environment(env)?.add_postgres(
            address.name().clone(),
            PostgresDocument {
                database: response_field(&database.database_name),
                username: response_field(&database.database_user),
                server: imported_server_selector(server),
                lifecycle: protected(),
                ..PostgresDocument::default()
            },
        )?;
        let mut inputs = serde_json::Map::new();
        insert_response(&mut inputs, "database", &database.database_name);
        insert_response(&mut inputs, "username", &database.database_user);
        self.push(
            address,
            database.postgres_id.as_str(),
            true,
            service_inputs(inputs, server),
            &env.address,
            Vec::new(),
        )
    }

    pub(super) fn add_redis(
        &mut self,
        env: &EnvScope,
        database: &dokploy_sdk::RedisDetails,
        server: Option<&str>,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.environment(env)?.add_redis(
            address.name().clone(),
            RedisDocument {
                server: imported_server_selector(server),
                lifecycle: protected(),
                ..RedisDocument::default()
            },
        )?;
        self.push(
            address,
            database.redis_id.as_str(),
            true,
            service_inputs(serde_json::Map::new(), server),
            &env.address,
            Vec::new(),
        )
    }

    pub(super) fn add_mysql(
        &mut self,
        env: &EnvScope,
        database: &dokploy_sdk::MySqlDetails,
        server: Option<&str>,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.environment(env)?.add_mysql(
            address.name().clone(),
            MySqlDocument {
                database: response_field(&database.database_name),
                username: response_field(&database.database_user),
                server: imported_server_selector(server),
                lifecycle: protected(),
                ..MySqlDocument::default()
            },
        )?;
        let mut inputs = serde_json::Map::new();
        insert_response(&mut inputs, "database", &database.database_name);
        insert_response(&mut inputs, "username", &database.database_user);
        self.push(
            address,
            database.mysql_id.as_str(),
            true,
            service_inputs(inputs, server),
            &env.address,
            Vec::new(),
        )
    }

    pub(super) fn add_mariadb(
        &mut self,
        env: &EnvScope,
        database: &dokploy_sdk::MariaDbDetails,
        server: Option<&str>,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.environment(env)?.add_mariadb(
            address.name().clone(),
            MariaDbDocument {
                database: response_field(&database.database_name),
                username: response_field(&database.database_user),
                server: imported_server_selector(server),
                lifecycle: protected(),
                ..MariaDbDocument::default()
            },
        )?;
        let mut inputs = serde_json::Map::new();
        insert_response(&mut inputs, "database", &database.database_name);
        insert_response(&mut inputs, "username", &database.database_user);
        self.push(
            address,
            database.mariadb_id.as_str(),
            true,
            service_inputs(inputs, server),
            &env.address,
            Vec::new(),
        )
    }

    pub(super) fn add_mongo(
        &mut self,
        env: &EnvScope,
        database: &dokploy_sdk::MongoDetails,
        server: Option<&str>,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        self.environment(env)?.add_mongo(
            address.name().clone(),
            MongoDocument {
                username: response_field(&database.database_user),
                replica_sets: response_field(&database.replica_sets),
                server: imported_server_selector(server),
                lifecycle: protected(),
                ..MongoDocument::default()
            },
        )?;
        let mut inputs = serde_json::Map::new();
        insert_response(&mut inputs, "username", &database.database_user);
        insert_response(&mut inputs, "replica_sets", &database.replica_sets);
        self.push(
            address,
            database.mongo_id.as_str(),
            true,
            service_inputs(inputs, server),
            &env.address,
            Vec::new(),
        )
    }

    pub(super) fn add_libsql(
        &mut self,
        env: &EnvScope,
        database: &dokploy_sdk::LibSqlDetails,
        server: Option<&str>,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        let node = imported_libsql_node(database)?;
        self.environment(env)?.add_libsql(
            address.name().clone(),
            LibSqlDocument {
                description: match &database.description {
                    Some(description) => Field::Set(description.clone()),
                    None => Field::Clear,
                },
                username: response_field(&database.database_user),
                node: Field::Set(node.clone()),
                server: imported_server_selector(server),
                lifecycle: protected(),
                ..LibSqlDocument::default()
            },
        )?;
        let mut inputs = serde_json::Map::new();
        inputs.insert(
            "description".to_owned(),
            database
                .description
                .clone()
                .map_or(serde_json::Value::Null, serde_json::Value::String),
        );
        insert_response(&mut inputs, "username", &database.database_user);
        inputs.insert(
            "node".to_owned(),
            match node {
                LibSqlNodeConfig::Primary => serde_json::json!({"type":"primary"}),
                LibSqlNodeConfig::Replica { primary_url } => {
                    serde_json::json!({"type":"replica","primary_url":primary_url})
                }
            },
        );
        self.push(
            address,
            database.libsql_id.as_str(),
            true,
            service_inputs(inputs, server),
            &env.address,
            Vec::new(),
        )
    }
}
