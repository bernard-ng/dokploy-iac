use std::{collections::BTreeMap, fmt};

use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{ResourceAddress, ResourceKind};

const CURRENT_FORMAT_VERSION: u32 = 1;

const SENSITIVE_KEY_SUFFIXES: &[&str] = &[
    "password",
    "apikey",
    "accesskey",
    "privatekey",
    "secret",
    "buildsecrets",
    "token",
    "refreshtoken",
    "env",
    "previewenv",
    "buildargs",
    "previewbuildargs",
];

/// The normalized base URL of the Dokploy instance owning a state lineage.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InstanceIdentity(String);

impl InstanceIdentity {
    /// Parses and normalizes an HTTP(S) Dokploy base URL.
    pub fn parse(value: &str) -> Result<Self, InstanceIdentityError> {
        let mut url = Url::parse(value).map_err(InstanceIdentityError::InvalidUrl)?;

        if !matches!(url.scheme(), "http" | "https") {
            return Err(InstanceIdentityError::UnsupportedScheme {
                scheme: url.scheme().to_owned(),
            });
        }

        if url.host_str().is_none() {
            return Err(InstanceIdentityError::MissingHost);
        }

        if !url.username().is_empty() || url.password().is_some() {
            return Err(InstanceIdentityError::CredentialsNotAllowed);
        }

        if url.query().is_some() {
            return Err(InstanceIdentityError::QueryNotAllowed);
        }

        if url.fragment().is_some() {
            return Err(InstanceIdentityError::FragmentNotAllowed);
        }

        let path = url.path().trim_end_matches('/');
        let path = path.strip_suffix("/api").unwrap_or(path);
        let normalized_path = if path.is_empty() { "/" } else { path };
        if url.path() != normalized_path {
            let normalized_path = normalized_path.to_owned();
            url.set_path(&normalized_path);
        }

        Ok(Self(url.to_string()))
    }

    /// Returns the canonical base URL used for instance comparison.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InstanceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for InstanceIdentity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InstanceIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// A URL that cannot safely identify one Dokploy instance.
#[derive(Debug, Error)]
pub enum InstanceIdentityError {
    #[error("invalid Dokploy instance URL: {0}")]
    InvalidUrl(#[source] url::ParseError),
    #[error("Dokploy instance URL must use HTTP or HTTPS, found `{scheme}`")]
    UnsupportedScheme { scheme: String },
    #[error("Dokploy instance URL must include a host")]
    MissingHost,
    #[error("Dokploy instance URL cannot contain credentials")]
    CredentialsNotAllowed,
    #[error("Dokploy instance URL cannot contain a query")]
    QueryNotAllowed,
    #[error("Dokploy instance URL cannot contain a fragment")]
    FragmentNotAllowed,
}

/// An opaque, non-empty identifier assigned by Dokploy.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RemoteId(String);

impl RemoteId {
    /// Creates an opaque identifier after rejecting empty values.
    pub fn new(value: impl Into<String>) -> Result<Self, RemoteIdError> {
        let value = value.into();

        if value.trim().is_empty() {
            return Err(RemoteIdError);
        }

        Ok(Self(value))
    }

    /// Returns the exact identifier supplied by Dokploy.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemoteId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for RemoteId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RemoteId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// An empty Dokploy remote identifier.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("remote identifier cannot be empty")]
pub struct RemoteIdError;

/// The non-sensitive inputs that Dokploy was last asked to manage.
///
/// This type is the state seam for managed JSON. It rejects scalar roots and
/// known secret-bearing field families recursively. Resource-specific adapters
/// remain responsible for passing only fields owned by their configuration
/// schema; secret values under an unrelated key cannot be inferred from JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ManagedInputs(serde_json::Value);

impl ManagedInputs {
    /// Validates JSON before it can cross into durable managed state.
    pub fn try_from_json(value: serde_json::Value) -> Result<Self, ManagedInputsError> {
        if !value.is_object() {
            return Err(ManagedInputsError::NotObject);
        }

        validate_non_sensitive_json(&value, "$".to_owned())?;

        Ok(Self(value))
    }

    /// Returns the validated managed input object.
    #[must_use]
    pub const fn as_json(&self) -> &serde_json::Value {
        &self.0
    }

