use std::{collections::BTreeMap, fmt};

use dokploy_state::{ResourceAddress, ResourceKind, ResourceName};
use schemars::Schema;
use thiserror::Error;

use crate::{
    ConfigValue, DomainConfig, Field, Lifecycle, MoveDeclaration, RemovedDeclaration, SecretSource,
    SourceConfig,
};

/// A fully parsed and semantically validated `dokploy.yaml` document.
#[derive(Clone)]
pub struct DokployConfig {
    pub(crate) version: u32,
    pub(crate) resources: BTreeMap<ResourceAddress, ResourceConfig>,
    pub(crate) parents: BTreeMap<ResourceAddress, ResourceAddress>,
    pub(crate) locations: BTreeMap<ResourceAddress, SourceLocation>,
    pub(crate) moves: Vec<MoveDeclaration>,
    pub(crate) removed: Vec<RemovedDeclaration>,
}

impl PartialEq for DokployConfig {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.resources == other.resources
            && self.parents == other.parents
            && self.moves == other.moves
            && self.removed == other.removed
    }
}

impl Eq for DokployConfig {}

impl DokployConfig {
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    #[must_use]
    pub const fn resources(&self) -> &BTreeMap<ResourceAddress, ResourceConfig> {
        &self.resources
    }

    #[must_use]
    pub fn resource(&self, address: &ResourceAddress) -> Option<&ResourceConfig> {
        self.resources.get(address)
    }

    /// Returns the containing project or environment for a nested resource.
    #[must_use]
    pub fn parent_of(&self, address: &ResourceAddress) -> Option<&ResourceAddress> {
        self.parents.get(address)
    }

    /// Returns all containment edges in stable child-address order.
    #[must_use]
    pub const fn parents(&self) -> &BTreeMap<ResourceAddress, ResourceAddress> {
        &self.parents
    }

    /// Returns a redaction-safe source location for a configured resource.
    #[must_use]
    pub fn location_of(&self, address: &ResourceAddress) -> Option<SourceLocation> {
        self.locations.get(address).copied()
    }

    #[must_use]
    pub fn moves(&self) -> &[MoveDeclaration] {
        &self.moves
    }

    #[must_use]
    pub fn removed(&self) -> &[RemovedDeclaration] {
        &self.removed
    }

    #[must_use]
    pub fn json_schema() -> Schema {
        crate::parser::json_schema()
    }
}

impl fmt::Debug for DokployConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DokployConfig")
            .field("version", &self.version)
            .field("resource_count", &self.resources.len())
            .field("parent_count", &self.parents.len())
            .field("move_count", &self.moves.len())
            .field("removed_count", &self.removed.len())
            .finish()
    }
}

/// The initial declarative resource variants.
#[derive(Clone, Eq, PartialEq)]
pub enum ResourceConfig {
    Project(ProjectConfig),
    Environment(EnvironmentConfig),
    Application(ApplicationConfig),
    Postgres(PostgresConfig),
    Redis(RedisConfig),
    Domain(DomainConfig),
}

impl ResourceConfig {
    #[must_use]
    pub const fn kind(&self) -> ResourceKind {
        match self {
            Self::Project(_) => ResourceKind::Project,
            Self::Environment(_) => ResourceKind::Environment,
            Self::Application(_) => ResourceKind::Application,
            Self::Postgres(_) => ResourceKind::Postgres,
            Self::Redis(_) => ResourceKind::Redis,
            Self::Domain(_) => ResourceKind::Domain,
        }
    }

