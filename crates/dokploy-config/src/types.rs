use std::{borrow::Cow, fmt, num::NonZeroU16, path::Path, str::FromStr};

use dokploy_state::ResourceAddress;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, de};

use crate::Field;

/// A nonzero TCP or UDP port number accepted by Dokploy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PortNumber(NonZeroU16);

impl PortNumber {
    /// Creates a validated nonzero port number.
    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the validated numeric port.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl<'de> Deserialize<'de> for PortNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        NonZeroU16::new(value)
            .map(Self)
            .ok_or_else(|| de::Error::custom("port number must be between 1 and 65535"))
    }
}

impl JsonSchema for PortNumber {
    fn schema_name() -> Cow<'static, str> {
        "PortNumber".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "integer",
            "minimum": 1,
            "maximum": 65535
        })
    }
}

/// The network scope used to publish an application port.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PortPublishModeConfig {
    /// Publish through Docker Swarm's routing mesh.
    Ingress,
    /// Publish directly on the node that runs the task.
    Host,
}

/// The transport protocol carried by a published application port.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PortProtocolConfig {
    /// Transmission Control Protocol.
    Tcp,
    /// User Datagram Protocol.
    Udp,
}

/// A validated property path used by references and lifecycle rules.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PropertyPath(Vec<String>);

impl PropertyPath {
    /// Returns the validated path segments.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }
}

impl fmt::Debug for PropertyPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PropertyPath([REDACTED])")
    }
}

impl fmt::Display for PropertyPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.join("."))
    }
}

impl FromStr for PropertyPath {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let segments: Vec<_> = value.split('.').collect();
        if segments.is_empty() || segments.iter().any(|segment| !valid_path_segment(segment)) {
            return Err("property path must contain dot-separated identifier segments");
        }

        Ok(Self(segments.into_iter().map(str::to_owned).collect()))
    }
}

impl<'de> Deserialize<'de> for PropertyPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

impl JsonSchema for PropertyPath {
    fn schema_name() -> Cow<'static, str> {
        "PropertyPath".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[A-Za-z_][A-Za-z0-9_-]*(\\.[A-Za-z_][A-Za-z0-9_-]*)*$"
        })
    }
}

/// A resource output reference such as `postgres.main.connection_url`.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceReference {
    address: ResourceAddress,
    property: PropertyPath,
}

impl ResourceReference {
    /// Returns the referenced logical resource.
    #[must_use]
    pub const fn address(&self) -> &ResourceAddress {
        &self.address
    }

    /// Returns the referenced resource output path.
    #[must_use]
    pub const fn property(&self) -> &PropertyPath {
        &self.property
    }
}

impl fmt::Debug for ResourceReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResourceReference([REDACTED])")
    }
}

impl FromStr for ResourceReference {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut segments = value.splitn(3, '.');
        let kind = segments.next().ok_or("reference kind is missing")?;
        let name = segments.next().ok_or("reference name is missing")?;
        let property = segments.next().ok_or("reference property is missing")?;

        Ok(Self {
            address: format!("{kind}.{name}")
                .parse()
                .map_err(|_| "reference address is invalid")?,
            property: property.parse()?,
        })
    }
}

impl<'de> Deserialize<'de> for ResourceReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

impl JsonSchema for ResourceReference {
    fn schema_name() -> Cow<'static, str> {
        "ResourceReference".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^(project|environment|application|compose|postgres|mysql|mariadb|mongo|libsql|redis|domain|port)\\.[a-z][a-z0-9_-]*\\.[A-Za-z_][A-Za-z0-9_.-]*$"
        })
    }
}

/// The supported non-plaintext secret descriptor kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretSourceKind {
    Env,
    File,
}

/// A descriptor for loading a secret at execution time.
#[derive(Clone, Eq, PartialEq)]
pub enum SecretSource {
    Env(String),
    File(String),
}

impl SecretSource {
    /// Returns the descriptor kind without resolving or exposing its value.
    #[must_use]
    pub const fn kind(&self) -> SecretSourceKind {
        match self {
            Self::Env(_) => SecretSourceKind::Env,
            Self::File(_) => SecretSourceKind::File,
        }
    }