    /// Consumes the validated wrapper.
    #[must_use]
    pub fn into_json(self) -> serde_json::Value {
        self.0
    }
}

impl<'de> Deserialize<'de> for ManagedInputs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::try_from_json(value).map_err(de::Error::custom)
    }
}

/// JSON that cannot enter the durable non-sensitive managed-input state.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ManagedInputsError {
    #[error("managed inputs must be a JSON object")]
    NotObject,
    #[error("managed inputs contain secret-bearing field `{path}`")]
    SensitiveField { path: String },
}

fn validate_non_sensitive_json(
    value: &serde_json::Value,
    path: String,
) -> Result<(), ManagedInputsError> {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let child_path = format!("{path}.{key}");
                if is_sensitive_key(key) {
                    return Err(ManagedInputsError::SensitiveField { path: child_path });
                }

                validate_non_sensitive_json(value, child_path)?;
            }
        }
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_non_sensitive_json(value, format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect();

    SENSITIVE_KEY_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}

/// Durable state for one managed resource.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceState {
    kind: ResourceKind,
    remote_id: RemoteId,
    protected: bool,
    last_applied: ManagedInputs,
    dependencies: Vec<ResourceAddress>,
}

impl ResourceState {
    /// Creates resource state and canonicalizes dependency ordering.
    #[must_use]
    pub fn new(
        kind: ResourceKind,
        remote_id: RemoteId,
        protected: bool,
        last_applied: ManagedInputs,
        mut dependencies: Vec<ResourceAddress>,
    ) -> Self {
        dependencies.sort();
        dependencies.dedup();

        Self {
            kind,
            remote_id,
            protected,
            last_applied,
            dependencies,
        }
    }

    /// Returns the remote resource kind.
    #[must_use]
    pub const fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// Returns the opaque identifier assigned by Dokploy.
    #[must_use]
    pub const fn remote_id(&self) -> &RemoteId {
        &self.remote_id
    }

    /// Returns whether deletion must be explicitly overridden.
    #[must_use]
    pub const fn is_protected(&self) -> bool {
        self.protected
    }

    /// Returns the non-sensitive inputs last sent to Dokploy.
    #[must_use]
    pub const fn last_applied(&self) -> &ManagedInputs {
        &self.last_applied
    }

    /// Returns the canonical dependency list.
    #[must_use]
    pub fn dependencies(&self) -> &[ResourceAddress] {
        &self.dependencies
    }
}

impl<'de> Deserialize<'de> for ResourceState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        #[serde(rename_all = "camelCase")]
        struct SerializedResourceState {
            kind: ResourceKind,
            remote_id: RemoteId,
            protected: bool,
            last_applied: ManagedInputs,
            dependencies: Vec<ResourceAddress>,
        }

        let state = SerializedResourceState::deserialize(deserializer)?;

        Ok(Self::new(
            state.kind,
            state.remote_id,
            state.protected,
            state.last_applied,
            state.dependencies,
        ))
    }
}

/// One versioned state lineage bound to exactly one Dokploy instance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateFile {
    format_version: u32,
    cli_version: Version,
    lineage: Uuid,
    serial: u64,
    instance: InstanceIdentity,
    resources: BTreeMap<ResourceAddress, ResourceState>,
}

/// The optimistic-concurrency identity of one durable state snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateRevision {
    lineage: Uuid,
    serial: u64,
}

impl StateRevision {
    /// Returns the state history this revision belongs to.
    #[must_use]
    pub const fn lineage(self) -> Uuid {
        self.lineage
    }

    /// Returns the monotonic version within the lineage.
    #[must_use]
    pub const fn serial(self) -> u64 {
        self.serial
    }
}

impl StateFile {
    /// Starts a new state lineage at serial zero.
    #[must_use]
    pub fn new(cli_version: Version, instance: InstanceIdentity) -> Self {
        Self {
            format_version: CURRENT_FORMAT_VERSION,
            cli_version,
            lineage: Uuid::new_v4(),
            serial: 0,
            instance,
            resources: BTreeMap::new(),
        }
    }

    /// Returns the on-disk state format version.
    #[must_use]
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Returns the CLI version that last wrote this state.
    #[must_use]
    pub const fn cli_version(&self) -> &Version {
        &self.cli_version
    }

    /// Returns the stable state-history identifier.
    #[must_use]
    pub const fn lineage(&self) -> Uuid {
        self.lineage
    }

