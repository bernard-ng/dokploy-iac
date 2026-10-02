//! Strict, bounded parsing and semantic validation for `dokploy.yaml`.
//!
//! The configuration module deliberately exposes one deep parsing seam:
//! [`DokployConfig::parse`] converts untrusted YAML into a deterministic,
//! validated model or returns a redacted error. Resource identity remains the
//! global `kind.name` form owned by `dokploy-state`; consequently, resource
//! names must be unique across all environments in the initial MVP.
//!
//! Secret sources are descriptors only. Parsing never reads the process
//! environment or filesystem. File descriptors are lexically restricted to a
//! relative path (optionally beginning with `./`); execution remains
//! responsible for config-directory resolution, regular-file checks, symlink
//! policy, and secret-byte handling. Source locations are retained separately
//! for safe diagnostics and do not affect normalized configuration equality.
//! Canonical writing rebuilds the nested YAML shape from that normalized model,
//! so source comments are intentionally not retained.

mod field;
mod file;
mod model;
mod parser;
mod types;
mod writer;

pub use field::Field;
pub use file::{
    ConfigFileError, DEFAULT_CONFIG_FILE, DEFAULT_SETTINGS_FILE, LoadedConfig, MAX_CONFIG_BYTES,
    initialize, initialize_settings, load, load_with_digest, peek_scope,
};
pub use model::{
    ApplicationConfig, BackupConfig, ComposeConfig, ConfigError, DokployConfig, EnvironmentConfig,
    LibSqlConfig, LibSqlNodeConfig, MariaDbConfig, MongoConfig, MountConfig, MySqlConfig,
    PortConfig, PostgresConfig, ProjectConfig, RedirectConfig, RedisConfig, ResourceConfig,
    ScheduleConfig, SecurityConfig, SourceLocation, TagConfig, ValidationDiagnostic,
    ValidationIssue,
};
pub use types::{
    ConfigValue, DomainConfig, ExternalName, ExternalSelector, GitHubSource, Lifecycle,
    MountSourceConfig, MoveDeclaration, NonEmptyText, PortNumber, PortProtocolConfig,
    PortPublishModeConfig, PropertyPath, RemovedDeclaration, ResourceReference,
    ScheduleShellConfig, SecretSource, SecretSourceKind, SelectorKind, SourceConfig,
};
pub use writer::{
    ApplicationDocument, BackupDocument, ComposeDocument, ConfigDocument, ConfigDocumentError,
    ConfigWriteError, DomainDocument, EnvironmentDocument, LibSqlDocument, LifecycleDocument,
    MariaDbDocument, MongoDocument, MountDocument, MySqlDocument, PortDocument, PostgresDocument,
    ProjectDocument, RedirectDocument, RedisDocument, ScheduleDocument, SecurityDocument,
    SettingsDocument, SourceDocument, TagDocument, render, write,
};
