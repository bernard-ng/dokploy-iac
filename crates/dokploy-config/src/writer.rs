use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::Path;

use dokploy_state::{ResourceAddress, ResourceKind, ResourceName};
use thiserror::Error;

use crate::{
    ConfigValue, DokployConfig, Field, Lifecycle, PropertyPath, ResourceConfig, SecretSource,
    SourceConfig,
};

/// A typed, nested document for constructing imported configuration safely.
///
/// Resources can only be added beneath an explicit environment. All optional
/// properties start unmanaged, including secret-bearing properties.
#[derive(Clone)]
pub struct ConfigDocument {
    project_name: ResourceName,
    project: ProjectDocument,
    environments: BTreeMap<ResourceName, EnvironmentDocument>,
    moves: Vec<(ResourceAddress, ResourceAddress)>,
    removed: Vec<(ResourceAddress, bool)>,
}

impl ConfigDocument {
    #[must_use]
    pub fn new(project_name: ResourceName) -> Self {
        Self {
            project_name,
            project: ProjectDocument::default(),
            environments: BTreeMap::new(),
            moves: Vec::new(),
            removed: Vec::new(),
        }
    }

    #[must_use]
    pub const fn project_mut(&mut self) -> &mut ProjectDocument {
        &mut self.project
    }

    pub fn add_environment(
        &mut self,
        name: ResourceName,
        environment: EnvironmentDocument,
    ) -> Result<(), ConfigDocumentError> {
        insert_resource(
            &mut self.environments,
            name,
            environment,
            ResourceKind::Environment,
        )
    }

    pub fn add_move(&mut self, from: ResourceAddress, to: ResourceAddress) {
        self.moves.push((from, to));
    }

    pub fn add_removed(&mut self, from: ResourceAddress, destroy: bool) {
        self.removed.push((from, destroy));
    }

    /// Validates this document through the strict parser and returns its model.
    pub fn to_config(&self) -> Result<DokployConfig, ConfigWriteError> {
        DokployConfig::parse(&render_document_unchecked(self))
            .map_err(ConfigWriteError::GeneratedConfig)
    }

    /// Renders this document in normalized canonical form.
    pub fn render(&self) -> Result<String, ConfigWriteError> {
        render(&self.to_config()?)
    }

    /// Atomically writes this document without replacing an existing file.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), ConfigWriteError> {
        write_source(path.as_ref(), &self.render()?)
    }

    /// Reconstructs a typed document from an already validated configuration.
    pub fn from_config(config: &DokployConfig) -> Result<Self, ConfigWriteError> {
        let (project_address, ResourceConfig::Project(project)) = config
            .resources
            .iter()
            .find(|(address, _)| address.kind() == ResourceKind::Project)
            .ok_or(ConfigWriteError::InconsistentModel)?
        else {
            return Err(ConfigWriteError::InconsistentModel);
        };
        let mut document = Self::new(project_address.name().clone());
        document.project = ProjectDocument {
            description: project.description.clone(),
            depends_on: project.depends_on.clone(),
            lifecycle: lifecycle_document(&project.lifecycle),
        };

        for (address, resource) in &config.resources {
            if let ResourceConfig::Environment(environment) = resource {
                document.environments.insert(
                    address.name().clone(),
                    EnvironmentDocument {
                        description: environment.description.clone(),
                        depends_on: environment.depends_on.clone(),
                        lifecycle: lifecycle_document(&environment.lifecycle),
                        ..EnvironmentDocument::default()
                    },
                );
            }
        }

        for (address, resource) in &config.resources {
            let Some(parent) = config.parents.get(address) else {
                if matches!(resource, ResourceConfig::Project(_)) {
                    continue;
                }

                return Err(ConfigWriteError::InconsistentModel);
            };
            if matches!(resource, ResourceConfig::Environment(_)) {
                continue;
            }
            let environment = document
                .environments
                .get_mut(parent.name())
                .ok_or(ConfigWriteError::InconsistentModel)?;

            match resource {
                ResourceConfig::Application(config) => {
                    environment.applications.insert(
                        address.name().clone(),
                        ApplicationDocument {
                            description: config.description.clone(),
                            replicas: config.replicas.clone(),
                            source: source_document(&config.source),
                            environment: config.environment.clone(),
                            depends_on: config.depends_on.clone(),
                            lifecycle: lifecycle_document(&config.lifecycle),
                        },
                    );
                }
                ResourceConfig::Postgres(config) => {
                    environment.postgres.insert(
                        address.name().clone(),
                        PostgresDocument {
                            database: config.database.clone(),
                            username: config.username.clone(),
                            password: config.password.clone(),
                            depends_on: config.depends_on.clone(),
                            lifecycle: lifecycle_document(&config.lifecycle),
                        },
                    );
                }
                ResourceConfig::Redis(config) => {
                    environment.redis.insert(
                        address.name().clone(),
                        RedisDocument {
                            password: config.password.clone(),
                            depends_on: config.depends_on.clone(),
                            lifecycle: lifecycle_document(&config.lifecycle),
                        },
                    );
                }
                ResourceConfig::Domain(config) => {
                    environment.domains.insert(
                        address.name().clone(),
                        DomainDocument {
                            host: config.host.clone(),
                            application: config.application.clone(),
                            depends_on: config.depends_on.clone(),
                            lifecycle: lifecycle_document(&config.lifecycle),
                        },
                    );
                }
                ResourceConfig::Project(_) | ResourceConfig::Environment(_) => {
                    return Err(ConfigWriteError::InconsistentModel);
                }
            }
        }

        document.moves = config
            .moves
            .iter()
            .map(|declaration| (declaration.from().clone(), declaration.to().clone()))
            .collect();
        document.removed = config
            .removed
            .iter()
            .map(|declaration| (declaration.from().clone(), declaration.destroy()))
            .collect();

        Ok(document)
    }
}

