use std::{collections::BTreeMap, fmt};

use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::sensitive::valid_environment_name;
use crate::strict_json::reject_duplicate_keys;
use crate::{ResourceAddress, ResourceKind, SensitiveInputs, SensitivePropertyPath};

const CURRENT_FORMAT_VERSION: u32 = 3;

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

/// The non-sensitive inputs and explicit sensitive clears last sent to Dokploy.
///
/// This type is the state seam for managed JSON. It rejects scalar roots, raw
/// sensitive values, noncanonical environment entries, and unknown
/// secret-bearing field families recursively. Resource-specific adapters remain
/// responsible for passing only fields owned by their configuration schema;
/// secret values under an unrelated key cannot be inferred from JSON.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ManagedInputs(serde_json::Value);

impl fmt::Debug for ManagedInputs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ManagedInputs")
            .field(&"[REDACTED]")
            .finish()
    }
}

impl ManagedInputs {
    /// Validates JSON before it can cross into durable managed state.
    pub fn try_from_json(value: serde_json::Value) -> Result<Self, ManagedInputsError> {
        if !value.is_object() {
            return Err(ManagedInputsError::NotObject);
        }

        validate_managed_input_object(
            value
                .as_object()
                .expect("the managed input root was checked as an object"),
        )?;

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
    #[error("managed inputs contain a non-null sensitive value at `{path}`")]
    NonNullSensitiveValue { path: String },
    #[error("managed inputs contain an invalid application environment at `{path}`")]
    InvalidEnvironment { path: String },
}

fn validate_managed_input_object(
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ManagedInputsError> {
    for (key, value) in fields {
        let path = format!("$.{key}");
        match key.as_str() {
            "password" => {
                if !value.is_null() {
                    return Err(ManagedInputsError::NonNullSensitiveValue { path });
                }
            }
            "environment" => validate_environment_clears(value, path)?,
            _ => {
                if is_sensitive_key(key) {
                    return Err(ManagedInputsError::SensitiveField { path });
                }
                validate_non_sensitive_json(value, path)?;
            }
        }
    }

    Ok(())
}

fn validate_environment_clears(
    value: &serde_json::Value,
    path: String,
) -> Result<(), ManagedInputsError> {
    if value.is_null() {
        return Ok(());
    }
    let Some(entries) = value.as_object() else {
        return Err(ManagedInputsError::InvalidEnvironment { path });
    };

    for (name, value) in entries {
        let entry_path = format!("{path}.{name}");
        if !valid_environment_name(name) {
            return Err(ManagedInputsError::InvalidEnvironment { path: entry_path });
        }
        if !value.is_null() {
            return Err(ManagedInputsError::NonNullSensitiveValue { path: entry_path });
        }
    }

    Ok(())
}

fn validate_non_sensitive_json(
    value: &serde_json::Value,
    path: String,
) -> Result<(), ManagedInputsError> {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let child_path = format!("{path}.{key}");
                if key == "environment" || is_sensitive_key(key) {
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
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceState {
    kind: ResourceKind,
    remote_id: RemoteId,
    protected: bool,
    last_applied: ManagedInputs,
    sensitive_inputs: SensitiveInputs,
    containment: Option<ResourceAddress>,
    dependencies: Vec<ResourceAddress>,
}

impl fmt::Debug for ResourceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceState")
            .field("kind", &self.kind)
            .field("remote_id", &"[REDACTED]")
            .field("protected", &self.protected)
            .field("last_applied", &"[REDACTED]")
            .field(
                "sensitive_input_count",
                &self.sensitive_inputs.paths().count(),
            )
            .field("containment", &self.containment)
            .field("dependency_count", &self.dependencies.len())
            .finish()
    }
}

impl ResourceState {
    /// Creates resource state and canonicalizes dependency ordering.
    #[must_use]
    pub fn new(
        kind: ResourceKind,
        remote_id: RemoteId,
        protected: bool,
        last_applied: ManagedInputs,
        containment: Option<ResourceAddress>,
        dependencies: Vec<ResourceAddress>,
    ) -> Self {
        Self::try_new(
            kind,
            remote_id,
            protected,
            last_applied,
            SensitiveInputs::default(),
            containment,
            dependencies,
        )
        .expect("resource state containment must match its kind")
    }

    /// Creates resource state with opaque sensitive intent receipts.
    pub fn try_new(
        kind: ResourceKind,
        remote_id: RemoteId,
        protected: bool,
        last_applied: ManagedInputs,
        sensitive_inputs: SensitiveInputs,
        containment: Option<ResourceAddress>,
        mut dependencies: Vec<ResourceAddress>,
    ) -> Result<Self, ResourceStateError> {
        ensure_disjoint_inputs(&last_applied, &sensitive_inputs)?;
        validate_containment(kind, containment.as_ref())?;
        dependencies.sort();
        dependencies.dedup();

        Ok(Self {
            kind,
            remote_id,
            protected,
            last_applied,
            sensitive_inputs,
            containment,
            dependencies,
        })
    }

    /// Replaces the direct logical containment parent after validating its kind.
    pub fn with_containment(
        mut self,
        containment: Option<ResourceAddress>,
    ) -> Result<Self, ResourceStateError> {
        validate_containment(self.kind, containment.as_ref())?;
        self.containment = containment;
        Ok(self)
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

    /// Returns opaque receipts for non-null sensitive input intents.
    #[must_use]
    pub const fn sensitive_inputs(&self) -> &SensitiveInputs {
        &self.sensitive_inputs
    }

    /// Returns the direct logical containment parent.
    #[must_use]
    pub const fn containment(&self) -> Option<&ResourceAddress> {
        self.containment.as_ref()
    }

    /// Returns the canonical dependency list.
    #[must_use]
    pub fn dependencies(&self) -> &[ResourceAddress] {
        &self.dependencies
    }
}

struct RequiredContainment(Option<ResourceAddress>);

impl<'de> Deserialize<'de> for RequiredContainment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<ResourceAddress>::deserialize(deserializer).map(Self)
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
            sensitive_inputs: SensitiveInputs,
            containment: RequiredContainment,
            dependencies: Vec<ResourceAddress>,
        }

        let state = SerializedResourceState::deserialize(deserializer)?;

        Self::try_new(
            state.kind,
            state.remote_id,
            state.protected,
            state.last_applied,
            state.sensitive_inputs,
            state.containment.0,
            state.dependencies,
        )
        .map_err(de::Error::custom)
    }
}

