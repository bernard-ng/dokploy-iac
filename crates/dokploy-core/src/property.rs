use std::{fmt, str::FromStr};

use dokploy_state::{ResourceKind, SensitiveFingerprint};
use serde::{Serialize, Serializer};
use thiserror::Error;

/// A validated application environment-variable name.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EnvironmentVariableName(String);

impl EnvironmentVariableName {
    /// Validates the same uppercase environment name grammar as configuration.
    pub fn new(value: impl Into<String>) -> Result<Self, EnvironmentVariableNameError> {
        let value = value.into();
        if value.len() > 256 {
            return Err(EnvironmentVariableNameError);
        }

        let mut characters = value.chars();
        if !matches!(characters.next(), Some(first) if first.is_ascii_uppercase() || first == '_')
            || !characters.all(|character| {
                character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
            })
        {
            return Err(EnvironmentVariableNameError);
        }

        Ok(Self(value))
    }

    /// Returns the validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EnvironmentVariableName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An invalid environment-variable name.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("environment variable name is invalid")]
pub struct EnvironmentVariableNameError;

/// A typed, path-capable property vocabulary shared by planner snapshots.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PropertyPath {
    /// Human-readable resource description.
    Description,
    /// Application replica count.
    Replicas,
    /// Application source root, used only for an explicit clear.
    Source,
    /// Application source repository.
    SourceRepository,
    /// Application source branch.
    SourceBranch,
    /// Application environment root, used for clear or owned-empty intent.
    Environment,
    /// One independently owned application environment entry.
    EnvironmentVariable(EnvironmentVariableName),
    /// Database name.
    Database,
    /// Database user name.
    Username,
    /// Write-only database password.
    Password,
    /// Write-only database root password.
    RootPassword,
    /// Whether MongoDB replica sets are enabled.
    ReplicaSets,
    /// Atomic LibSQL primary-or-replica node selection.
    Node,
    /// Opaque Compose document.
    ComposeDocument,
    /// Domain host name.
    Host,
    /// Domain application reference.
    Application,
    /// Externally published application port.
    PublishedPort,
    /// Container target port.
    TargetPort,
    /// Docker publication mode.
    PublishMode,
    /// Transport protocol.
    Protocol,
    /// Redirect regular expression, the application-scoped collision key.
    Regex,
    /// Redirect replacement expression.
    Replacement,
    /// Whether a Redirect is permanent.
    Permanent,
    /// Mount target service reference.
    Target,
    /// Mount storage mechanism.
    MountType,
    /// Mount path inside the target service.
    MountPath,
    /// Bind Mount host path.
    HostPath,
    /// Volume Mount name.
    VolumeName,
    /// File Mount relative file path.
    FilePath,
    /// Opaque, write-only file Mount content.
    FileContent,
    /// Deployment status, valid only in lifecycle metadata.
    DeploymentStatus,
}

impl PropertyPath {
    /// Creates a validated environment entry path.
    pub fn environment_variable(
        name: impl Into<String>,
    ) -> Result<Self, EnvironmentVariableNameError> {
        Ok(Self::EnvironmentVariable(EnvironmentVariableName::new(
            name,
        )?))
    }

    /// Returns whether values at this path are sensitive or write-only.
    #[must_use]
    pub const fn is_sensitive(&self) -> bool {
        matches!(
            self,
            Self::Password
                | Self::RootPassword
                | Self::ComposeDocument
                | Self::FileContent
                | Self::EnvironmentVariable(_)
        )
    }

    /// Returns whether this path is lifecycle-only.
    #[must_use]
    pub const fn is_lifecycle_only(&self) -> bool {
        matches!(self, Self::DeploymentStatus)
    }

    pub(crate) const fn is_source_child(&self) -> bool {
        matches!(self, Self::SourceRepository | Self::SourceBranch)
    }

    pub(crate) const fn is_environment_child(&self) -> bool {
        matches!(self, Self::EnvironmentVariable(_))
    }