fn lifecycle_document(lifecycle: &Lifecycle) -> LifecycleDocument {
    LifecycleDocument {
        protect: lifecycle.protect().clone(),
        ignore_changes: lifecycle.ignore_changes().to_vec(),
    }
}

fn source_document(source: &Field<SourceConfig>) -> Field<SourceDocument> {
    match source {
        Field::Unmanaged => Field::Unmanaged,
        Field::Clear => Field::Clear,
        Field::Set(SourceConfig::GitHub(source)) => Field::Set(SourceDocument::GitHub {
            repository: source.repository().to_owned(),
            branch: source.branch().clone(),
        }),
    }
}

impl std::fmt::Debug for ConfigDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigDocument")
            .field("environment_count", &self.environments.len())
            .field("move_count", &self.moves.len())
            .field("removed_count", &self.removed.len())
            .finish_non_exhaustive()
    }
}

/// Project properties accepted by [`ConfigDocument`].
#[derive(Clone, Default)]
pub struct ProjectDocument {
    pub description: Field<String>,
    pub depends_on: Vec<ResourceAddress>,
    pub lifecycle: LifecycleDocument,
}

/// Environment properties and explicitly contained imported resources.
#[derive(Clone, Default)]
pub struct EnvironmentDocument {
    pub description: Field<String>,
    pub depends_on: Vec<ResourceAddress>,
    pub lifecycle: LifecycleDocument,
    applications: BTreeMap<ResourceName, ApplicationDocument>,
    postgres: BTreeMap<ResourceName, PostgresDocument>,
    redis: BTreeMap<ResourceName, RedisDocument>,
    domains: BTreeMap<ResourceName, DomainDocument>,
}

impl EnvironmentDocument {
    pub fn add_application(
        &mut self,
        name: ResourceName,
        application: ApplicationDocument,
    ) -> Result<(), ConfigDocumentError> {
        insert_resource(
            &mut self.applications,
            name,
            application,
            ResourceKind::Application,
        )
    }

    pub fn add_postgres(
        &mut self,
        name: ResourceName,
        postgres: PostgresDocument,
    ) -> Result<(), ConfigDocumentError> {
        insert_resource(&mut self.postgres, name, postgres, ResourceKind::Postgres)
    }

    pub fn add_redis(
        &mut self,
        name: ResourceName,
        redis: RedisDocument,
    ) -> Result<(), ConfigDocumentError> {
        insert_resource(&mut self.redis, name, redis, ResourceKind::Redis)
    }