    /// Returns the monotonic mutation number.
    #[must_use]
    pub const fn serial(&self) -> u64 {
        self.serial
    }

    /// Returns the lineage and serial required for a checked durable write.
    #[must_use]
    pub const fn revision(&self) -> StateRevision {
        StateRevision {
            lineage: self.lineage,
            serial: self.serial,
        }
    }

    /// Returns the instance that owns this state.
    #[must_use]
    pub const fn instance(&self) -> &InstanceIdentity {
        &self.instance
    }

    /// Returns all resources in stable logical-address order.
    #[must_use]
    pub const fn resources(&self) -> &BTreeMap<ResourceAddress, ResourceState> {
        &self.resources
    }

    /// Returns state for one logical address.
    #[must_use]
    pub fn resource(&self, address: &ResourceAddress) -> Option<&ResourceState> {
        self.resources.get(address)
    }

    /// Refuses to use this lineage with another Dokploy instance.
    pub fn ensure_instance(&self, current: &InstanceIdentity) -> Result<(), StateError> {
        if self.instance == *current {
            return Ok(());
        }

        Err(StateError::InstanceMismatch {
            state: self.instance.clone(),
            current: current.clone(),
        })
    }

    /// Inserts or replaces one resource and advances the serial exactly once.
    pub fn upsert_resource(
        &mut self,
        address: ResourceAddress,
        resource: ResourceState,
    ) -> Result<Option<ResourceState>, StateError> {
        if address.kind() != resource.kind() {
            return Err(StateError::ResourceKindMismatch {
                address,
                state_kind: resource.kind(),
            });
        }

        let next_serial = self.next_serial()?;
        let previous = self.resources.insert(address, resource);
        self.serial = next_serial;

        Ok(previous)
    }

    /// Removes one resource and advances the serial when state changed.
    pub fn remove_resource(
        &mut self,
        address: &ResourceAddress,
    ) -> Result<Option<ResourceState>, StateError> {
        if !self.resources.contains_key(address) {
            return Ok(None);
        }

        let next_serial = self.next_serial()?;
        let removed = self.resources.remove(address);
        self.serial = next_serial;

        Ok(removed)
    }

    fn next_serial(&self) -> Result<u64, StateError> {
        self.serial.checked_add(1).ok_or(StateError::SerialOverflow)
    }
}

impl<'de> Deserialize<'de> for StateFile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        #[serde(rename_all = "camelCase")]
        struct SerializedStateFile {
            format_version: u32,
            cli_version: Version,
            lineage: Uuid,
            serial: u64,
            instance: InstanceIdentity,
            resources: BTreeMap<ResourceAddress, ResourceState>,
        }

        let state = SerializedStateFile::deserialize(deserializer)?;
        if state.format_version != CURRENT_FORMAT_VERSION {
            return Err(de::Error::custom(StateError::UnsupportedFormatVersion {
                found: state.format_version,
                supported: CURRENT_FORMAT_VERSION,
            }));
        }

        if state.lineage.is_nil() {
            return Err(de::Error::custom(StateError::NilLineage));
        }

        for (address, resource) in &state.resources {
            if address.kind() != resource.kind() {
                return Err(de::Error::custom(StateError::ResourceKindMismatch {
                    address: address.clone(),
                    state_kind: resource.kind(),
                }));
            }
        }

        Ok(Self {
            format_version: state.format_version,
            cli_version: state.cli_version,
            lineage: state.lineage,
            serial: state.serial,
            instance: state.instance,
            resources: state.resources,
        })
    }
}

/// A state invariant that prevented a read or mutation.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StateError {
    #[error("state belongs to Dokploy instance `{state}`, but the current instance is `{current}`")]
    InstanceMismatch {
        state: InstanceIdentity,
        current: InstanceIdentity,
    },
    #[error(
        "resource `{address}` has kind `{state_kind}`, which does not match its logical address"
    )]
    ResourceKindMismatch {
        address: ResourceAddress,
        state_kind: ResourceKind,
    },
    #[error("unsupported state format version {found}; this CLI supports version {supported}")]
    UnsupportedFormatVersion { found: u32, supported: u32 },
    #[error("state serial cannot advance beyond its maximum value")]
    SerialOverflow,
    #[error("state lineage cannot be the nil UUID")]
    NilLineage,
}
