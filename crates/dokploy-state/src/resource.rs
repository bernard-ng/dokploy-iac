use std::{cmp::Ordering, fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

/// The document and durable state a resource kind belongs to.
///
/// A workspace can hold a project document and a settings document side by side.
/// Each owns an independent state lineage, so a crash or recovery in one scope
/// never blocks the other.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum StateScope {
    /// Projects, environments, services, and everything below them.
    #[default]
    Project,
    /// Instance-level settings such as tags.
    Settings,
}

impl StateScope {
    /// Returns the canonical scope name used in state files and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Settings => "settings",
        }
    }
}

impl fmt::Display for StateScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A kind supported by the initial declarative resource model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Project,
    Environment,
    Application,
    Compose,
    Postgres,
    #[serde(rename = "mysql")]
    MySql,
    #[serde(rename = "mariadb")]
    MariaDb,
    Mongo,
    #[serde(rename = "libsql")]
    LibSql,
    Redis,
    Domain,
    Port,
    Redirect,
    Security,
    Mount,
    Schedule,
    Backup,
}

impl ResourceKind {
    /// Returns the canonical kind segment used in logical addresses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Environment => "environment",
            Self::Application => "application",
            Self::Compose => "compose",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::MariaDb => "mariadb",
            Self::Mongo => "mongo",
            Self::LibSql => "libsql",
            Self::Redis => "redis",
            Self::Domain => "domain",
            Self::Port => "port",
            Self::Redirect => "redirect",
            Self::Security => "security",
            Self::Mount => "mount",
            Self::Schedule => "schedule",
            Self::Backup => "backup",
        }
    }

    /// Returns the document scope this kind is declared and tracked in.
    #[must_use]
    pub const fn scope(self) -> StateScope {
        match self {
            Self::Project
            | Self::Environment
            | Self::Application
            | Self::Compose
            | Self::Postgres
            | Self::MySql
            | Self::MariaDb
            | Self::Mongo
            | Self::LibSql
            | Self::Redis
            | Self::Domain
            | Self::Port
            | Self::Redirect
            | Self::Security
            | Self::Mount
            | Self::Schedule
            | Self::Backup => StateScope::Project,
        }
    }

    /// Returns the required containment parent kind, if the resource is nested.
    #[must_use]
    pub const fn containment_parent_kind(self) -> Option<Self> {
        match self {
            Self::Project => None,
            Self::Environment => Some(Self::Project),
            Self::Application
            | Self::Compose
            | Self::Postgres
            | Self::MySql
            | Self::MariaDb
            | Self::Mongo
            | Self::LibSql
            | Self::Redis
            | Self::Domain
            | Self::Mount
            | Self::Schedule
            | Self::Backup => Some(Self::Environment),
            Self::Port | Self::Redirect | Self::Security => Some(Self::Application),
        }
    }

    /// Returns whether a Mount may target resources of this kind.
    ///
    /// This is the closed target union shared by configuration, planner
    /// snapshots, and the Dokploy adapter.
    #[must_use]
    pub const fn is_mount_target(self) -> bool {
        matches!(
            self,
            Self::Application
                | Self::Compose
                | Self::Postgres
                | Self::MySql
                | Self::MariaDb
                | Self::Mongo
                | Self::LibSql
                | Self::Redis
        )
    }

    /// Returns whether a Schedule may target resources of this kind.
    ///
    /// Dokploy host and Dokploy-server Schedule scopes are intentionally not
    /// part of this closed union.
    #[must_use]
    pub const fn is_schedule_target(self) -> bool {
        matches!(self, Self::Application | Self::Compose)
    }

    /// Returns whether a Backup may target resources of this kind.
    ///
    /// Backups are supported for the five database kinds proven by the SDK
    /// contract. Compose and web-server Backups remain unsupported.
    #[must_use]
    pub const fn is_backup_target(self) -> bool {
        matches!(
            self,
            Self::Postgres | Self::MySql | Self::MariaDb | Self::Mongo | Self::LibSql
        )
    }
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Ord for ResourceKind {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for ResourceKind {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl FromStr for ResourceKind {
    type Err = ResourceKindParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "project" => Ok(Self::Project),
            "environment" => Ok(Self::Environment),
            "application" => Ok(Self::Application),
            "compose" => Ok(Self::Compose),
            "postgres" => Ok(Self::Postgres),
            "mysql" => Ok(Self::MySql),
            "mariadb" => Ok(Self::MariaDb),
            "mongo" => Ok(Self::Mongo),
            "libsql" => Ok(Self::LibSql),
            "redis" => Ok(Self::Redis),
            "domain" => Ok(Self::Domain),
            "port" => Ok(Self::Port),
            "redirect" => Ok(Self::Redirect),
            "security" => Ok(Self::Security),
            "mount" => Ok(Self::Mount),
            "schedule" => Ok(Self::Schedule),
            "backup" => Ok(Self::Backup),
            _ => Err(ResourceKindParseError {
                value: value.to_owned(),
            }),
        }
    }
}