    pub fn add_domain(
        &mut self,
        name: ResourceName,
        domain: DomainDocument,
    ) -> Result<(), ConfigDocumentError> {
        insert_resource(&mut self.domains, name, domain, ResourceKind::Domain)
    }
}

/// Application properties accepted by an imported document.
#[derive(Clone, Default)]
pub struct ApplicationDocument {
    pub description: Field<String>,
    pub replicas: Field<u32>,
    pub source: Field<SourceDocument>,
    pub environment: Field<BTreeMap<String, Field<ConfigValue>>>,
    pub depends_on: Vec<ResourceAddress>,
    pub lifecycle: LifecycleDocument,
}

/// PostgreSQL properties accepted by an imported document.
#[derive(Clone, Default)]
pub struct PostgresDocument {
    pub database: Field<String>,
    pub username: Field<String>,
    pub password: Field<SecretSource>,
    pub depends_on: Vec<ResourceAddress>,
    pub lifecycle: LifecycleDocument,
}

/// Redis properties accepted by an imported document.
#[derive(Clone, Default)]
pub struct RedisDocument {
    pub password: Field<SecretSource>,
    pub depends_on: Vec<ResourceAddress>,
    pub lifecycle: LifecycleDocument,
}

/// Domain properties accepted by an imported document.
#[derive(Clone, Default)]
pub struct DomainDocument {
    pub host: Field<String>,
    pub application: Field<ResourceAddress>,
    pub depends_on: Vec<ResourceAddress>,
    pub lifecycle: LifecycleDocument,
}

/// Generic lifecycle properties accepted by an imported document.
#[derive(Clone, Default)]
pub struct LifecycleDocument {
    pub protect: Field<bool>,
    pub ignore_changes: Vec<PropertyPath>,
}

/// Application source properties accepted by an imported document.
#[derive(Clone)]
pub enum SourceDocument {
    GitHub {
        repository: String,
        branch: Field<String>,
    },
}

fn insert_resource<T>(
    resources: &mut BTreeMap<ResourceName, T>,
    name: ResourceName,
    resource: T,
    kind: ResourceKind,
) -> Result<(), ConfigDocumentError> {
    match resources.entry(name) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(resource);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(_) => {
            Err(ConfigDocumentError::DuplicateResource { kind })
        }
    }
}

/// Renders a deterministic, nested `dokploy.yaml` document.
///
/// The result is reparsed through [`DokployConfig::parse`] before it is
/// returned. Source comments are not retained because the normalized model
/// deliberately does not store them.
pub fn render(config: &DokployConfig) -> Result<String, ConfigWriteError> {
    let source = render_unchecked(config)?;
    let reparsed = DokployConfig::parse(&source).map_err(ConfigWriteError::GeneratedConfig)?;

    if &reparsed != config {
        return Err(ConfigWriteError::RoundTripMismatch);
    }

    Ok(source)
}

/// Atomically writes canonical `dokploy.yaml` without replacing an existing file.
///
/// Rendering and strict-parser validation complete before the destination is
/// touched. Like [`render`], this canonical rewrite cannot retain source
/// comments.
pub fn write(path: impl AsRef<Path>, config: &DokployConfig) -> Result<(), ConfigWriteError> {
    let source = render(config)?;
    write_source(path.as_ref(), &source)
}

fn write_source(path: &Path, source: &str) -> Result<(), ConfigWriteError> {
    let parent = existing_parent(path);
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|source| ConfigWriteError::Create { source })?;

    temporary
        .write_all(source.as_bytes())
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| ConfigWriteError::Create { source })?;

    temporary
        .persist_noclobber(path)
        .map_err(|error| match error.error.kind() {
            io::ErrorKind::AlreadyExists => ConfigWriteError::AlreadyExists,
            _ => ConfigWriteError::Create {
                source: error.error,
            },
        })?;

    sync_parent(parent)
}