/// Resource inputs that cannot be represented without ambiguous ownership.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ResourceStateError {
    #[error("managed and sensitive inputs overlap at `{path}`")]
    OverlappingInput { path: SensitivePropertyPath },
    #[error("resource kind `{kind}` requires containment by `{required}`")]
    MissingContainment {
        kind: ResourceKind,
        required: ResourceKind,
    },
    #[error("resource kind `{kind}` cannot have a containment parent")]
    UnexpectedContainment { kind: ResourceKind },
    #[error("resource kind `{kind}` requires containment by `{required}`, found `{found}`")]
    InvalidContainmentKind {
        kind: ResourceKind,
        required: ResourceKind,
        found: ResourceKind,
    },
}

fn validate_containment(
    kind: ResourceKind,
    containment: Option<&ResourceAddress>,
) -> Result<(), ResourceStateError> {
    match (kind.containment_parent_kind(), containment) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(ResourceStateError::UnexpectedContainment { kind }),
        (Some(required), None) => Err(ResourceStateError::MissingContainment { kind, required }),
        (Some(required), Some(parent)) if parent.kind() == required => Ok(()),
        (Some(required), Some(parent)) => Err(ResourceStateError::InvalidContainmentKind {
            kind,
            required,
            found: parent.kind(),
        }),
    }
}