/// An unknown logical resource kind.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("unknown resource kind `{value}`")]
pub struct ResourceKindParseError {
    value: String,
}

/// A validated logical name within one resource kind.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceName(String);

impl ResourceName {
    /// Validates a logical name.
    pub fn new(value: impl Into<String>) -> Result<Self, ResourceNameError> {
        let value = value.into();
        let mut characters = value.char_indices();

        let Some((_, first)) = characters.next() else {
            return Err(ResourceNameError::Empty);
        };

        if !first.is_ascii_lowercase() {
            return Err(ResourceNameError::InvalidStart { character: first });
        }

        if let Some((index, character)) =
            characters.find(|(_, character)| !is_name_character(*character))
        {
            return Err(ResourceNameError::InvalidCharacter { index, character });
        }

        Ok(Self(value))
    }

    /// Returns the validated logical name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResourceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ResourceName {
    type Err = ResourceNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for ResourceName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ResourceName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// A logical resource name that does not satisfy the canonical grammar.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ResourceNameError {
    #[error("resource name cannot be empty")]
    Empty,
    #[error("resource name must start with a lowercase ASCII letter, found `{character}`")]
    InvalidStart { character: char },
    #[error(
        "resource name contains invalid character `{character}` at byte {index}; use lowercase ASCII letters, digits, `-`, or `_`"
    )]
    InvalidCharacter { index: usize, character: char },
}

/// A stable logical identity independent of Dokploy's remote identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceAddress {
    kind: ResourceKind,
    name: ResourceName,
}

impl ResourceAddress {
    /// Creates an address from already typed parts.
    #[must_use]
    pub fn new(kind: ResourceKind, name: ResourceName) -> Self {
        Self { kind, name }
    }

    /// Returns the addressed resource kind.
    #[must_use]
    pub const fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// Returns the addressed logical name.
    #[must_use]
    pub const fn name(&self) -> &ResourceName {
        &self.name
    }
}

impl fmt::Display for ResourceAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.kind, self.name)
    }
}

impl FromStr for ResourceAddress {
    type Err = ResourceAddressParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (kind, name) = value
            .split_once('.')
            .ok_or(ResourceAddressParseError::MissingSeparator)?;

        Ok(Self {
            kind: kind.parse()?,
            name: name.parse()?,
        })
    }
}

impl Serialize for ResourceAddress {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ResourceAddress {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

/// A malformed logical resource address.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ResourceAddressParseError {
    #[error("resource address must contain a kind and name separated by `.`")]
    MissingSeparator,
    #[error(transparent)]
    Kind(#[from] ResourceKindParseError),
    #[error(transparent)]
    Name(#[from] ResourceNameError),
}

fn is_name_character(character: char) -> bool {
    character.is_ascii_lowercase() || character.is_ascii_digit() || matches!(character, '-' | '_')
}