fn render_document_unchecked(document: &ConfigDocument) -> String {
    let mut output = String::from("version: 1\nproject:\n");

    line(
        &mut output,
        2,
        "name",
        &quoted(document.project_name.as_str()),
    );
    string_field(&mut output, 2, "description", &document.project.description);
    document_common_fields(
        &mut output,
        2,
        &document.project.depends_on,
        &document.project.lifecycle,
    );

    if document.environments.is_empty() {
        output.push_str("environments: {}\n");
    } else {
        output.push_str("environments:\n");
    }

    for (name, environment) in &document.environments {
        let item_start = output.len();
        mapping_header(&mut output, 2, name.as_str());
        string_field(&mut output, 4, "description", &environment.description);
        document_common_fields(
            &mut output,
            4,
            &environment.depends_on,
            &environment.lifecycle,
        );
        render_document_children(
            &mut output,
            "applications",
            &environment.applications,
            render_application_document,
        );
        render_document_children(
            &mut output,
            "postgres",
            &environment.postgres,
            render_postgres_document,
        );
        render_document_children(
            &mut output,
            "redis",
            &environment.redis,
            render_redis_document,
        );
        render_document_children(
            &mut output,
            "domains",
            &environment.domains,
            render_domain_document,
        );
        collapse_empty_mapping(&mut output, item_start, 2, name.as_str());
    }

    if !document.moves.is_empty() {
        output.push_str("moves:\n");
        for (from, to) in &document.moves {
            line(&mut output, 2, "- from", &quoted(&from.to_string()));
            line(&mut output, 4, "to", &quoted(&to.to_string()));
        }
    }

    if !document.removed.is_empty() {
        output.push_str("removed:\n");
        for (from, destroy) in &document.removed {
            line(&mut output, 2, "- from", &quoted(&from.to_string()));
            line(
                &mut output,
                4,
                "destroy",
                if *destroy { "true" } else { "false" },
            );
        }
    }

    output
}

fn render_document_children<T>(
    output: &mut String,
    collection_name: &str,
    resources: &BTreeMap<ResourceName, T>,
    renderer: fn(&mut String, usize, &T),
) {
    if resources.is_empty() {
        return;
    }

    mapping_header(output, 4, collection_name);
    for (name, resource) in resources {
        let item_start = output.len();
        mapping_header(output, 6, name.as_str());
        renderer(output, 8, resource);
        collapse_empty_mapping(output, item_start, 6, name.as_str());
    }
}

fn render_application_document(output: &mut String, indent: usize, config: &ApplicationDocument) {
    string_field(output, indent, "description", &config.description);
    u32_field(output, indent, "replicas", &config.replicas);
    document_source_field(output, indent, &config.source);
    environment_field(output, indent, &config.environment);
    document_common_fields(output, indent, &config.depends_on, &config.lifecycle);
}

fn render_postgres_document(output: &mut String, indent: usize, config: &PostgresDocument) {
    string_field(output, indent, "database", &config.database);
    string_field(output, indent, "username", &config.username);
    secret_field(output, indent, "password", &config.password);
    document_common_fields(output, indent, &config.depends_on, &config.lifecycle);
}

fn render_redis_document(output: &mut String, indent: usize, config: &RedisDocument) {
    secret_field(output, indent, "password", &config.password);
    document_common_fields(output, indent, &config.depends_on, &config.lifecycle);
}

fn render_domain_document(output: &mut String, indent: usize, config: &DomainDocument) {
    string_field(output, indent, "host", &config.host);
    address_field(output, indent, "application", &config.application);
    document_common_fields(output, indent, &config.depends_on, &config.lifecycle);
}

fn document_common_fields(
    output: &mut String,
    indent: usize,
    dependencies: &[ResourceAddress],
    lifecycle: &LifecycleDocument,
) {
    if !dependencies.is_empty() {
        mapping_header(output, indent, "depends_on");
        for dependency in dependencies {
            sequence_scalar(output, indent + 2, &quoted(&dependency.to_string()));
        }
    }

    if !matches!(lifecycle.protect, Field::Unmanaged) || !lifecycle.ignore_changes.is_empty() {
        mapping_header(output, indent, "lifecycle");
        bool_field(output, indent + 2, "protect", &lifecycle.protect);
        if !lifecycle.ignore_changes.is_empty() {
            mapping_header(output, indent + 2, "ignore_changes");
            for property in &lifecycle.ignore_changes {
                sequence_scalar(output, indent + 4, &quoted(&property.to_string()));
            }
        }
    }
}