fn ensure_disjoint_inputs(
    managed: &ManagedInputs,
    sensitive: &SensitiveInputs,
) -> Result<(), ResourceStateError> {
    let managed = managed
        .as_json()
        .as_object()
        .expect("ManagedInputs guarantees an object root");
    let environment = managed.get("environment");

    for path in sensitive.paths() {
        let overlaps = if path.is_password() {
            managed.contains_key("password")
        } else if let Some(name) = path.environment_name() {
            match environment {
                Some(serde_json::Value::Null) => true,
                Some(serde_json::Value::Object(entries)) if entries.is_empty() => true,
                Some(serde_json::Value::Object(entries)) => entries.contains_key(name),
                _ => false,
            }
        } else {
            false
        };

        if overlaps {
            return Err(ResourceStateError::OverlappingInput { path: path.clone() });
        }
    }

    Ok(())
}

/// One versioned state lineage bound to exactly one Dokploy instance.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateFile {
    format_version: u32,
    cli_version: Version,
    lineage: Uuid,
    serial: u64,
    instance: InstanceIdentity,
    resources: BTreeMap<ResourceAddress, ResourceState>,
}

impl fmt::Debug for StateFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StateFile")
            .field("format_version", &self.format_version)
            .field("cli_version", &self.cli_version)
            .field("lineage", &self.lineage)
            .field("serial", &self.serial)
            .field("instance", &self.instance)
            .field("resource_count", &self.resources.len())
            .finish()
    }
}

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

    /// Starts a new state lineage at serial zero with an atomically imported resource set.
    pub fn new_with_resources(
        cli_version: Version,
        instance: InstanceIdentity,
        resources: BTreeMap<ResourceAddress, ResourceState>,
    ) -> Result<Self, StateError> {
        let mut identities = BTreeMap::new();
        for (address, resource) in &resources {
            if let Some(first) = identities.insert(
                (resource.kind(), resource.remote_id().clone()),
                address.clone(),
            ) {
                return Err(StateError::DuplicateRemoteIdentity {
                    first,
                    second: address.clone(),
                });
            }
            if let Some(parent) = resource.containment()
                && !resources.contains_key(parent)
            {
                return Err(StateError::MissingResourceReference {
                    address: address.clone(),
                    reference: parent.clone(),
                });
            }
            for dependency in resource.dependencies() {
                if !resources.contains_key(dependency) {
                    return Err(StateError::MissingResourceReference {
                        address: address.clone(),
                        reference: dependency.clone(),
                    });
                }
            }
        }
        Self::from_serialized(SerializedStateFile {
            format_version: CURRENT_FORMAT_VERSION,
            cli_version,
            lineage: Uuid::new_v4(),
            serial: 0,
            instance,
            resources,
        })
    }

    /// Strictly decodes one state document, including duplicate-key checks.
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, StateDecodeError> {
        reject_duplicate_keys(bytes).map_err(|()| StateDecodeError)?;
        let state: SerializedStateFile =
            serde_json::from_slice(bytes).map_err(|_| StateDecodeError)?;

        Self::from_serialized(state).map_err(|_| StateDecodeError)
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

    /// Moves one logical address atomically and rewrites every stored reference.
    pub fn move_resource(
        &mut self,
        source: &ResourceAddress,
        target: ResourceAddress,
    ) -> Result<(), StateError> {
        let source_resource =
            self.resources
                .get(source)
                .ok_or_else(|| StateError::ResourceNotFound {
                    address: source.clone(),
                })?;
        if source.kind() != target.kind() {
            return Err(StateError::MoveKindMismatch {
                from: source.clone(),
                to: target,
            });
        }
        if source == &target {
            return Ok(());
        }
        if self.resources.contains_key(&target) {
            return Err(StateError::ResourceAlreadyExists { address: target });
        }
        debug_assert_eq!(source_resource.kind(), source.kind());

        let next_serial = self.next_serial()?;
        let mut resources = self.resources.clone();
        let resource = resources
            .remove(source)
            .expect("the move source was checked as present");
        resources.insert(target.clone(), resource);
        for resource in resources.values_mut() {
            if resource.containment.as_ref() == Some(source) {
                resource.containment = Some(target.clone());
            }
            for dependency in &mut resource.dependencies {
                if dependency == source {
                    *dependency = target.clone();
                }
            }
            resource.dependencies.sort();
            resource.dependencies.dedup();
        }

        self.resources = resources;
        self.serial = next_serial;

        Ok(())
    }

    /// Sets effective deletion protection and advances only when it changes.
    pub fn set_resource_protection(
        &mut self,
        address: &ResourceAddress,
        protected: bool,
    ) -> Result<bool, StateError> {
        let resource = self
            .resources
            .get(address)
            .ok_or_else(|| StateError::ResourceNotFound {
                address: address.clone(),
            })?;
        if resource.protected == protected {
            return Ok(false);
        }

        let next_serial = self.next_serial()?;
        self.resources
            .get_mut(address)
            .expect("the protected resource was checked as present")
            .protected = protected;
        self.serial = next_serial;

        Ok(true)
    }

    /// Forgets local ownership without a remote mutation when no resource depends on it.
    pub fn forget_resource(
        &mut self,
        address: &ResourceAddress,
    ) -> Result<Option<ResourceState>, StateError> {
        if !self.resources.contains_key(address) {
            return Ok(None);
        }
        let dependents = self
            .resources
            .iter()
            .filter_map(|(candidate, resource)| {
                (candidate != address
                    && (resource.containment.as_ref() == Some(address)
                        || resource.dependencies.contains(address)))
                .then_some(candidate.clone())
            })
            .collect::<Vec<_>>();
        if !dependents.is_empty() {
            return Err(StateError::ResourceHasDependents {
                address: address.clone(),
                dependents,
            });
        }

        let next_serial = self.next_serial()?;
        let removed = self.resources.remove(address);
        self.serial = next_serial;

        Ok(removed)
    }

    fn next_serial(&self) -> Result<u64, StateError> {
        self.serial.checked_add(1).ok_or(StateError::SerialOverflow)
    }

    fn from_serialized(state: SerializedStateFile) -> Result<Self, StateError> {
        if state.format_version != CURRENT_FORMAT_VERSION {
            return Err(StateError::UnsupportedFormatVersion {
                found: state.format_version,
                supported: CURRENT_FORMAT_VERSION,
            });
        }

        if state.lineage.is_nil() {
            return Err(StateError::NilLineage);
        }

        for (address, resource) in &state.resources {
            if address.kind() != resource.kind() {
                return Err(StateError::ResourceKindMismatch {
                    address: address.clone(),
                    state_kind: resource.kind(),
                });
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

/// A deliberately detail-free state decoding failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("state JSON is malformed or violates state invariants")]
pub struct StateDecodeError;

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
    #[error("resources `{first}` and `{second}` use the same remote identity")]
    DuplicateRemoteIdentity {
        first: ResourceAddress,
        second: ResourceAddress,
    },
    #[error("resource `{address}` references missing resource `{reference}`")]
    MissingResourceReference {
        address: ResourceAddress,
        reference: ResourceAddress,
    },
    #[error("resource `{address}` does not exist in state")]
    ResourceNotFound { address: ResourceAddress },
    #[error("resource `{address}` already exists in state")]
    ResourceAlreadyExists { address: ResourceAddress },
    #[error("cannot move `{from}` to different resource kind `{to}`")]
    MoveKindMismatch {
        from: ResourceAddress,
        to: ResourceAddress,
    },
    #[error("resource `{address}` is still referenced by {dependents:?}")]
    ResourceHasDependents {
        address: ResourceAddress,
        dependents: Vec<ResourceAddress>,
    },
    #[error("unsupported state format version {found}; this CLI supports version {supported}")]
    UnsupportedFormatVersion { found: u32, supported: u32 },
    #[error("state serial cannot advance beyond its maximum value")]
    SerialOverflow,
    #[error("state lineage cannot be the nil UUID")]
    NilLineage,
}