    /// Returns an environment variable descriptor for later resolution.
    #[must_use]
    pub fn env_name(&self) -> Option<&str> {
        match self {
            Self::Env(value) => Some(value),
            Self::File(_) => None,
        }
    }

    /// Returns a workspace-relative file descriptor for later resolution.
    #[must_use]
    pub fn file_path(&self) -> Option<&str> {
        match self {
            Self::File(value) => Some(value),
            Self::Env(_) => None,
        }
    }

    pub(crate) fn is_safe(&self) -> bool {
        match self {
            Self::Env(name) => valid_environment_name(name),
            Self::File(value) => safe_relative_secret_path(value),
        }
    }

    pub(crate) fn is_unsafe_file(&self) -> bool {
        matches!(self, Self::File(value) if !safe_relative_secret_path(value))
    }
}

impl fmt::Debug for SecretSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretSource([REDACTED])")
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawSecretSource {
    #[serde(default)]
    env: Field<String>,
    #[serde(default)]
    file: Field<String>,
}

impl<'de> Deserialize<'de> for SecretSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawSecretSource::deserialize(deserializer)?;
        match (raw.env, raw.file) {
            (Field::Set(value), Field::Unmanaged) => Ok(Self::Env(value)),
            (Field::Unmanaged, Field::Set(value)) => Ok(Self::File(value)),
            _ => Err(de::Error::custom(
                "secret descriptor must contain exactly one non-null `env` or `file` selector",
            )),
        }
    }
}

impl JsonSchema for SecretSource {
    fn schema_name() -> Cow<'static, str> {
        "SecretSource".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "env": {
                            "type": "string",
                            "pattern": "^[A-Z_][A-Z0-9_]*$",
                            "maxLength": 256
                        }
                    },
                    "required": ["env"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "file": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 4096
                        }
                    },
                    "required": ["file"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

/// A non-secret literal, resource reference, or deferred secret descriptor.
#[derive(Clone, Eq, PartialEq)]
pub enum ConfigValue {
    Literal(String),
    Reference(ResourceReference),
    Secret(SecretSource),
}

impl fmt::Debug for ConfigValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(_) => formatter.write_str("Literal([REDACTED])"),
            Self::Reference(_) => formatter.write_str("Reference([REDACTED])"),
            Self::Secret(_) => formatter.write_str("Secret([REDACTED])"),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawConfigValue {
    #[serde(default)]
    value: Field<String>,
    #[serde(default)]
    from: Field<ResourceReference>,
    #[serde(default)]
    secret: Field<SecretSource>,
}

impl<'de> Deserialize<'de> for ConfigValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawConfigValue::deserialize(deserializer)?;
        match (raw.value, raw.from, raw.secret) {
            (Field::Set(value), Field::Unmanaged, Field::Unmanaged) => Ok(Self::Literal(value)),
            (Field::Unmanaged, Field::Set(reference), Field::Unmanaged) => {
                Ok(Self::Reference(reference))
            }
            (Field::Unmanaged, Field::Unmanaged, Field::Set(secret)) => Ok(Self::Secret(secret)),
            _ => Err(de::Error::custom(
                "configured value must contain exactly one non-null `value`, `from`, or `secret` selector",
            )),
        }
    }
}

impl JsonSchema for ConfigValue {
    fn schema_name() -> Cow<'static, str> {
        "ConfigValue".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "required": ["value"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": { "from": ResourceReference::json_schema(generator) },
                    "required": ["from"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": { "secret": SecretSource::json_schema(generator) },
                    "required": ["secret"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

/// Generic lifecycle behavior owned by the reconciliation engine.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    #[serde(default)]
    protect: Field<bool>,
    #[serde(default)]
    ignore_changes: Vec<PropertyPath>,
}

impl Lifecycle {
    /// Returns the ownership-aware protection setting.
    #[must_use]
    pub const fn protect(&self) -> &Field<bool> {
        &self.protect
    }

    /// Returns ignored paths in deterministic lexical order after validation.
    #[must_use]
    pub fn ignore_changes(&self) -> &[PropertyPath] {
        &self.ignore_changes
    }

    pub(crate) fn normalize(&mut self) {
        self.ignore_changes.sort();
    }
}

impl fmt::Debug for Lifecycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Lifecycle")
            .field("protect", &self.protect)
            .field("ignored_path_count", &self.ignore_changes.len())
            .finish()
    }
}

/// A GitHub application source.
#[derive(Clone, Deserialize, Eq, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitHubSource {
    repository: String,
    #[serde(default)]
    branch: Field<String>,
}