fn document_source_field(output: &mut String, indent: usize, source: &Field<SourceDocument>) {
    match source {
        Field::Unmanaged => {}
        Field::Clear => line(output, indent, "source", "null"),
        Field::Set(SourceDocument::GitHub { repository, branch }) => {
            mapping_header(output, indent, "source");
            line(output, indent + 2, "type", &quoted("github"));
            line(output, indent + 2, "repository", &quoted(repository));
            string_field(output, indent + 2, "branch", branch);
        }
    }
}

fn render_unchecked(config: &DokployConfig) -> Result<String, ConfigWriteError> {
    let (project_address, ResourceConfig::Project(project)) = config
        .resources
        .iter()
        .find(|(address, _)| address.kind() == ResourceKind::Project)
        .ok_or(ConfigWriteError::InconsistentModel)?
    else {
        return Err(ConfigWriteError::InconsistentModel);
    };
    let mut output = String::from("version: 1\nproject:\n");

    line(
        &mut output,
        2,
        "name",
        &quoted(project_address.name().as_str()),
    );
    string_field(&mut output, 2, "description", &project.description);
    common_fields(&mut output, 2, project.depends_on(), project.lifecycle());

    let environments = config
        .resources
        .iter()
        .filter(|(address, resource)| {
            address.kind() == ResourceKind::Environment
                && matches!(resource, ResourceConfig::Environment(_))
        })
        .collect::<Vec<_>>();

    if environments.is_empty() {
        output.push_str("environments: {}\n");
    } else {
        output.push_str("environments:\n");
    }

    for (environment_address, resource) in environments {
        let ResourceConfig::Environment(environment) = resource else {
            return Err(ConfigWriteError::InconsistentModel);
        };
        let item_start = output.len();
        mapping_header(&mut output, 2, environment_address.name().as_str());
        string_field(&mut output, 4, "description", &environment.description);
        common_fields(
            &mut output,
            4,
            environment.depends_on(),
            environment.lifecycle(),
        );

        render_children(
            &mut output,
            config,
            environment_address,
            ResourceKind::Application,
            "applications",
            render_application,
        )?;
        render_children(
            &mut output,
            config,
            environment_address,
            ResourceKind::Postgres,
            "postgres",
            render_postgres,
        )?;
        render_children(
            &mut output,
            config,
            environment_address,
            ResourceKind::Redis,
            "redis",
            render_redis,
        )?;
        render_children(
            &mut output,
            config,
            environment_address,
            ResourceKind::Domain,
            "domains",
            render_domain,
        )?;
        collapse_empty_mapping(
            &mut output,
            item_start,
            2,
            environment_address.name().as_str(),
        );
    }

    if !config.moves.is_empty() {
        output.push_str("moves:\n");
        for declaration in &config.moves {
            line(
                &mut output,
                2,
                "- from",
                &quoted(&declaration.from().to_string()),
            );
            line(&mut output, 4, "to", &quoted(&declaration.to().to_string()));
        }
    }

    if !config.removed.is_empty() {
        output.push_str("removed:\n");
        for declaration in &config.removed {
            line(
                &mut output,
                2,
                "- from",
                &quoted(&declaration.from().to_string()),
            );
            line(
                &mut output,
                4,
                "destroy",
                if declaration.destroy() {
                    "true"
                } else {
                    "false"
                },
            );
        }
    }

    Ok(output)
}

type ResourceRenderer = fn(&mut String, usize, &ResourceConfig) -> Result<(), ConfigWriteError>;

fn render_children(
    output: &mut String,
    config: &DokployConfig,
    parent: &ResourceAddress,
    kind: ResourceKind,
    collection_name: &str,
    renderer: ResourceRenderer,
) -> Result<(), ConfigWriteError> {
    let children = config
        .resources
        .iter()
        .filter(|(address, _)| {
            address.kind() == kind && config.parents.get(*address) == Some(parent)
        })
        .collect::<Vec<_>>();

    if children.is_empty() {
        return Ok(());
    }

    mapping_header(output, 4, collection_name);
    for (address, resource) in children {
        let item_start = output.len();
        mapping_header(output, 6, address.name().as_str());
        renderer(output, 8, resource)?;
        collapse_empty_mapping(output, item_start, 6, address.name().as_str());
    }

    Ok(())
}