    #[must_use]
    pub const fn as_project(&self) -> Option<&ProjectConfig> {
        match self {
            Self::Project(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_application(&self) -> Option<&ApplicationConfig> {
        match self {
            Self::Application(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_environment(&self) -> Option<&EnvironmentConfig> {
        match self {
            Self::Environment(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_postgres(&self) -> Option<&PostgresConfig> {
        match self {
            Self::Postgres(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_redis(&self) -> Option<&RedisConfig> {
        match self {
            Self::Redis(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_domain(&self) -> Option<&DomainConfig> {
        match self {
            Self::Domain(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub fn depends_on(&self) -> &[ResourceAddress] {
        match self {
            Self::Project(config) => &config.depends_on,
            Self::Environment(config) => &config.depends_on,
            Self::Application(config) => &config.depends_on,
            Self::Postgres(config) => &config.depends_on,
            Self::Redis(config) => &config.depends_on,
            Self::Domain(config) => &config.depends_on,
        }
    }

    #[must_use]
    pub const fn lifecycle(&self) -> &Lifecycle {
        match self {
            Self::Project(config) => &config.lifecycle,
            Self::Environment(config) => &config.lifecycle,
            Self::Application(config) => &config.lifecycle,
            Self::Postgres(config) => &config.lifecycle,
            Self::Redis(config) => &config.lifecycle,
            Self::Domain(config) => &config.lifecycle,
        }
    }

    pub(crate) fn lifecycle_mut(&mut self) -> &mut Lifecycle {
        match self {
            Self::Project(config) => &mut config.lifecycle,
            Self::Environment(config) => &mut config.lifecycle,
            Self::Application(config) => &mut config.lifecycle,
            Self::Postgres(config) => &mut config.lifecycle,
            Self::Redis(config) => &mut config.lifecycle,
            Self::Domain(config) => &mut config.lifecycle,
        }
    }

    pub(crate) fn depends_on_mut(&mut self) -> &mut Vec<ResourceAddress> {
        match self {
            Self::Project(config) => &mut config.depends_on,
            Self::Environment(config) => &mut config.depends_on,
            Self::Application(config) => &mut config.depends_on,
            Self::Postgres(config) => &mut config.depends_on,
            Self::Redis(config) => &mut config.depends_on,
            Self::Domain(config) => &mut config.depends_on,
        }
    }

    pub(crate) fn references(&self) -> Vec<&crate::ResourceReference> {
        match self {
            Self::Application(config) => {
                config.environment.as_set().map_or_else(Vec::new, |vars| {
                    vars.values()
                        .filter_map(|value| match value {
                            Field::Set(ConfigValue::Reference(reference)) => Some(reference),
                            Field::Unmanaged
                            | Field::Clear
                            | Field::Set(ConfigValue::Literal(_) | ConfigValue::Secret(_)) => None,
                        })
                        .collect()
                })
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn secret_sources(&self) -> Vec<&SecretSource> {
        let mut secrets = Vec::new();
        match self {
            Self::Application(config) => {
                if let Some(variables) = config.environment.as_set() {
                    secrets.extend(variables.values().filter_map(|value| match value {
                        Field::Set(ConfigValue::Secret(source)) => Some(source),
                        Field::Unmanaged
                        | Field::Clear
                        | Field::Set(ConfigValue::Literal(_) | ConfigValue::Reference(_)) => None,
                    }));
                }
            }
            Self::Postgres(config) => {
                if let Field::Set(secret) = &config.password {
                    secrets.push(secret);
                }
            }
            Self::Redis(config) => {
                if let Field::Set(secret) = &config.password {
                    secrets.push(secret);
                }
            }
            Self::Project(_) | Self::Environment(_) | Self::Domain(_) => {}
        }
        secrets
    }

    pub(crate) fn environment_names_valid(&self) -> bool {
        match self {
            Self::Application(config) => config.environment.as_set().is_none_or(|variables| {
                variables
                    .keys()
                    .all(|name| crate::types::valid_environment_name(name))
            }),
            _ => true,
        }
    }
}

impl fmt::Debug for ResourceConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceConfig")
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

macro_rules! redacted_debug {
    ($type:ty, $name:literal) => {
        impl fmt::Debug for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!($name, "([REDACTED])"))
            }
        }
    };
}

#[derive(Clone, Eq, PartialEq)]
pub struct ProjectConfig {
    pub(crate) description: Field<String>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl ProjectConfig {
    #[must_use]
    pub const fn description(&self) -> &Field<String> {
        &self.description
    }

    #[must_use]
    pub fn depends_on(&self) -> &[ResourceAddress] {
        &self.depends_on
    }

    #[must_use]
    pub const fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }
}

redacted_debug!(ProjectConfig, "ProjectConfig");

#[derive(Clone, Eq, PartialEq)]
pub struct EnvironmentConfig {
    pub(crate) description: Field<String>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl EnvironmentConfig {
    #[must_use]
    pub const fn description(&self) -> &Field<String> {
        &self.description
    }

    #[must_use]
    pub fn depends_on(&self) -> &[ResourceAddress] {
        &self.depends_on
    }

    #[must_use]
    pub const fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }
}

redacted_debug!(EnvironmentConfig, "EnvironmentConfig");

#[derive(Clone, Eq, PartialEq)]
pub struct ApplicationConfig {
    pub(crate) description: Field<String>,
    pub(crate) replicas: Field<u32>,
    pub(crate) source: Field<SourceConfig>,
    pub(crate) environment: Field<BTreeMap<String, Field<ConfigValue>>>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl ApplicationConfig {
    #[must_use]
    pub const fn description(&self) -> &Field<String> {
        &self.description
    }

    #[must_use]
    pub const fn replicas(&self) -> &Field<u32> {
        &self.replicas
    }

    #[must_use]
    pub const fn source(&self) -> &Field<SourceConfig> {
        &self.source
    }

    #[must_use]
    pub const fn environment(&self) -> &Field<BTreeMap<String, Field<ConfigValue>>> {
        &self.environment
    }

    #[must_use]
    pub fn depends_on(&self) -> &[ResourceAddress] {
        &self.depends_on
    }

    #[must_use]
    pub const fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }
}

redacted_debug!(ApplicationConfig, "ApplicationConfig");

#[derive(Clone, Eq, PartialEq)]
pub struct PostgresConfig {
    pub(crate) database: Field<String>,
    pub(crate) username: Field<String>,
    pub(crate) password: Field<SecretSource>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl PostgresConfig {
    #[must_use]
    pub const fn database(&self) -> &Field<String> {
        &self.database
    }

    #[must_use]
    pub const fn username(&self) -> &Field<String> {
        &self.username
    }

    #[must_use]
    pub const fn password(&self) -> &Field<SecretSource> {
        &self.password
    }
}

redacted_debug!(PostgresConfig, "PostgresConfig");

#[derive(Clone, Eq, PartialEq)]
pub struct RedisConfig {
    pub(crate) password: Field<SecretSource>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl RedisConfig {
    #[must_use]
    pub const fn password(&self) -> &Field<SecretSource> {
        &self.password
    }
}

redacted_debug!(RedisConfig, "RedisConfig");

/// A stable, non-secret semantic issue code.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ValidationIssue {
    InvalidResourceName { kind: ResourceKind },
    DuplicateResourceAddress { kind: ResourceKind },
    DuplicateDependency,
    SelfDependency,
    MissingDependency,
    MissingReference,
    CrossEnvironmentReference,
    UnsupportedReferenceProperty,
    InvalidEnvironmentVariable,
    InvalidSecretEnvironment,
    UnsafeSecretFile,
    DuplicateIgnoredChange,
    InvalidIgnoredChange,
    DuplicateMoveSource,
    DuplicateMoveTarget,
    InvalidMove,
    MoveSourceConfigured,
    MoveTargetMissing,
    DuplicateRemoval,
    RemovedResourceConfigured,
    MoveRemovalConflict,
}

/// A one-indexed, redaction-safe location in `dokploy.yaml`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceLocation {
    line: u64,
    column: u64,
}

impl SourceLocation {
    pub(crate) const UNKNOWN: Self = Self { line: 0, column: 0 };

    pub(crate) const fn new(line: u64, column: u64) -> Self {
        Self { line, column }
    }

    #[must_use]
    pub const fn line(self) -> u64 {
        self.line
    }

    #[must_use]
    pub const fn column(self) -> u64 {
        self.column
    }
}

/// A semantic issue paired with source position but never raw source text.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ValidationDiagnostic {
    location: SourceLocation,
    issue: ValidationIssue,
}

impl ValidationDiagnostic {
    pub(crate) const fn new(issue: ValidationIssue, location: SourceLocation) -> Self {
        Self { location, issue }
    }

    #[must_use]
    pub const fn issue(self) -> ValidationIssue {
        self.issue
    }

    #[must_use]
    pub const fn location(self) -> SourceLocation {
        self.location
    }
}

/// Configuration parsing or validation failed without retaining source text.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("dokploy.yaml exceeds the {limit_bytes}-byte input limit")]
    InputTooLarge { limit_bytes: usize },
    #[error("dokploy.yaml is not valid at line {line}, column {column}")]
    Parse { line: u64, column: u64 },
    #[error("dokploy.yaml is not valid")]
    ParseWithoutLocation,
    #[error("dokploy.yaml version {found} is unsupported; expected version 1")]
    UnsupportedVersion { found: u32 },
    #[error("dokploy.yaml failed semantic validation")]
    Invalid {
        issues: Vec<ValidationIssue>,
        diagnostics: Vec<ValidationDiagnostic>,
    },
}

impl ConfigError {
    #[must_use]
    pub fn issues(&self) -> &[ValidationIssue] {
        match self {
            Self::Invalid { issues, .. } => issues,
            _ => &[],
        }
    }

    /// Returns redaction-safe semantic diagnostics with source positions.
    #[must_use]
    pub fn diagnostics(&self) -> &[ValidationDiagnostic] {
        match self {
            Self::Invalid { diagnostics, .. } => diagnostics,
            _ => &[],
        }
    }
}

pub(crate) fn address(
    kind: ResourceKind,
    raw_name: String,
    location: SourceLocation,
    diagnostics: &mut Vec<ValidationDiagnostic>,
) -> Option<ResourceAddress> {
    match ResourceName::new(raw_name) {
        Ok(name) => Some(ResourceAddress::new(kind, name)),
        Err(_) => {
            diagnostics.push(ValidationDiagnostic::new(
                ValidationIssue::InvalidResourceName { kind },
                location,
            ));
            None
        }
    }
}