impl GitHubSource {
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    #[must_use]
    pub const fn branch(&self) -> &Field<String> {
        &self.branch
    }
}

impl fmt::Debug for GitHubSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GitHubSource([REDACTED])")
    }
}

/// An application source configuration.
#[derive(Clone, Deserialize, Eq, PartialEq, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    #[serde(rename = "github")]
    #[schemars(rename = "github")]
    GitHub(GitHubSource),
}

impl fmt::Debug for SourceConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceConfig([REDACTED])")
    }
}

/// One declarative logical-address move.
#[derive(Clone, Deserialize, Eq, Ord, PartialEq, PartialOrd, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveDeclaration {
    #[schemars(with = "String")]
    from: ResourceAddress,
    #[schemars(with = "String")]
    to: ResourceAddress,
}

impl MoveDeclaration {
    #[must_use]
    pub const fn from(&self) -> &ResourceAddress {
        &self.from
    }

    #[must_use]
    pub const fn to(&self) -> &ResourceAddress {
        &self.to
    }
}

impl fmt::Debug for MoveDeclaration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MoveDeclaration([REDACTED])")
    }
}

/// One declaration that relinquishes or destroys a resource removed from config.
#[derive(Clone, Deserialize, Eq, Ord, PartialEq, PartialOrd, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemovedDeclaration {
    #[schemars(with = "String")]
    from: ResourceAddress,
    #[serde(default)]
    destroy: bool,
}

impl RemovedDeclaration {
    #[must_use]
    pub const fn from(&self) -> &ResourceAddress {
        &self.from
    }

    #[must_use]
    pub const fn destroy(&self) -> bool {
        self.destroy
    }
}

impl fmt::Debug for RemovedDeclaration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemovedDeclaration")
            .field("destroy", &self.destroy)
            .finish_non_exhaustive()
    }
}

/// Managed domain inputs.
#[derive(Clone, Eq, PartialEq)]
pub struct DomainConfig {
    pub(crate) host: Field<String>,
    pub(crate) application: Field<ResourceAddress>,
    pub(crate) depends_on: Vec<ResourceAddress>,
    pub(crate) lifecycle: Lifecycle,
}

impl DomainConfig {
    #[must_use]
    pub const fn host(&self) -> &Field<String> {
        &self.host
    }

    #[must_use]
    pub const fn application(&self) -> &Field<ResourceAddress> {
        &self.application
    }
}

impl fmt::Debug for DomainConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DomainConfig([REDACTED])")
    }
}

fn valid_path_segment(segment: &str) -> bool {
    let mut characters = segment.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

pub(crate) fn valid_environment_name(value: &str) -> bool {
    if value.len() > 256 {
        return false;
    }

    let mut characters = value.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_uppercase() || first == '_')
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

fn safe_relative_secret_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 4096
        || value.contains(['\\', ':'])
        || value.starts_with(['~', '/'])
        || Path::new(value).is_absolute()
    {
        return false;
    }

    let mut segments = value.split('/');
    let first = segments.next().expect("non-empty paths have a segment");
    if first == "." {
        let Some(next) = segments.next() else {
            return false;
        };
        valid_secret_path_segment(next) && segments.all(valid_secret_path_segment)
    } else {
        valid_secret_path_segment(first) && segments.all(valid_secret_path_segment)
    }
}

fn valid_secret_path_segment(segment: &str) -> bool {
    !segment.is_empty() && !matches!(segment, "." | "..")
}