fn render_application(
    output: &mut String,
    indent: usize,
    resource: &ResourceConfig,
) -> Result<(), ConfigWriteError> {
    let ResourceConfig::Application(config) = resource else {
        return Err(ConfigWriteError::InconsistentModel);
    };

    string_field(output, indent, "description", &config.description);
    u32_field(output, indent, "replicas", &config.replicas);
    source_field(output, indent, &config.source);
    environment_field(output, indent, &config.environment);
    common_fields(output, indent, config.depends_on(), config.lifecycle());

    Ok(())
}

fn render_postgres(
    output: &mut String,
    indent: usize,
    resource: &ResourceConfig,
) -> Result<(), ConfigWriteError> {
    let ResourceConfig::Postgres(config) = resource else {
        return Err(ConfigWriteError::InconsistentModel);
    };

    string_field(output, indent, "database", &config.database);
    string_field(output, indent, "username", &config.username);
    secret_field(output, indent, "password", &config.password);
    common_fields(output, indent, resource.depends_on(), resource.lifecycle());

    Ok(())
}

fn render_redis(
    output: &mut String,
    indent: usize,
    resource: &ResourceConfig,
) -> Result<(), ConfigWriteError> {
    let ResourceConfig::Redis(config) = resource else {
        return Err(ConfigWriteError::InconsistentModel);
    };

    secret_field(output, indent, "password", &config.password);
    common_fields(output, indent, resource.depends_on(), resource.lifecycle());

    Ok(())
}

fn render_domain(
    output: &mut String,
    indent: usize,
    resource: &ResourceConfig,
) -> Result<(), ConfigWriteError> {
    let ResourceConfig::Domain(config) = resource else {
        return Err(ConfigWriteError::InconsistentModel);
    };

    string_field(output, indent, "host", &config.host);
    address_field(output, indent, "application", &config.application);
    common_fields(output, indent, resource.depends_on(), resource.lifecycle());

    Ok(())
}

fn common_fields(
    output: &mut String,
    indent: usize,
    dependencies: &[ResourceAddress],
    lifecycle: &Lifecycle,
) {
    if !dependencies.is_empty() {
        mapping_header(output, indent, "depends_on");
        for dependency in dependencies {
            sequence_scalar(output, indent + 2, &quoted(&dependency.to_string()));
        }
    }

    if !matches!(lifecycle.protect(), Field::Unmanaged) || !lifecycle.ignore_changes().is_empty() {
        mapping_header(output, indent, "lifecycle");
        bool_field(output, indent + 2, "protect", lifecycle.protect());
        if !lifecycle.ignore_changes().is_empty() {
            mapping_header(output, indent + 2, "ignore_changes");
            for property in lifecycle.ignore_changes() {
                sequence_scalar(output, indent + 4, &quoted(&property.to_string()));
            }
        }
    }
}

fn source_field(output: &mut String, indent: usize, source: &Field<SourceConfig>) {
    match source {
        Field::Unmanaged => {}
        Field::Clear => line(output, indent, "source", "null"),
        Field::Set(SourceConfig::GitHub(source)) => {
            mapping_header(output, indent, "source");
            line(output, indent + 2, "type", &quoted("github"));
            line(
                output,
                indent + 2,
                "repository",
                &quoted(source.repository()),
            );
            string_field(output, indent + 2, "branch", source.branch());
        }
    }
}

fn environment_field(
    output: &mut String,
    indent: usize,
    environment: &Field<std::collections::BTreeMap<String, Field<ConfigValue>>>,
) {
    match environment {
        Field::Unmanaged => {}
        Field::Clear => line(output, indent, "environment", "null"),
        Field::Set(values) if values.is_empty() => line(output, indent, "environment", "{}"),
        Field::Set(values) => {
            mapping_header(output, indent, "environment");
            for (name, value) in values {
                match value {
                    Field::Unmanaged => {}
                    Field::Clear => line(output, indent + 2, name, "null"),
                    Field::Set(ConfigValue::Literal(value)) => {
                        mapping_header(output, indent + 2, name);
                        line(output, indent + 4, "value", &quoted(value));
                    }
                    Field::Set(ConfigValue::Reference(reference)) => {
                        mapping_header(output, indent + 2, name);
                        line(
                            output,
                            indent + 4,
                            "from",
                            &quoted(&reference_text(reference)),
                        );
                    }
                    Field::Set(ConfigValue::Secret(secret)) => {
                        mapping_header(output, indent + 2, name);
                        mapping_header(output, indent + 4, "secret");
                        render_secret(output, indent + 6, secret);
                    }
                }
            }
        }
    }
}

