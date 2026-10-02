use std::{collections::BTreeMap, fmt};

use dokploy_state::{ResourceAddress, ResourceKind, ResourceName};
use schemars::Schema;
use thiserror::Error;

use crate::{
    ConfigValue, DomainConfig, ExternalSelector, Field, Lifecycle, MountSourceConfig,
    MoveDeclaration, NonEmptyText, PortNumber, PortProtocolConfig, PortPublishModeConfig,
    RemovedDeclaration, ScheduleShellConfig, SecretSource, SourceConfig,
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
    Compose(ComposeConfig),
    Postgres(PostgresConfig),
    MySql(MySqlConfig),
    MariaDb(MariaDbConfig),
    Mongo(MongoConfig),
    LibSql(LibSqlConfig),
    Redis(RedisConfig),
    Domain(DomainConfig),
    Port(PortConfig),
    Redirect(RedirectConfig),
    Security(SecurityConfig),
    Mount(MountConfig),
    Schedule(ScheduleConfig),
    Backup(BackupConfig),
}

impl ResourceConfig {
    #[must_use]
    pub const fn kind(&self) -> ResourceKind {
        match self {
            Self::Project(_) => ResourceKind::Project,
            Self::Environment(_) => ResourceKind::Environment,
            Self::Application(_) => ResourceKind::Application,
            Self::Compose(_) => ResourceKind::Compose,
            Self::Postgres(_) => ResourceKind::Postgres,
            Self::MySql(_) => ResourceKind::MySql,
            Self::MariaDb(_) => ResourceKind::MariaDb,
            Self::Mongo(_) => ResourceKind::Mongo,
            Self::LibSql(_) => ResourceKind::LibSql,
            Self::Redis(_) => ResourceKind::Redis,
            Self::Domain(_) => ResourceKind::Domain,
            Self::Port(_) => ResourceKind::Port,
            Self::Redirect(_) => ResourceKind::Redirect,
            Self::Security(_) => ResourceKind::Security,
            Self::Mount(_) => ResourceKind::Mount,
            Self::Schedule(_) => ResourceKind::Schedule,
            Self::Backup(_) => ResourceKind::Backup,
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
    pub const fn as_compose(&self) -> Option<&ComposeConfig> {
        match self {
            Self::Compose(config) => Some(config),
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
    pub const fn as_mysql(&self) -> Option<&MySqlConfig> {
        match self {
            Self::MySql(config) => Some(config),
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
    pub const fn as_mariadb(&self) -> Option<&MariaDbConfig> {
        match self {
            Self::MariaDb(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_mongo(&self) -> Option<&MongoConfig> {
        match self {
            Self::Mongo(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_libsql(&self) -> Option<&LibSqlConfig> {
        match self {
            Self::LibSql(config) => Some(config),
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
    pub const fn as_port(&self) -> Option<&PortConfig> {
        match self {
            Self::Port(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_redirect(&self) -> Option<&RedirectConfig> {
        match self {
            Self::Redirect(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_security(&self) -> Option<&SecurityConfig> {
        match self {
            Self::Security(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_mount(&self) -> Option<&MountConfig> {
        match self {
            Self::Mount(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_schedule(&self) -> Option<&ScheduleConfig> {
        match self {
            Self::Schedule(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_backup(&self) -> Option<&BackupConfig> {
        match self {
            Self::Backup(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub fn depends_on(&self) -> &[ResourceAddress] {
        match self {
            Self::Project(config) => &config.depends_on,
            Self::Environment(config) => &config.depends_on,
            Self::Application(config) => &config.depends_on,
            Self::Compose(config) => &config.depends_on,
            Self::Postgres(config) => &config.depends_on,
            Self::MySql(config) => &config.depends_on,
            Self::MariaDb(config) => &config.depends_on,
            Self::Mongo(config) => &config.depends_on,
            Self::LibSql(config) => &config.depends_on,
            Self::Redis(config) => &config.depends_on,
            Self::Domain(config) => &config.depends_on,
            Self::Port(config) => &config.depends_on,
            Self::Redirect(config) => &config.depends_on,
            Self::Security(config) => &config.depends_on,
            Self::Mount(config) => &config.depends_on,
            Self::Schedule(config) => &config.depends_on,
            Self::Backup(config) => &config.depends_on,
        }
    }

    #[must_use]
    pub const fn lifecycle(&self) -> &Lifecycle {
        match self {
            Self::Project(config) => &config.lifecycle,
            Self::Environment(config) => &config.lifecycle,
            Self::Application(config) => &config.lifecycle,
            Self::Compose(config) => &config.lifecycle,
            Self::Postgres(config) => &config.lifecycle,
            Self::MySql(config) => &config.lifecycle,
            Self::MariaDb(config) => &config.lifecycle,
            Self::Mongo(config) => &config.lifecycle,
            Self::LibSql(config) => &config.lifecycle,
            Self::Redis(config) => &config.lifecycle,
            Self::Domain(config) => &config.lifecycle,
            Self::Port(config) => &config.lifecycle,
            Self::Redirect(config) => &config.lifecycle,
            Self::Security(config) => &config.lifecycle,
            Self::Mount(config) => &config.lifecycle,
            Self::Schedule(config) => &config.lifecycle,
            Self::Backup(config) => &config.lifecycle,
        }
    }

    pub(crate) fn lifecycle_mut(&mut self) -> &mut Lifecycle {
        match self {
            Self::Project(config) => &mut config.lifecycle,
            Self::Environment(config) => &mut config.lifecycle,
            Self::Application(config) => &mut config.lifecycle,
            Self::Compose(config) => &mut config.lifecycle,
            Self::Postgres(config) => &mut config.lifecycle,
            Self::MySql(config) => &mut config.lifecycle,
            Self::MariaDb(config) => &mut config.lifecycle,
            Self::Mongo(config) => &mut config.lifecycle,
            Self::LibSql(config) => &mut config.lifecycle,
            Self::Redis(config) => &mut config.lifecycle,
            Self::Domain(config) => &mut config.lifecycle,
            Self::Port(config) => &mut config.lifecycle,
            Self::Redirect(config) => &mut config.lifecycle,
            Self::Security(config) => &mut config.lifecycle,
            Self::Mount(config) => &mut config.lifecycle,
            Self::Schedule(config) => &mut config.lifecycle,
            Self::Backup(config) => &mut config.lifecycle,
        }
    }

    pub(crate) fn depends_on_mut(&mut self) -> &mut Vec<ResourceAddress> {
        match self {
            Self::Project(config) => &mut config.depends_on,
            Self::Environment(config) => &mut config.depends_on,
            Self::Application(config) => &mut config.depends_on,
            Self::Compose(config) => &mut config.depends_on,
            Self::Postgres(config) => &mut config.depends_on,
            Self::MySql(config) => &mut config.depends_on,
            Self::MariaDb(config) => &mut config.depends_on,
            Self::Mongo(config) => &mut config.depends_on,
            Self::LibSql(config) => &mut config.depends_on,
            Self::Redis(config) => &mut config.depends_on,
            Self::Domain(config) => &mut config.depends_on,
            Self::Port(config) => &mut config.depends_on,
            Self::Redirect(config) => &mut config.depends_on,
            Self::Security(config) => &mut config.depends_on,
            Self::Mount(config) => &mut config.depends_on,
            Self::Schedule(config) => &mut config.depends_on,
            Self::Backup(config) => &mut config.depends_on,
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
            Self::Compose(config) => {
                if let Field::Set(secret) = &config.document {
                    secrets.push(secret);
                }
            }
            Self::Postgres(config) => {
                if let Field::Set(secret) = &config.password {
                    secrets.push(secret);
                }
            }
            Self::MySql(config) => {
                if let Field::Set(secret) = &config.password {
                    secrets.push(secret);
                }
                if let Field::Set(secret) = &config.root_password {
                    secrets.push(secret);
                }
            }
            Self::MariaDb(config) => {
                if let Field::Set(secret) = &config.password {
                    secrets.push(secret);
                }
                if let Field::Set(secret) = &config.root_password {
                    secrets.push(secret);
                }
            }
            Self::Mongo(config) => {
                if let Field::Set(secret) = &config.password {
                    secrets.push(secret);
                }
            }
            Self::LibSql(config) => {
                if let Field::Set(secret) = &config.password {
                    secrets.push(secret);
                }
            }
            Self::Redis(config) => {
                if let Field::Set(secret) = &config.password {
                    secrets.push(secret);
                }
            }
            Self::Security(config) => {
                if let Field::Set(secret) = &config.password {
                    secrets.push(secret);
                }
            }
            Self::Mount(config) => {
                if let MountSourceConfig::File {
                    content: Field::Set(secret),
                    ..
                } = &config.source
                {
                    secrets.push(secret);
                }
            }
            Self::Schedule(config) => {
                if let Field::Set(secret) = &config.command {
                    secrets.push(secret);
                }
                if let Field::Set(secret) = &config.script {
                    secrets.push(secret);
                }
            }
            Self::Project(_)
            | Self::Environment(_)
            | Self::Domain(_)
            | Self::Port(_)
            | Self::Redirect(_)
            | Self::Backup(_) => {}
        }
        secrets
    }

    /// Returns every configured external selector with its kind and property name.
    #[must_use]
    pub fn external_selectors(
        &self,
    ) -> Vec<(&'static str, crate::SelectorKind, &ExternalSelector)> {
        let mut selectors = Vec::new();
        if let Self::Application(config) = self {
            for (name, kind, field) in [
                ("server", crate::SelectorKind::Server, &config.server),
                (
                    "build_server",
                    crate::SelectorKind::Server,
                    &config.build_server,
                ),
                ("registry", crate::SelectorKind::Registry, &config.registry),
                (
                    "build_registry",
                    crate::SelectorKind::Registry,
                    &config.build_registry,
                ),
                (
                    "rollback_registry",
                    crate::SelectorKind::Registry,
                    &config.rollback_registry,
                ),
            ] {
                if let Field::Set(selector) = field {
                    selectors.push((name, kind, selector));
                }
            }
        }
        if let Some(Field::Set(selector)) = self.server_placement() {
            selectors.push(("server", crate::SelectorKind::Server, selector));
        }
        if let Self::Backup(config) = self {
            selectors.push((
                "destination",
                crate::SelectorKind::Destination,
                &config.destination,
            ));
        }
        selectors
    }

    /// Returns the create-only server placement field of a Compose or database service.
    ///
    /// Application declares its placement alongside its other associations.
    #[must_use]
    pub const fn server_placement(&self) -> Option<&Field<ExternalSelector>> {
        match self {
            Self::Compose(config) => Some(&config.server),
            Self::Postgres(config) => Some(&config.server),
            Self::MySql(config) => Some(&config.server),
            Self::MariaDb(config) => Some(&config.server),
            Self::Mongo(config) => Some(&config.server),
            Self::LibSql(config) => Some(&config.server),
            Self::Redis(config) => Some(&config.server),
            _ => None,
        }
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
    pub(crate) server: Field<ExternalSelector>,
    pub(crate) build_server: Field<ExternalSelector>,
    pub(crate) registry: Field<ExternalSelector>,
    pub(crate) build_registry: Field<ExternalSelector>,
    pub(crate) rollback_registry: Field<ExternalSelector>,
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

    /// Returns the create-only server placement selector.
    ///
    /// Dokploy accepts a server only when an application is created, so a
    /// changed placement replaces the application. `null` is rejected because
    /// local placement is the explicit `local` selector.
    #[must_use]
    pub const fn server(&self) -> &Field<ExternalSelector> {
        &self.server
    }

    /// Returns the nullable build-server selector, changeable in place.
    #[must_use]
    pub const fn build_server(&self) -> &Field<ExternalSelector> {
        &self.build_server
    }

    /// Returns the nullable runtime image registry selector, changeable in place.
    #[must_use]
    pub const fn registry(&self) -> &Field<ExternalSelector> {
        &self.registry
    }

    /// Returns the nullable build image registry selector, changeable in place.
    #[must_use]
    pub const fn build_registry(&self) -> &Field<ExternalSelector> {
        &self.build_registry
    }

    /// Returns the nullable rollback image registry selector, changeable in place.
    #[must_use]
    pub const fn rollback_registry(&self) -> &Field<ExternalSelector> {
        &self.rollback_registry
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

/// Complete declarative inputs for one application Port.
#[derive(Clone, Eq, PartialEq)]
pub struct PortConfig {
    pub(crate) published_port: PortNumber,
    pub(crate) target_port: PortNumber,
    pub(crate) publish_mode: PortPublishModeConfig,
    pub(crate) protocol: PortProtocolConfig,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl PortConfig {
    #[must_use]
    pub const fn published_port(&self) -> PortNumber {
        self.published_port
    }

    #[must_use]
    pub const fn target_port(&self) -> PortNumber {
        self.target_port
    }

    #[must_use]
    pub const fn publish_mode(&self) -> PortPublishModeConfig {
        self.publish_mode
    }

    #[must_use]
    pub const fn protocol(&self) -> PortProtocolConfig {
        self.protocol
    }
}

redacted_debug!(PortConfig, "PortConfig");

/// Complete declarative inputs for one application Redirect.
#[derive(Clone, Eq, PartialEq)]
pub struct RedirectConfig {
    pub(crate) regex: NonEmptyText,
    pub(crate) replacement: NonEmptyText,
    pub(crate) permanent: bool,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl RedirectConfig {
    /// Returns the regular expression, which is unique within one application.
    #[must_use]
    pub const fn regex(&self) -> &NonEmptyText {
        &self.regex
    }

    #[must_use]
    pub const fn replacement(&self) -> &NonEmptyText {
        &self.replacement
    }

    #[must_use]
    pub const fn permanent(&self) -> bool {
        self.permanent
    }
}

redacted_debug!(RedirectConfig, "RedirectConfig");

/// Declarative inputs for one application basic-auth entry.
///
/// The password is a descriptor only. It can be omitted (unmanaged) but never
/// cleared, because Dokploy has no password-clear operation.
#[derive(Clone, Eq, PartialEq)]
pub struct SecurityConfig {
    pub(crate) username: NonEmptyText,
    pub(crate) password: Field<SecretSource>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl SecurityConfig {
    /// Returns the username, which is unique within one application.
    #[must_use]
    pub const fn username(&self) -> &NonEmptyText {
        &self.username
    }

    #[must_use]
    pub const fn password(&self) -> &Field<SecretSource> {
        &self.password
    }
}

redacted_debug!(SecurityConfig, "SecurityConfig");
/// Complete declarative inputs for one Mount on an application, Compose
/// project, or supported database.
///
/// The target is an owned property and inferred dependency, not a containment
/// parent. Mounts are contained by their environment like Domains.
#[derive(Clone, Eq, PartialEq)]
pub struct MountConfig {
    pub(crate) target: ResourceAddress,
    pub(crate) mount_path: String,
    pub(crate) source: MountSourceConfig,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl MountConfig {
    /// Returns the logical address of the mounted service.
    #[must_use]
    pub const fn target(&self) -> &ResourceAddress {
        &self.target
    }

    /// Returns the absolute path inside the target service.
    #[must_use]
    pub fn mount_path(&self) -> &str {
        &self.mount_path
    }

    /// Returns the typed storage source.
    #[must_use]
    pub const fn source(&self) -> &MountSourceConfig {
        &self.source
    }
}

redacted_debug!(MountConfig, "MountConfig");

/// Complete declarative inputs for one Schedule on an application or one
/// Compose service.
///
/// The target is an owned property and inferred dependency, not a containment
/// parent. Schedules are contained by their environment like Mounts. The
/// command and script are descriptors only: they can be omitted (unmanaged,
/// which requires protection for the command) but never cleared.
#[derive(Clone, Eq, PartialEq)]
pub struct ScheduleConfig {
    pub(crate) name: String,
    pub(crate) target: ResourceAddress,
    pub(crate) service_name: Option<String>,
    pub(crate) cron_expression: String,
    pub(crate) shell_type: ScheduleShellConfig,
    pub(crate) enabled: bool,
    pub(crate) description: Field<String>,
    pub(crate) timezone: Field<String>,
    pub(crate) command: Field<SecretSource>,
    pub(crate) script: Field<SecretSource>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl ScheduleConfig {
    /// Returns the Dokploy Schedule name, the collision key within one target.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the logical address of the application or Compose target.
    #[must_use]
    pub const fn target(&self) -> &ResourceAddress {
        &self.target
    }

    /// Returns the Compose service name, present exactly for Compose targets.
    #[must_use]
    pub fn service_name(&self) -> Option<&str> {
        self.service_name.as_deref()
    }

    /// Returns the cron expression.
    #[must_use]
    pub fn cron_expression(&self) -> &str {
        &self.cron_expression
    }

    #[must_use]
    pub const fn shell_type(&self) -> ScheduleShellConfig {
        self.shell_type
    }

    /// Returns whether Dokploy may run the Schedule.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn description(&self) -> &Field<String> {
        &self.description
    }

    #[must_use]
    pub const fn timezone(&self) -> &Field<String> {
        &self.timezone
    }

    /// Returns the executable command descriptor, which is never a literal.
    #[must_use]
    pub const fn command(&self) -> &Field<SecretSource> {
        &self.command
    }

    /// Returns the optional script descriptor, which is never a literal.
    #[must_use]
    pub const fn script(&self) -> &Field<SecretSource> {
        &self.script
    }
}

redacted_debug!(ScheduleConfig, "ScheduleConfig");

/// Complete declarative inputs for one database Backup policy.
///
/// The typed database target is an owned property and inferred dependency, not
/// a containment parent: Backups are contained by their environment like
/// Mounts. The destination is an external record selected by exact name and is
/// never owned by the workspace.
#[derive(Clone, Eq, PartialEq)]
pub struct BackupConfig {
    pub(crate) target: ResourceAddress,
    pub(crate) destination: ExternalSelector,
    pub(crate) schedule: String,
    pub(crate) prefix: String,
    pub(crate) database: String,
    pub(crate) enabled: bool,
    pub(crate) keep_latest: Field<u32>,
    pub(crate) include_encryption_key: bool,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl BackupConfig {
    /// Returns the logical address of the backed-up database.
    #[must_use]
    pub const fn target(&self) -> &ResourceAddress {
        &self.target
    }

    /// Returns the external backup-destination selector.
    #[must_use]
    pub const fn destination(&self) -> &ExternalSelector {
        &self.destination
    }

    /// Returns the cron expression that schedules the Backup.
    #[must_use]
    pub fn schedule(&self) -> &str {
        &self.schedule
    }

    /// Returns the destination object prefix.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Returns the name of the database inside the target service.
    #[must_use]
    pub fn database(&self) -> &str {
        &self.database
    }

    /// Returns whether Dokploy runs the Backup on its schedule.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Returns the nullable retention count; `null` keeps every backup.
    #[must_use]
    pub const fn keep_latest(&self) -> &Field<u32> {
        &self.keep_latest
    }

    /// Returns whether Dokploy includes its encryption key in the backup.
    #[must_use]
    pub const fn include_encryption_key(&self) -> bool {
        self.include_encryption_key
    }
}

redacted_debug!(BackupConfig, "BackupConfig");

#[derive(Clone, Eq, PartialEq)]
pub struct ComposeConfig {
    pub(crate) description: Field<String>,
    pub(crate) document: Field<SecretSource>,
    pub(crate) server: Field<ExternalSelector>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl ComposeConfig {
    /// Returns the create-only server placement selector.
    ///
    /// Dokploy accepts a server only when the service is created, so a changed
    /// placement replaces the service. `null` is rejected because local
    /// placement is the explicit `local` selector.
    #[must_use]
    pub const fn server(&self) -> &Field<ExternalSelector> {
        &self.server
    }

    #[must_use]
    pub const fn description(&self) -> &Field<String> {
        &self.description
    }

    #[must_use]
    pub const fn document(&self) -> &Field<SecretSource> {
        &self.document
    }
}

redacted_debug!(ComposeConfig, "ComposeConfig");

#[derive(Clone, Eq, PartialEq)]
pub struct PostgresConfig {
    pub(crate) database: Field<String>,
    pub(crate) username: Field<String>,
    pub(crate) password: Field<SecretSource>,
    pub(crate) server: Field<ExternalSelector>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl PostgresConfig {
    /// Returns the create-only server placement selector.
    ///
    /// Dokploy accepts a server only when the service is created, so a changed
    /// placement replaces the service. `null` is rejected because local
    /// placement is the explicit `local` selector.
    #[must_use]
    pub const fn server(&self) -> &Field<ExternalSelector> {
        &self.server
    }

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
pub struct MySqlConfig {
    pub(crate) database: Field<String>,
    pub(crate) username: Field<String>,
    pub(crate) password: Field<SecretSource>,
    pub(crate) root_password: Field<SecretSource>,
    pub(crate) server: Field<ExternalSelector>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl MySqlConfig {
    /// Returns the create-only server placement selector.
    ///
    /// Dokploy accepts a server only when the service is created, so a changed
    /// placement replaces the service. `null` is rejected because local
    /// placement is the explicit `local` selector.
    #[must_use]
    pub const fn server(&self) -> &Field<ExternalSelector> {
        &self.server
    }

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

    #[must_use]
    pub const fn root_password(&self) -> &Field<SecretSource> {
        &self.root_password
    }
}

redacted_debug!(MySqlConfig, "MySqlConfig");

#[derive(Clone, Eq, PartialEq)]
pub struct MariaDbConfig {
    pub(crate) database: Field<String>,
    pub(crate) username: Field<String>,
    pub(crate) password: Field<SecretSource>,
    pub(crate) root_password: Field<SecretSource>,
    pub(crate) server: Field<ExternalSelector>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl MariaDbConfig {
    /// Returns the create-only server placement selector.
    ///
    /// Dokploy accepts a server only when the service is created, so a changed
    /// placement replaces the service. `null` is rejected because local
    /// placement is the explicit `local` selector.
    #[must_use]
    pub const fn server(&self) -> &Field<ExternalSelector> {
        &self.server
    }

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

    #[must_use]
    pub const fn root_password(&self) -> &Field<SecretSource> {
        &self.root_password
    }
}

redacted_debug!(MariaDbConfig, "MariaDbConfig");

#[derive(Clone, Eq, PartialEq)]
pub struct MongoConfig {
    pub(crate) username: Field<String>,
    pub(crate) password: Field<SecretSource>,
    pub(crate) replica_sets: Field<bool>,
    pub(crate) server: Field<ExternalSelector>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl MongoConfig {
    /// Returns the create-only server placement selector.
    ///
    /// Dokploy accepts a server only when the service is created, so a changed
    /// placement replaces the service. `null` is rejected because local
    /// placement is the explicit `local` selector.
    #[must_use]
    pub const fn server(&self) -> &Field<ExternalSelector> {
        &self.server
    }

    #[must_use]
    pub const fn username(&self) -> &Field<String> {
        &self.username
    }

    #[must_use]
    pub const fn password(&self) -> &Field<SecretSource> {
        &self.password
    }

    #[must_use]
    pub const fn replica_sets(&self) -> &Field<bool> {
        &self.replica_sets
    }
}

redacted_debug!(MongoConfig, "MongoConfig");

/// One complete LibSQL primary-or-replica selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibSqlNodeConfig {
    /// A writable primary node.
    Primary,
    /// A replica connected to one primary URL.
    Replica { primary_url: String },
}

impl LibSqlNodeConfig {
    /// Returns the primary URL only for a replica node.
    #[must_use]
    pub fn primary_url(&self) -> Option<&str> {
        match self {
            Self::Primary => None,
            Self::Replica { primary_url } => Some(primary_url),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct LibSqlConfig {
    pub(crate) description: Field<String>,
    pub(crate) username: Field<String>,
    pub(crate) password: Field<SecretSource>,
    pub(crate) node: Field<LibSqlNodeConfig>,
    pub(crate) server: Field<ExternalSelector>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl LibSqlConfig {
    /// Returns the create-only server placement selector.
    ///
    /// Dokploy accepts a server only when the service is created, so a changed
    /// placement replaces the service. `null` is rejected because local
    /// placement is the explicit `local` selector.
    #[must_use]
    pub const fn server(&self) -> &Field<ExternalSelector> {
        &self.server
    }

    #[must_use]
    pub const fn description(&self) -> &Field<String> {
        &self.description
    }

    #[must_use]
    pub const fn username(&self) -> &Field<String> {
        &self.username
    }

    #[must_use]
    pub const fn password(&self) -> &Field<SecretSource> {
        &self.password
    }

    #[must_use]
    pub const fn node(&self) -> &Field<LibSqlNodeConfig> {
        &self.node
    }
}

redacted_debug!(LibSqlConfig, "LibSqlConfig");

#[derive(Clone, Eq, PartialEq)]
pub struct RedisConfig {
    pub(crate) password: Field<SecretSource>,
    pub(crate) server: Field<ExternalSelector>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl RedisConfig {
    /// Returns the create-only server placement selector.
    ///
    /// Dokploy accepts a server only when the service is created, so a changed
    /// placement replaces the service. `null` is rejected because local
    /// placement is the explicit `local` selector.
    #[must_use]
    pub const fn server(&self) -> &Field<ExternalSelector> {
        &self.server
    }

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
    InvalidLibSqlPrimaryUrl,
    ComposeDocumentCannotBeCleared,
    UnmanagedComposeDocumentRequiresProtection,
    DuplicatePortCollision,
    DuplicateRedirectCollision,
    SecurityPasswordCannotBeCleared,
    DuplicateSecurityCollision,
    InvalidMountTarget,
    InvalidMountField,
    DuplicateMountCollision,
    MountContentCannotBeCleared,
    UnmanagedMountContentRequiresProtection,
    InvalidScheduleTarget,
    InvalidScheduleField,
    DuplicateScheduleCollision,
    ScheduleSecretCannotBeCleared,
    UnmanagedScheduleCommandRequiresProtection,
    ScheduleComposeServiceMismatch,
    ScheduleFieldCannotBeCleared,
    InvalidExternalSelectorName,
    LocalSelectorUnsupported,
    ServerPlacementCannotBeCleared,
    InvalidBackupTarget,
    InvalidBackupField,
    DuplicateBackupCollision,
    EnvironmentsMustNestUnderProject,
}

impl ValidationIssue {
    /// Returns the stable public code for this semantic validation issue.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidResourceName { .. } => "DOKCFG001",
            Self::DuplicateResourceAddress { .. } => "DOKCFG002",
            Self::DuplicateDependency => "DOKCFG003",
            Self::SelfDependency => "DOKCFG004",
            Self::MissingDependency => "DOKCFG005",
            Self::MissingReference => "DOKCFG006",
            Self::CrossEnvironmentReference => "DOKCFG007",
            Self::UnsupportedReferenceProperty => "DOKCFG008",
            Self::InvalidEnvironmentVariable => "DOKCFG009",
            Self::InvalidSecretEnvironment => "DOKCFG010",
            Self::UnsafeSecretFile => "DOKCFG011",
            Self::DuplicateIgnoredChange => "DOKCFG012",
            Self::InvalidIgnoredChange => "DOKCFG013",
            Self::DuplicateMoveSource => "DOKCFG014",
            Self::DuplicateMoveTarget => "DOKCFG015",
            Self::InvalidMove => "DOKCFG016",
            Self::MoveSourceConfigured => "DOKCFG017",
            Self::MoveTargetMissing => "DOKCFG018",
            Self::DuplicateRemoval => "DOKCFG019",
            Self::RemovedResourceConfigured => "DOKCFG020",
            Self::MoveRemovalConflict => "DOKCFG021",
            Self::InvalidLibSqlPrimaryUrl => "DOKCFG022",
            Self::ComposeDocumentCannotBeCleared => "DOKCFG023",
            Self::UnmanagedComposeDocumentRequiresProtection => "DOKCFG024",
            Self::DuplicatePortCollision => "DOKCFG025",
            Self::DuplicateRedirectCollision => "DOKCFG026",
            Self::SecurityPasswordCannotBeCleared => "DOKCFG027",
            Self::DuplicateSecurityCollision => "DOKCFG028",
            Self::InvalidMountTarget => "DOKCFG040",
            Self::InvalidMountField => "DOKCFG041",
            Self::DuplicateMountCollision => "DOKCFG042",
            Self::MountContentCannotBeCleared => "DOKCFG043",
            Self::UnmanagedMountContentRequiresProtection => "DOKCFG044",
            Self::InvalidScheduleTarget => "DOKCFG050",
            Self::InvalidScheduleField => "DOKCFG051",
            Self::DuplicateScheduleCollision => "DOKCFG052",
            Self::ScheduleSecretCannotBeCleared => "DOKCFG053",
            Self::UnmanagedScheduleCommandRequiresProtection => "DOKCFG054",
            Self::ScheduleComposeServiceMismatch => "DOKCFG055",
            Self::ScheduleFieldCannotBeCleared => "DOKCFG056",
            Self::InvalidExternalSelectorName => "DOKCFG029",
            Self::LocalSelectorUnsupported => "DOKCFG030",
            Self::ServerPlacementCannotBeCleared => "DOKCFG031",
            Self::InvalidBackupTarget => "DOKCFG060",
            Self::InvalidBackupField => "DOKCFG061",
            Self::DuplicateBackupCollision => "DOKCFG062",
            Self::EnvironmentsMustNestUnderProject => "DOKCFG032",
        }
    }

    /// Returns a concise description that never contains source values.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::InvalidResourceName { .. } => "resource name is invalid",
            Self::DuplicateResourceAddress { .. } => "resource address is duplicated",
            Self::DuplicateDependency => "dependency is duplicated",
            Self::SelfDependency => "resource depends on itself",
            Self::MissingDependency => "dependency does not exist",
            Self::MissingReference => "referenced resource does not exist",
            Self::CrossEnvironmentReference => "reference crosses environment boundaries",
            Self::UnsupportedReferenceProperty => "referenced output is unsupported",
            Self::InvalidEnvironmentVariable => "environment variable name is invalid",
            Self::InvalidSecretEnvironment => "secret environment variable name is invalid",
            Self::UnsafeSecretFile => "secret file path is unsafe",
            Self::DuplicateIgnoredChange => "ignored lifecycle path is duplicated",
            Self::InvalidIgnoredChange => "ignored lifecycle path is unsupported",
            Self::DuplicateMoveSource => "move source is duplicated",
            Self::DuplicateMoveTarget => "move target is duplicated",
            Self::InvalidMove => "move declaration is invalid",
            Self::MoveSourceConfigured => "move source is still configured",
            Self::MoveTargetMissing => "move target is not configured",
            Self::DuplicateRemoval => "removal is duplicated",
            Self::RemovedResourceConfigured => "removed resource is still configured",
            Self::MoveRemovalConflict => "move and removal declarations conflict",
            Self::InvalidLibSqlPrimaryUrl => "LibSQL replica primary URL is empty",
            Self::ComposeDocumentCannotBeCleared => "Compose document cannot be null",
            Self::UnmanagedComposeDocumentRequiresProtection => {
                "unmanaged Compose document requires lifecycle.protect: true"
            }
            Self::DuplicatePortCollision => {
                "published port and protocol are duplicated within one application"
            }
            Self::DuplicateRedirectCollision => {
                "redirect regular expression is duplicated within one application"
            }
            Self::SecurityPasswordCannotBeCleared => "Security password cannot be null",
            Self::DuplicateSecurityCollision => {
                "security username is duplicated within one application"
            }
            Self::InvalidMountTarget => {
                "Mount target must be an application, Compose, or database in this environment"
            }
            Self::InvalidMountField => "Mount path, source, or name is invalid",
            Self::DuplicateMountCollision => "Mount path is duplicated within one target",
            Self::MountContentCannotBeCleared => "file Mount content cannot be null",
            Self::UnmanagedMountContentRequiresProtection => {
                "unmanaged file Mount content requires lifecycle.protect: true"
            }
            Self::InvalidScheduleTarget => {
                "Schedule target must be an application, or a Compose with a service_name, in this environment"
            }
            Self::InvalidScheduleField => {
                "Schedule name, cron expression, service name, timezone, or description is invalid"
            }
            Self::DuplicateScheduleCollision => "Schedule name is duplicated within one target",
            Self::ScheduleSecretCannotBeCleared => "Schedule command and script cannot be null",
            Self::UnmanagedScheduleCommandRequiresProtection => {
                "an unmanaged Schedule command requires lifecycle.protect: true"
            }
            Self::ScheduleComposeServiceMismatch => {
                "Schedules on one Compose must all use the same service_name"
            }
            Self::ScheduleFieldCannotBeCleared => {
                "Schedule description and timezone cannot be null"
            }
            Self::InvalidExternalSelectorName => {
                "external selector name must be a non-empty, trimmed value without control characters"
            }
            Self::LocalSelectorUnsupported => {
                "the local selector is supported only for server placement"
            }
            Self::ServerPlacementCannotBeCleared => {
                "server placement cannot be null; use `local: true` for the local server"
            }
            Self::InvalidBackupTarget => {
                "Backup target must be a PostgreSQL, MySQL, MariaDB, MongoDB, or LibSQL database in this environment"
            }
            Self::InvalidBackupField => {
                "Backup schedule, prefix, database, or retention count is invalid"
            }
            Self::DuplicateBackupCollision => {
                "Backup target, destination, prefix, and database are duplicated"
            }
            Self::EnvironmentsMustNestUnderProject => {
                "`environments` moved under `project`; nest the block as `project.environments`"
            }
        }
    }
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message())
    }
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
#[derive(Error)]
pub enum ConfigError {
    #[error("dokploy.yaml exceeds the {limit_bytes}-byte input limit")]
    InputTooLarge { limit_bytes: usize },
    #[error("dokploy.yaml is not valid at line {line}, column {column}")]
    Parse { line: u64, column: u64 },
    #[error("dokploy.yaml is not valid")]
    ParseWithoutLocation,
    #[error("dokploy.yaml version is unsupported; expected version 1")]
    UnsupportedVersion { found: u32 },
    #[error("dokploy.yaml failed semantic validation")]
    Invalid {
        issues: Vec<ValidationIssue>,
        diagnostics: Vec<ValidationDiagnostic>,
    },
}

impl fmt::Debug for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { limit_bytes } => formatter
                .debug_struct("InputTooLarge")
                .field("limit_bytes", limit_bytes)
                .finish(),
            Self::Parse { line, column } => formatter
                .debug_struct("Parse")
                .field("line", line)
                .field("column", column)
                .finish(),
            Self::ParseWithoutLocation => formatter.write_str("ParseWithoutLocation"),
            Self::UnsupportedVersion { .. } => {
                formatter.write_str("UnsupportedVersion { found: [REDACTED] }")
            }
            Self::Invalid {
                issues,
                diagnostics,
            } => formatter
                .debug_struct("Invalid")
                .field("issues", issues)
                .field("diagnostics", diagnostics)
                .finish(),
        }
    }
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