    pub(crate) const fn valid_for_kind(&self, kind: ResourceKind) -> bool {
        match kind {
            ResourceKind::Project | ResourceKind::Environment => {
                matches!(self, Self::Description)
            }
            ResourceKind::Application => matches!(
                self,
                Self::Description
                    | Self::Replicas
                    | Self::Source
                    | Self::SourceRepository
                    | Self::SourceBranch
                    | Self::Environment
                    | Self::EnvironmentVariable(_)
                    | Self::DeploymentStatus
            ),
            ResourceKind::Compose => matches!(self, Self::Description | Self::ComposeDocument),
            ResourceKind::Postgres => {
                matches!(self, Self::Database | Self::Username | Self::Password)
            }
            ResourceKind::MySql => matches!(
                self,
                Self::Database | Self::Username | Self::Password | Self::RootPassword
            ),
            ResourceKind::MariaDb => matches!(
                self,
                Self::Database | Self::Username | Self::Password | Self::RootPassword
            ),
            ResourceKind::Mongo => {
                matches!(self, Self::Username | Self::Password | Self::ReplicaSets)
            }
            ResourceKind::LibSql => matches!(
                self,
                Self::Description | Self::Username | Self::Password | Self::Node
            ),
            ResourceKind::Redis => matches!(self, Self::Password),
            ResourceKind::Domain => matches!(self, Self::Host | Self::Application),
            ResourceKind::Port => matches!(
                self,
                Self::PublishedPort | Self::TargetPort | Self::PublishMode | Self::Protocol
            ),
            ResourceKind::Redirect => {
                matches!(self, Self::Regex | Self::Replacement | Self::Permanent)
            }
            ResourceKind::Security => matches!(self, Self::Username | Self::Password),
            ResourceKind::Mount => matches!(
                self,
                Self::Target
                    | Self::MountType
                    | Self::MountPath
                    | Self::HostPath
                    | Self::VolumeName
                    | Self::FilePath
                    | Self::FileContent
            ),
        }
    }
}

impl fmt::Display for PropertyPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Description => formatter.write_str("description"),
            Self::Replicas => formatter.write_str("replicas"),
            Self::Source => formatter.write_str("source"),
            Self::SourceRepository => formatter.write_str("source.repository"),
            Self::SourceBranch => formatter.write_str("source.branch"),
            Self::Environment => formatter.write_str("environment"),
            Self::EnvironmentVariable(name) => {
                write!(formatter, "environment.{}", name.as_str())
            }
            Self::Database => formatter.write_str("database"),
            Self::Username => formatter.write_str("username"),
            Self::Password => formatter.write_str("password"),
            Self::RootPassword => formatter.write_str("root_password"),
            Self::ReplicaSets => formatter.write_str("replica_sets"),
            Self::Node => formatter.write_str("node"),
            Self::ComposeDocument => formatter.write_str("document"),
            Self::Host => formatter.write_str("host"),
            Self::Application => formatter.write_str("application"),
            Self::PublishedPort => formatter.write_str("published_port"),
            Self::TargetPort => formatter.write_str("target_port"),
            Self::PublishMode => formatter.write_str("publish_mode"),
            Self::Protocol => formatter.write_str("protocol"),
            Self::Regex => formatter.write_str("regex"),
            Self::Replacement => formatter.write_str("replacement"),
            Self::Permanent => formatter.write_str("permanent"),
            Self::Target => formatter.write_str("target"),
            Self::MountType => formatter.write_str("mount_type"),
            Self::MountPath => formatter.write_str("mount_path"),
            Self::HostPath => formatter.write_str("host_path"),
            Self::VolumeName => formatter.write_str("volume_name"),
            Self::FilePath => formatter.write_str("file_path"),
            Self::FileContent => formatter.write_str("content"),
            Self::DeploymentStatus => formatter.write_str("deployment.status"),
        }
    }
}