fn reference_text(reference: &crate::ResourceReference) -> String {
    format!("{}.{}", reference.address(), reference.property())
}

fn string_field(output: &mut String, indent: usize, name: &str, field: &Field<String>) {
    match field {
        Field::Unmanaged => {}
        Field::Clear => line(output, indent, name, "null"),
        Field::Set(value) => line(output, indent, name, &quoted(value)),
    }
}

fn u32_field(output: &mut String, indent: usize, name: &str, field: &Field<u32>) {
    match field {
        Field::Unmanaged => {}
        Field::Clear => line(output, indent, name, "null"),
        Field::Set(value) => line(output, indent, name, &value.to_string()),
    }
}

fn bool_field(output: &mut String, indent: usize, name: &str, field: &Field<bool>) {
    match field {
        Field::Unmanaged => {}
        Field::Clear => line(output, indent, name, "null"),
        Field::Set(value) => line(output, indent, name, if *value { "true" } else { "false" }),
    }
}

fn address_field(output: &mut String, indent: usize, name: &str, field: &Field<ResourceAddress>) {
    match field {
        Field::Unmanaged => {}
        Field::Clear => line(output, indent, name, "null"),
        Field::Set(value) => line(output, indent, name, &quoted(&value.to_string())),
    }
}

fn secret_field(output: &mut String, indent: usize, name: &str, field: &Field<SecretSource>) {
    match field {
        Field::Unmanaged => {}
        Field::Clear => line(output, indent, name, "null"),
        Field::Set(secret) => {
            mapping_header(output, indent, name);
            render_secret(output, indent + 2, secret);
        }
    }
}

fn render_secret(output: &mut String, indent: usize, secret: &SecretSource) {
    match secret {
        SecretSource::Env(value) => line(output, indent, "env", &quoted(value)),
        SecretSource::File(value) => line(output, indent, "file", &quoted(value)),
    }
}

fn collapse_empty_mapping(output: &mut String, start: usize, indent: usize, name: &str) {
    if output[start..].lines().count() == 1 {
        output.truncate(start);
        line(output, indent, name, "{}");
    }
}

fn mapping_header(output: &mut String, indent: usize, name: &str) {
    let _ = writeln!(output, "{}{}:", " ".repeat(indent), name);
}

fn sequence_scalar(output: &mut String, indent: usize, value: &str) {
    let _ = writeln!(output, "{}- {value}", " ".repeat(indent));
}

fn line(output: &mut String, indent: usize, name: &str, value: &str) {
    let _ = writeln!(output, "{}{}: {value}", " ".repeat(indent), name);
}

fn quoted(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');

    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                let _ = write!(output, "\\u{:04X}", u32::from(character));
            }
            character => output.push(character),
        }
    }

    output.push('"');
    output
}

fn existing_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), ConfigWriteError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ConfigWriteError::DurabilityUnknown { source })
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), ConfigWriteError> {
    Ok(())
}

/// A typed imported document contains a duplicate logical resource.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ConfigDocumentError {
    #[error("the imported document contains a duplicate {kind} resource")]
    DuplicateResource { kind: ResourceKind },
}

/// Canonical rendering or durable file creation failed.
#[derive(Debug, Error)]
pub enum ConfigWriteError {
    #[error("the configuration file already exists")]
    AlreadyExists,

    #[error("failed to create the configuration file")]
    Create {
        #[source]
        source: io::Error,
    },

    #[error("the configuration file was created, but its durability could not be confirmed")]
    DurabilityUnknown {
        #[source]
        source: io::Error,
    },

    #[error("generated configuration failed strict validation")]
    GeneratedConfig(#[source] crate::ConfigError),

    #[error("generated configuration did not preserve the normalized model")]
    RoundTripMismatch,

    #[error("configuration model is internally inconsistent")]
    InconsistentModel,
}
