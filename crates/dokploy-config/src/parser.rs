use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

use dokploy_state::{ResourceAddress, ResourceKind};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema, schema_for};
use serde::Deserialize;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Spanned};

use crate::model::{
    ApplicationConfig, ComposeConfig, ConfigError, DokployConfig, EnvironmentConfig, LibSqlConfig,
    LibSqlNodeConfig, MariaDbConfig, MongoConfig, MySqlConfig, PortConfig, PostgresConfig,
    ProjectConfig, RedisConfig, ResourceConfig, SourceLocation, ValidationDiagnostic,
    ValidationIssue, address,
};
use crate::{
    ConfigValue, DomainConfig, Field, Lifecycle, MoveDeclaration, PortNumber, PortProtocolConfig,
    PortPublishModeConfig, RemovedDeclaration, SecretSource, SourceConfig,
};

type ResourceTables<'a> = (
    &'a mut BTreeMap<ResourceAddress, ResourceConfig>,
    &'a mut BTreeMap<ResourceAddress, ResourceAddress>,
    &'a mut BTreeMap<ResourceAddress, SourceLocation>,
);

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawDocument {
    #[schemars(schema_with = "version_schema")]
    version: u32,
    #[schemars(with = "RawProject")]
    project: Spanned<RawProject>,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawEnvironment>")]
    environments: BTreeMap<String, Spanned<RawEnvironment>>,
    #[serde(default)]
    #[schemars(with = "Vec<MoveDeclaration>")]
    moves: Vec<Spanned<MoveDeclaration>>,
    #[serde(default)]
    #[schemars(with = "Vec<RemovedDeclaration>")]
    removed: Vec<Spanned<RemovedDeclaration>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawProject {
    name: String,
    #[serde(default)]
    description: Field<String>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawEnvironment {
    #[serde(default)]
    description: Field<String>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawApplication>")]
    applications: BTreeMap<String, Spanned<RawApplication>>,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawCompose>")]
    compose: BTreeMap<String, Spanned<RawCompose>>,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawPostgres>")]
    postgres: BTreeMap<String, Spanned<RawPostgres>>,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawMySql>")]
    mysql: BTreeMap<String, Spanned<RawMySql>>,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawMariaDb>")]
    mariadb: BTreeMap<String, Spanned<RawMariaDb>>,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawMongo>")]
    mongo: BTreeMap<String, Spanned<RawMongo>>,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawLibSql>")]
    libsql: BTreeMap<String, Spanned<RawLibSql>>,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawRedis>")]
    redis: BTreeMap<String, Spanned<RawRedis>>,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawDomain>")]
    domains: BTreeMap<String, Spanned<RawDomain>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawApplication {
    #[serde(default)]
    description: Field<String>,
    #[serde(default)]
    replicas: Field<u32>,
    #[serde(default)]
    source: Field<SourceConfig>,
    #[serde(default)]
    environment: Field<BTreeMap<String, Field<ConfigValue>>>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, RawPort>")]
    ports: BTreeMap<String, Spanned<RawPort>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawPort {
    published_port: PortNumber,
    target_port: PortNumber,
    publish_mode: PortPublishModeConfig,
    protocol: PortProtocolConfig,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawCompose {
    #[serde(default)]
    description: Field<String>,
    #[serde(default)]
    document: Field<SecretSource>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawPostgres {
    #[serde(default)]
    database: Field<String>,
    #[serde(default)]
    username: Field<String>,
    #[serde(default)]
    password: Field<SecretSource>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawMySql {
    #[serde(default)]
    database: Field<String>,
    #[serde(default)]
    username: Field<String>,
    #[serde(default)]
    password: Field<SecretSource>,
    #[serde(default)]
    root_password: Field<SecretSource>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawRedis {
    #[serde(default)]
    password: Field<SecretSource>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawMariaDb {
    #[serde(default)]
    database: Field<String>,
    #[serde(default)]
    username: Field<String>,
    #[serde(default)]
    password: Field<SecretSource>,
    #[serde(default)]
    root_password: Field<SecretSource>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawMongo {
    #[serde(default)]
    username: Field<String>,
    #[serde(default)]
    password: Field<SecretSource>,
    #[serde(default)]
    replica_sets: Field<bool>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum RawLibSqlNode {
    Primary {},
    Replica {
        #[schemars(schema_with = "non_empty_string_schema")]
        primary_url: String,
    },
}

impl From<RawLibSqlNode> for LibSqlNodeConfig {
    fn from(value: RawLibSqlNode) -> Self {
        match value {
            RawLibSqlNode::Primary {} => Self::Primary,
            RawLibSqlNode::Replica { primary_url } => Self::Replica { primary_url },
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawLibSql {
    #[serde(default)]
    description: Field<String>,
    #[serde(default)]
    username: Field<String>,
    #[serde(default)]
    password: Field<SecretSource>,
    #[serde(default)]
    node: Field<RawLibSqlNode>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawDomain {
    #[serde(default)]
    host: Field<String>,
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    application: Field<ResourceAddress>,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    depends_on: Vec<ResourceAddress>,
    #[serde(default)]
    lifecycle: Lifecycle,
}

impl DokployConfig {
    /// Parses untrusted YAML with strict syntax, resource budgets, and semantic validation.
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        if source.len() > crate::MAX_CONFIG_BYTES {
            return Err(ConfigError::InputTooLarge {
                limit_bytes: crate::MAX_CONFIG_BYTES,
            });
        }

        let options = serde_saphyr::options! {
            duplicate_keys: DuplicateKeyPolicy::Error,
            merge_keys: MergeKeyPolicy::Error,
            strict_booleans: true,
            reject_unsupported_tags: true,
            emit_comments: false,
            budget: serde_saphyr::budget! {
                max_reader_input_bytes: Some(crate::MAX_CONFIG_BYTES),
                max_events: 50_000,
                max_aliases: 0,
                max_anchors: 0,
                max_recorded_anchor_events: 0,
                max_recorded_anchor_bytes: 0,
                max_depth: 32,
                max_inclusion_depth: 0,
                max_documents: 1,
                max_nodes: 20_000,
                max_total_scalar_bytes: 512 * 1024,
                max_total_comment_bytes: 512 * 1024,
                max_merge_keys: 0,
            },
        };
        let raw: Spanned<RawDocument> = serde_saphyr::from_str_with_options(source, options)
            .map_err(|error| {
                error
                    .location()
                    .map_or(ConfigError::ParseWithoutLocation, |location| {
                        ConfigError::Parse {
                            line: location.line(),
                            column: location.column(),
                        }
                    })
            })?;

        Self::from_raw(raw.value)
    }

    fn from_raw(raw: RawDocument) -> Result<Self, ConfigError> {
        if raw.version != 1 {
            return Err(ConfigError::UnsupportedVersion { found: raw.version });
        }

        let mut resources = BTreeMap::new();
        let mut parents = BTreeMap::new();
        let mut locations = BTreeMap::new();
        let mut diagnostics = Vec::new();
        let project_location = source_location(raw.project.defined);
        let project = raw.project.value;

        let project_address = address(
            ResourceKind::Project,
            project.name,
            project_location,
            &mut diagnostics,
        );
        if let Some(address) = project_address.as_ref() {
            insert_resource(
                &mut resources,
                &mut locations,
                address.clone(),
                ResourceConfig::Project(ProjectConfig {
                    description: project.description,
                    depends_on: project.depends_on,
                    lifecycle: project.lifecycle,
                }),
                project_location,
                &mut diagnostics,
            );
        }

        for (environment_name, environment) in raw.environments {
            let environment_location = source_location(environment.defined);
            let environment = environment.value;
            let environment_address = address(
                ResourceKind::Environment,
                environment_name,
                environment_location,
                &mut diagnostics,
            );
            if let Some(address) = environment_address.as_ref()
                && insert_resource(
                    &mut resources,
                    &mut locations,
                    address.clone(),
                    ResourceConfig::Environment(EnvironmentConfig {
                        description: environment.description,
                        depends_on: environment.depends_on,
                        lifecycle: environment.lifecycle,
                    }),
                    environment_location,
                    &mut diagnostics,
                )
                && let Some(project_address) = project_address.as_ref()
            {
                parents.insert(address.clone(), project_address.clone());
            }

            for (name, raw_config) in environment.applications {
                let location = source_location(raw_config.defined);
                let raw_config = raw_config.value;
                let child_address =
                    address(ResourceKind::Application, name, location, &mut diagnostics);
                let application_address = child_address.clone();
                insert_child_resource(
                    (&mut resources, &mut parents, &mut locations),
                    child_address,
                    environment_address.as_ref(),
                    ResourceConfig::Application(ApplicationConfig {
                        description: raw_config.description,
                        replicas: raw_config.replicas,
                        source: raw_config.source,
                        environment: raw_config.environment,
                        depends_on: raw_config.depends_on,
                        lifecycle: raw_config.lifecycle,
                    }),
                    location,
                    &mut diagnostics,
                );

                for (name, raw_port) in raw_config.ports {
                    let port_location = source_location(raw_port.defined);
                    let raw_port = raw_port.value;
                    let port_address =
                        address(ResourceKind::Port, name, port_location, &mut diagnostics);
                    insert_child_resource(
                        (&mut resources, &mut parents, &mut locations),
                        port_address,
                        application_address.as_ref(),
                        ResourceConfig::Port(PortConfig {
                            published_port: raw_port.published_port,
                            target_port: raw_port.target_port,
                            publish_mode: raw_port.publish_mode,
                            protocol: raw_port.protocol,
                            depends_on: raw_port.depends_on,
                            lifecycle: raw_port.lifecycle,
                        }),
                        port_location,
                        &mut diagnostics,
                    );
                }
            }

            for (name, raw_config) in environment.compose {
                let location = source_location(raw_config.defined);
                let raw_config = raw_config.value;
                let child_address =
                    address(ResourceKind::Compose, name, location, &mut diagnostics);
                insert_child_resource(
                    (&mut resources, &mut parents, &mut locations),
                    child_address,
                    environment_address.as_ref(),
                    ResourceConfig::Compose(ComposeConfig {
                        description: raw_config.description,
                        document: raw_config.document,
                        depends_on: raw_config.depends_on,
                        lifecycle: raw_config.lifecycle,
                    }),
                    location,
                    &mut diagnostics,
                );
            }

            for (name, raw_config) in environment.postgres {
                let location = source_location(raw_config.defined);
                let raw_config = raw_config.value;
                let child_address =
                    address(ResourceKind::Postgres, name, location, &mut diagnostics);
                insert_child_resource(
                    (&mut resources, &mut parents, &mut locations),
                    child_address,
                    environment_address.as_ref(),
                    ResourceConfig::Postgres(PostgresConfig {
                        database: raw_config.database,
                        username: raw_config.username,
                        password: raw_config.password,
                        depends_on: raw_config.depends_on,
                        lifecycle: raw_config.lifecycle,
                    }),
                    location,
                    &mut diagnostics,
                );
            }

            for (name, raw_config) in environment.mysql {
                let location = source_location(raw_config.defined);
                let raw_config = raw_config.value;
                let child_address = address(ResourceKind::MySql, name, location, &mut diagnostics);
                insert_child_resource(
                    (&mut resources, &mut parents, &mut locations),
                    child_address,
                    environment_address.as_ref(),
                    ResourceConfig::MySql(MySqlConfig {
                        database: raw_config.database,
                        username: raw_config.username,
                        password: raw_config.password,
                        root_password: raw_config.root_password,
                        depends_on: raw_config.depends_on,
                        lifecycle: raw_config.lifecycle,
                    }),
                    location,
                    &mut diagnostics,
                );
            }

            for (name, raw_config) in environment.mariadb {
                let location = source_location(raw_config.defined);
                let raw_config = raw_config.value;
                let child_address =
                    address(ResourceKind::MariaDb, name, location, &mut diagnostics);
                insert_child_resource(
                    (&mut resources, &mut parents, &mut locations),
                    child_address,
                    environment_address.as_ref(),
                    ResourceConfig::MariaDb(MariaDbConfig {
                        database: raw_config.database,
                        username: raw_config.username,
                        password: raw_config.password,
                        root_password: raw_config.root_password,
                        depends_on: raw_config.depends_on,
                        lifecycle: raw_config.lifecycle,
                    }),
                    location,
                    &mut diagnostics,
                );
            }

            for (name, raw_config) in environment.mongo {
                let location = source_location(raw_config.defined);
                let raw_config = raw_config.value;
                let child_address = address(ResourceKind::Mongo, name, location, &mut diagnostics);
                insert_child_resource(
                    (&mut resources, &mut parents, &mut locations),
                    child_address,
                    environment_address.as_ref(),
                    ResourceConfig::Mongo(MongoConfig {
                        username: raw_config.username,
                        password: raw_config.password,
                        replica_sets: raw_config.replica_sets,
                        depends_on: raw_config.depends_on,
                        lifecycle: raw_config.lifecycle,
                    }),
                    location,
                    &mut diagnostics,
                );
            }

            for (name, raw_config) in environment.libsql {
                let location = source_location(raw_config.defined);
                let raw_config = raw_config.value;
                let child_address = address(ResourceKind::LibSql, name, location, &mut diagnostics);
                let node = match raw_config.node {
                    Field::Unmanaged => Field::Unmanaged,
                    Field::Clear => Field::Clear,
                    Field::Set(node) => Field::Set(node.into()),
                };
                insert_child_resource(
                    (&mut resources, &mut parents, &mut locations),
                    child_address,
                    environment_address.as_ref(),
                    ResourceConfig::LibSql(LibSqlConfig {
                        description: raw_config.description,
                        username: raw_config.username,
                        password: raw_config.password,
                        node,
                        depends_on: raw_config.depends_on,
                        lifecycle: raw_config.lifecycle,
                    }),
                    location,
                    &mut diagnostics,
                );
            }

            for (name, raw_config) in environment.redis {
                let location = source_location(raw_config.defined);
                let raw_config = raw_config.value;
                let child_address = address(ResourceKind::Redis, name, location, &mut diagnostics);
                insert_child_resource(
                    (&mut resources, &mut parents, &mut locations),
                    child_address,
                    environment_address.as_ref(),
                    ResourceConfig::Redis(RedisConfig {
                        password: raw_config.password,
                        depends_on: raw_config.depends_on,
                        lifecycle: raw_config.lifecycle,
                    }),
                    location,
                    &mut diagnostics,
                );
            }

            for (name, raw_config) in environment.domains {
                let location = source_location(raw_config.defined);
                let raw_config = raw_config.value;
                let child_address = address(ResourceKind::Domain, name, location, &mut diagnostics);
                insert_child_resource(
                    (&mut resources, &mut parents, &mut locations),
                    child_address,
                    environment_address.as_ref(),
                    ResourceConfig::Domain(DomainConfig {
                        host: raw_config.host,
                        application: raw_config.application,
                        depends_on: raw_config.depends_on,
                        lifecycle: raw_config.lifecycle,
                    }),
                    location,
                    &mut diagnostics,
                );
            }
        }

        validate_resources(&mut resources, &parents, &locations, &mut diagnostics);
        validate_moves(&resources, &raw.moves, &raw.removed, &mut diagnostics);
        validate_removed(&resources, &raw.removed, &mut diagnostics);

        let mut moves: Vec<_> = raw.moves.into_iter().map(|item| item.value).collect();
        let mut removed: Vec<_> = raw.removed.into_iter().map(|item| item.value).collect();
        moves.sort();
        removed.sort();
        diagnostics.sort();

        if diagnostics.is_empty() {
            Ok(Self {
                version: raw.version,
                resources,
                parents,
                locations,
                moves,
                removed,
            })
        } else {
            let issues = diagnostics
                .iter()
                .map(|diagnostic| diagnostic.issue())
                .collect();
            Err(ConfigError::Invalid {
                issues,
                diagnostics,
            })
        }
    }
}

pub(crate) fn json_schema() -> Schema {
    schema_for!(RawDocument)
}

fn insert_resource(
    resources: &mut BTreeMap<ResourceAddress, ResourceConfig>,
    locations: &mut BTreeMap<ResourceAddress, SourceLocation>,
    address: ResourceAddress,
    config: ResourceConfig,
    location: SourceLocation,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> bool {
    match resources.entry(address.clone()) {
        Entry::Vacant(entry) => {
            entry.insert(config);
            locations.insert(address, location);
            true
        }
        Entry::Occupied(entry) => {
            emit(
                diagnostics,
                ValidationIssue::DuplicateResourceAddress {
                    kind: entry.key().kind(),
                },
                location,
            );
            false
        }
    }
}

fn insert_child_resource(
    tables: ResourceTables<'_>,
    address: Option<ResourceAddress>,
    parent: Option<&ResourceAddress>,
    config: ResourceConfig,
    location: SourceLocation,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let (resources, parents, locations) = tables;
    let (Some(address), Some(parent)) = (address, parent) else {
        return;
    };

    if insert_resource(
        resources,
        locations,
        address.clone(),
        config,
        location,
        diagnostics,
    ) {
        parents.insert(address, parent.clone());
    }
}

fn validate_resources(
    resources: &mut BTreeMap<ResourceAddress, ResourceConfig>,
    parents: &BTreeMap<ResourceAddress, ResourceAddress>,
    locations: &BTreeMap<ResourceAddress, SourceLocation>,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let addresses: BTreeSet<_> = resources.keys().cloned().collect();

    for (address, config) in resources.iter_mut() {
        let location = locations
            .get(address)
            .copied()
            .unwrap_or(SourceLocation::UNKNOWN);
        detect_duplicates(
            config.depends_on(),
            ValidationIssue::DuplicateDependency,
            location,
            diagnostics,
        );
        detect_duplicates(
            config.lifecycle().ignore_changes(),
            ValidationIssue::DuplicateIgnoredChange,
            location,
            diagnostics,
        );
        if config
            .lifecycle()
            .ignore_changes()
            .iter()
            .any(|path| !ignored_change_is_supported(address.kind(), path))
        {
            emit(diagnostics, ValidationIssue::InvalidIgnoredChange, location);
        }

        for dependency in config.depends_on() {
            if dependency == address {
                emit(diagnostics, ValidationIssue::SelfDependency, location);
            } else if !addresses.contains(dependency) {
                emit(diagnostics, ValidationIssue::MissingDependency, location);
            }
        }

        if !config.environment_names_valid() {
            emit(
                diagnostics,
                ValidationIssue::InvalidEnvironmentVariable,
                location,
            );
        }

        for source in config.secret_sources() {
            if source.is_unsafe_file() {
                emit(diagnostics, ValidationIssue::UnsafeSecretFile, location);
            } else if !source.is_safe() {
                emit(
                    diagnostics,
                    ValidationIssue::InvalidSecretEnvironment,
                    location,
                );
            }
        }

        for reference in config.references() {
            if !addresses.contains(reference.address()) {
                emit(diagnostics, ValidationIssue::MissingReference, location);
            } else if parents.get(address) != parents.get(reference.address()) {
                emit(
                    diagnostics,
                    ValidationIssue::CrossEnvironmentReference,
                    location,
                );
            } else if !output_is_supported(reference.address().kind(), reference.property()) {
                emit(
                    diagnostics,
                    ValidationIssue::UnsupportedReferenceProperty,
                    location,
                );
            }
        }

        if let ResourceConfig::Domain(domain) = config
            && let Field::Set(application) = &domain.application
            && (application.kind() != ResourceKind::Application || !addresses.contains(application))
        {
            emit(diagnostics, ValidationIssue::MissingReference, location);
        } else if let ResourceConfig::Domain(domain) = config
            && let Field::Set(application) = &domain.application
            && parents.get(address) != parents.get(application)
        {
            emit(
                diagnostics,
                ValidationIssue::CrossEnvironmentReference,
                location,
            );
        }

        if let ResourceConfig::LibSql(libsql) = config
            && matches!(
                &libsql.node,
                Field::Set(LibSqlNodeConfig::Replica { primary_url }) if primary_url.is_empty()
            )
        {
            emit(
                diagnostics,
                ValidationIssue::InvalidLibSqlPrimaryUrl,
                location,
            );
        }

        if let ResourceConfig::Compose(compose) = config
            && matches!(compose.document, Field::Clear)
        {
            emit(
                diagnostics,
                ValidationIssue::ComposeDocumentCannotBeCleared,
                location,
            );
        } else if let ResourceConfig::Compose(compose) = config
            && matches!(compose.document, Field::Unmanaged)
            && !matches!(compose.lifecycle.protect(), Field::Set(true))
        {
            emit(
                diagnostics,
                ValidationIssue::UnmanagedComposeDocumentRequiresProtection,
                location,
            );
        }

        config.depends_on_mut().sort();
        config.lifecycle_mut().normalize();
    }
}

fn validate_moves(
    resources: &BTreeMap<ResourceAddress, ResourceConfig>,
    moves: &[Spanned<MoveDeclaration>],
    removed: &[Spanned<RemovedDeclaration>],
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let mut sources = BTreeSet::new();
    let mut targets = BTreeSet::new();
    let removed_addresses: BTreeSet<_> = removed.iter().map(|item| item.value.from()).collect();

    for item in moves {
        let declaration = &item.value;
        let location = source_location(item.defined);
        if !sources.insert(declaration.from()) {
            emit(diagnostics, ValidationIssue::DuplicateMoveSource, location);
        }
        if !targets.insert(declaration.to()) {
            emit(diagnostics, ValidationIssue::DuplicateMoveTarget, location);
        }
        if declaration.from() == declaration.to()
            || declaration.from().kind() != declaration.to().kind()
        {
            emit(diagnostics, ValidationIssue::InvalidMove, location);
        }
        if resources.contains_key(declaration.from()) {
            emit(diagnostics, ValidationIssue::MoveSourceConfigured, location);
        }
        if !resources.contains_key(declaration.to()) {
            emit(diagnostics, ValidationIssue::MoveTargetMissing, location);
        }
        if removed_addresses.contains(declaration.from())
            || removed_addresses.contains(declaration.to())
        {
            emit(diagnostics, ValidationIssue::MoveRemovalConflict, location);
        }
    }
}

fn validate_removed(
    resources: &BTreeMap<ResourceAddress, ResourceConfig>,
    removed: &[Spanned<RemovedDeclaration>],
    diagnostics: &mut Vec<ValidationDiagnostic>,
) {
    let mut addresses = BTreeSet::new();
    for item in removed {
        let declaration = &item.value;
        let location = source_location(item.defined);
        if !addresses.insert(declaration.from()) {
            emit(diagnostics, ValidationIssue::DuplicateRemoval, location);
        }
        if resources.contains_key(declaration.from()) {
            emit(
                diagnostics,
                ValidationIssue::RemovedResourceConfigured,
                location,
            );
        }
    }
}

fn detect_duplicates<T>(
    values: &[T],
    issue: ValidationIssue,
    location: SourceLocation,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) where
    T: Ord,
{
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            emit(diagnostics, issue, location);
        }
    }
}

fn output_is_supported(kind: ResourceKind, property: &crate::PropertyPath) -> bool {
    let value = property.to_string();
    match kind {
        ResourceKind::Postgres | ResourceKind::MySql | ResourceKind::MariaDb => matches!(
            value.as_str(),
            "connection_url" | "host" | "port" | "database" | "username"
        ),
        ResourceKind::Mongo => {
            matches!(
                value.as_str(),
                "connection_url" | "host" | "port" | "username"
            )
        }
        ResourceKind::Redis => matches!(value.as_str(), "connection_url" | "host" | "port"),
        ResourceKind::Application => matches!(value.as_str(), "url"),
        ResourceKind::Project
        | ResourceKind::Environment
        | ResourceKind::Compose
        | ResourceKind::LibSql
        | ResourceKind::Domain
        | ResourceKind::Port => false,
    }
}

fn ignored_change_is_supported(kind: ResourceKind, property: &crate::PropertyPath) -> bool {
    let value = property.to_string();
    match kind {
        ResourceKind::Project | ResourceKind::Environment => value == "description",
        ResourceKind::Application => matches!(
            value.as_str(),
            "description"
                | "replicas"
                | "source.repository"
                | "source.branch"
                | "deployment.status"
        ),
        ResourceKind::Compose => value == "description",
        ResourceKind::Postgres | ResourceKind::MySql | ResourceKind::MariaDb => {
            matches!(value.as_str(), "database" | "username")
        }
        ResourceKind::Mongo => matches!(value.as_str(), "username" | "replica_sets"),
        ResourceKind::LibSql => matches!(value.as_str(), "description" | "username"),
        ResourceKind::Redis => false,
        ResourceKind::Domain => matches!(value.as_str(), "host" | "application"),
        ResourceKind::Port => matches!(
            value.as_str(),
            "published_port" | "target_port" | "publish_mode" | "protocol"
        ),
    }
}

fn version_schema(_generator: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "const": 1
    })
}

fn non_empty_string_schema(_generator: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "string",
        "minLength": 1
    })
}

fn source_location(location: serde_saphyr::Location) -> SourceLocation {
    SourceLocation::new(location.line(), location.column())
}

fn emit(
    diagnostics: &mut Vec<ValidationDiagnostic>,
    issue: ValidationIssue,
    location: SourceLocation,
) {
    diagnostics.push(ValidationDiagnostic::new(issue, location));
}
