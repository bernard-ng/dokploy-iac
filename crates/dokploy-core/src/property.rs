use std::{fmt, str::FromStr};

use dokploy_state::ResourceKind;
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
    /// Domain host name.
    Host,
    /// Domain application reference.
    Application,
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
        matches!(self, Self::Password | Self::EnvironmentVariable(_))
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
            ResourceKind::Postgres => {
                matches!(self, Self::Database | Self::Username | Self::Password)
            }
            ResourceKind::Redis => matches!(self, Self::Password),
            ResourceKind::Domain => matches!(self, Self::Host | Self::Application),
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
            Self::Host => formatter.write_str("host"),
            Self::Application => formatter.write_str("application"),
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
            "host" => Ok(Self::Host),
            "application" => Ok(Self::Application),
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

/// A property value owned by desired or durable stored state.
#[derive(Clone, Eq, PartialEq)]
pub enum OwnedValue {
    /// The property is owned and should be cleared remotely.
    Null,
    /// The environment collection is owned and intentionally empty.
    EmptyCollection,
    /// The property is owned with an opaque comparable value.
    Value(ComparableValue),
    /// A sensitive value is owned, but its bytes never enter planner snapshots.
    Sensitive,
}

impl fmt::Debug for OwnedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("Null"),
            Self::EmptyCollection => formatter.write_str("EmptyCollection"),
            Self::Value(_) => formatter.write_str("Value([REDACTED])"),
            Self::Sensitive => formatter.write_str("Sensitive([REDACTED])"),
        }
    }
}