impl FromStr for PropertyPath {
    type Err = PropertyPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "description" => Ok(Self::Description),
            "replicas" => Ok(Self::Replicas),
            "source" => Ok(Self::Source),
            "source.repository" => Ok(Self::SourceRepository),
            "source.branch" => Ok(Self::SourceBranch),
            "environment" => Ok(Self::Environment),
            "database" => Ok(Self::Database),
            "username" => Ok(Self::Username),
            "password" => Ok(Self::Password),
            "root_password" => Ok(Self::RootPassword),
            "replica_sets" => Ok(Self::ReplicaSets),
            "node" => Ok(Self::Node),
            "document" => Ok(Self::ComposeDocument),
            "host" => Ok(Self::Host),
            "application" => Ok(Self::Application),
            "published_port" => Ok(Self::PublishedPort),
            "target_port" => Ok(Self::TargetPort),
            "publish_mode" => Ok(Self::PublishMode),
            "protocol" => Ok(Self::Protocol),
            "regex" => Ok(Self::Regex),
            "replacement" => Ok(Self::Replacement),
            "permanent" => Ok(Self::Permanent),
            "target" => Ok(Self::Target),
            "mount_type" => Ok(Self::MountType),
            "mount_path" => Ok(Self::MountPath),
            "host_path" => Ok(Self::HostPath),
            "volume_name" => Ok(Self::VolumeName),
            "file_path" => Ok(Self::FilePath),
            "content" => Ok(Self::FileContent),
            "deployment.status" => Ok(Self::DeploymentStatus),
            _ => {
                let Some(name) = value.strip_prefix("environment.") else {
                    return Err(PropertyPathError);
                };
                Self::environment_variable(name).map_err(|_| PropertyPathError)
            }
        }
    }
}

impl Serialize for PropertyPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

/// A path outside the first planner checkpoint's typed vocabulary.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("managed state contains a property path unsupported by this planner version")]
pub struct PropertyPathError;

/// A non-null property value comparable only inside the pure planner.
#[derive(Clone, Eq, PartialEq)]
pub struct ComparableValue(pub(crate) serde_json::Value);

impl ComparableValue {
    /// Wraps a non-null JSON value without exposing it through debug or plans.
    pub fn try_from_json(value: serde_json::Value) -> Result<Self, ComparableValueError> {
        if value.is_null() {
            return Err(ComparableValueError);
        }

        Ok(Self(value))
    }

    pub(crate) const fn as_json(&self) -> &serde_json::Value {
        &self.0
    }
}

impl fmt::Debug for ComparableValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ComparableValue([REDACTED])")
    }
}

/// A null value that must use [`OwnedValue::Null`] instead.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("comparable values cannot be null; use explicit property null ownership")]
pub struct ComparableValueError;

/// An opaque, comparable receipt for one sensitive desired input.
///
/// The receipt can be compared inside the planner, but its key identifier and
/// MAC are never exposed through this interface, debug output, or plan data.
#[derive(Clone)]
pub struct SensitiveIntent(SensitiveFingerprint);

impl SensitiveIntent {
    /// Wraps a durable fingerprint for pure intent comparison.
    #[must_use]
    pub const fn from_fingerprint(fingerprint: SensitiveFingerprint) -> Self {
        Self(fingerprint)
    }

    /// Compares two receipts without exposing their representation.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    pub(crate) const fn fingerprint(&self) -> &SensitiveFingerprint {
        &self.0
    }
}

impl PartialEq for SensitiveIntent {
    fn eq(&self, other: &Self) -> bool {
        self.matches(other)
    }
}

impl Eq for SensitiveIntent {}

impl fmt::Debug for SensitiveIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveIntent([REDACTED])")
    }
}

/// A property value owned by desired or durable stored state.
#[derive(Clone, Eq, PartialEq)]
pub enum OwnedValue {
    /// The property is owned and should be cleared remotely.
    Null,
    /// The environment collection is owned and intentionally empty.
    EmptyCollection,
    /// The property is owned with an opaque comparable value.
    Value(ComparableValue),
    /// A sensitive value is owned through an opaque intent receipt.
    Sensitive(SensitiveIntent),
}

impl fmt::Debug for OwnedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("Null"),
            Self::EmptyCollection => formatter.write_str("EmptyCollection"),
            Self::Value(_) => formatter.write_str("Value([REDACTED])"),
            Self::Sensitive(_) => formatter.write_str("Sensitive([REDACTED])"),
        }
    }
}
