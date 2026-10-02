use std::{collections::BTreeMap, fmt};

use dokploy_state::{InstanceIdentity, RemoteId, ResourceAddress, ResourceKind, StateFile};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;

use crate::{ComparableValue, OwnedValue, PropertyPath, SensitiveIntent};

/// A lowercase SHA-256 digest of the configuration used to build desired state.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConfigDigest(String);

impl ConfigDigest {
    /// Parses a 64-character lowercase hexadecimal digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, ConfigDigestError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ConfigDigestError);
        }

        Ok(Self(value))
    }

    /// Returns the canonical lowercase hexadecimal representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ConfigDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ConfigDigest")
            .field(&self.0)
            .finish()
    }
}

/// A malformed configuration digest.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("configuration digest must be 64 lowercase hexadecimal characters")]
pub struct ConfigDigestError;

/// Desired ownership of durable resource protection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProtectionIntent {
    /// Protection is omitted and its stored value remains unmanaged.
    #[default]
    Unmanaged,
    /// Protection is explicitly owned with this value.
    Set(bool),
}

/// Desired properties and lifecycle metadata for one resource.
pub struct DesiredResource {
    pub(crate) properties: BTreeMap<PropertyPath, OwnedValue>,
    pub(crate) protection: ProtectionIntent,
    pub(crate) containment: Option<ResourceAddress>,
    pub(crate) dependencies: Vec<ResourceAddress>,
    pub(crate) ignore_changes: Vec<PropertyPath>,
    pub(crate) replace_on_changes: Vec<PropertyPath>,
}

impl DesiredResource {
    /// Creates a resource with no protection ownership, dependencies, or lifecycle overrides.
    #[must_use]
    pub fn new(properties: BTreeMap<PropertyPath, OwnedValue>) -> Self {
        Self {
            properties,
            protection: ProtectionIntent::Unmanaged,
            containment: None,
            dependencies: Vec::new(),
            ignore_changes: Vec::new(),
            replace_on_changes: Vec::new(),
        }
    }

    /// Sets ownership-aware protection intent.
    #[must_use]
    pub fn with_protection(mut self, protection: ProtectionIntent) -> Self {
        self.protection = protection;
        self
    }

    /// Sets the direct logical containment parent.
    #[must_use]
    pub fn with_containment(mut self, containment: Option<ResourceAddress>) -> Self {
        self.containment = containment;
        self
    }

    /// Sets canonical desired dependencies.
    #[must_use]
    pub fn with_dependencies(mut self, mut dependencies: Vec<ResourceAddress>) -> Self {
        dependencies.sort();
        dependencies.dedup();
        self.dependencies = dependencies;
        self
    }

    /// Adds ignored property paths in deterministic order.
    #[must_use]
    pub fn with_ignored_changes(mut self, mut properties: Vec<PropertyPath>) -> Self {
        properties.sort();
        properties.dedup();
        self.ignore_changes = properties;
        self
    }

    /// Adds replacement property metadata for a future planner checkpoint.
    #[must_use]
    pub fn with_replacement_changes(mut self, mut properties: Vec<PropertyPath>) -> Self {
        properties.sort();
        properties.dedup();
        self.replace_on_changes = properties;
        self
    }

    /// Returns owned properties in stable key order.
    #[must_use]
    pub const fn properties(&self) -> &BTreeMap<PropertyPath, OwnedValue> {
        &self.properties
    }

    /// Returns protection intent.
    #[must_use]
    pub const fn protection(&self) -> ProtectionIntent {
        self.protection
    }

    /// Returns the direct logical containment parent.
    #[must_use]
    pub const fn containment(&self) -> Option<&ResourceAddress> {
        self.containment.as_ref()
    }

    /// Returns canonical desired dependencies.
    #[must_use]
    pub fn dependencies(&self) -> &[ResourceAddress] {
        &self.dependencies
    }

    /// Returns lifecycle paths that must retain their previous ownership baseline.
    #[must_use]
    pub fn ignored_changes(&self) -> &[PropertyPath] {
        &self.ignore_changes
    }
}

impl fmt::Debug for DesiredResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesiredResource")
            .field("property_count", &self.properties.len())
            .field("protection", &self.protection)
            .field("containment", &self.containment)
            .field("dependency_count", &self.dependencies.len())
            .field("ignored_property_count", &self.ignore_changes.len())
            .field("replacement_property_count", &self.replace_on_changes.len())
            .finish()
    }
}

/// A requested identity-preserving logical-address move.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MoveDirective {
    from: ResourceAddress,
    to: ResourceAddress,
}

impl MoveDirective {
    /// Creates a requested logical-address move.
    #[must_use]
    pub fn new(from: ResourceAddress, to: ResourceAddress) -> Self {
        Self { from, to }
    }

    /// Returns the current logical address.
    #[must_use]
    pub const fn from(&self) -> &ResourceAddress {
        &self.from
    }

    /// Returns the requested logical address.
    #[must_use]
    pub const fn to(&self) -> &ResourceAddress {
        &self.to
    }
}

/// A requested retain-or-destroy removal from managed state.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RemovalDirective {
    address: ResourceAddress,
    destroy: bool,
}

impl RemovalDirective {
    /// Creates a removal request.
    #[must_use]
    pub fn new(address: ResourceAddress, destroy: bool) -> Self {
        Self { address, destroy }
    }

    /// Returns the managed address to remove.
    #[must_use]
    pub const fn address(&self) -> &ResourceAddress {
        &self.address
    }

    /// Returns whether the remote object should eventually be destroyed.
    #[must_use]
    pub const fn destroy(&self) -> bool {
        self.destroy
    }
}

/// Desired resources and lifecycle directives indexed by logical address.
pub struct DesiredState {
    pub(crate) digest: ConfigDigest,
    pub(crate) resources: BTreeMap<ResourceAddress, DesiredResource>,
    pub(crate) moves: Vec<MoveDirective>,
    pub(crate) removals: Vec<RemovalDirective>,
}

impl DesiredState {
    /// Creates desired state without move or removal directives.
    pub fn try_new(
        digest: ConfigDigest,
        resources: BTreeMap<ResourceAddress, DesiredResource>,
    ) -> Result<Self, DesiredStateError> {
        for (address, resource) in &resources {
            validate_desired_resource(address, resource)?;
            for dependency in &resource.dependencies {
                if dependency == address {
                    return Err(DesiredStateError::SelfDependency {
                        address: address.clone(),
                    });
                }
                if !resources.contains_key(dependency) {
                    return Err(DesiredStateError::MissingDependency {
                        address: address.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
            validate_desired_containment(address, resource)?;
        }

        Ok(Self {
            digest,
            resources,
            moves: Vec::new(),
            removals: Vec::new(),
        })
    }

    /// Orders move directives canonically while preserving duplicates for validation.
    #[must_use]
    pub fn with_moves(mut self, mut moves: Vec<MoveDirective>) -> Self {
        moves.sort();
        self.moves = moves;
        self
    }

    /// Orders removal directives canonically while preserving duplicates for validation.
    #[must_use]
    pub fn with_removals(mut self, mut removals: Vec<RemovalDirective>) -> Self {
        removals.sort();
        self.removals = removals;
        self
    }

    /// Returns the digest of the configuration that produced this snapshot.
    #[must_use]
    pub const fn digest(&self) -> &ConfigDigest {
        &self.digest
    }

    /// Returns desired resources in stable logical-address order.
    #[must_use]
    pub const fn resources(&self) -> &BTreeMap<ResourceAddress, DesiredResource> {
        &self.resources
    }

    /// Returns requested moves in stable order.
    #[must_use]
    pub fn moves(&self) -> &[MoveDirective] {
        &self.moves
    }

    /// Returns requested removals in stable order.
    #[must_use]
    pub fn removals(&self) -> &[RemovalDirective] {
        &self.removals
    }
}

/// Desired state that cannot safely enter the planner.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DesiredStateError {
    /// A property path is not valid for the resource kind or is lifecycle-only.
    #[error("desired resource `{address}` contains an invalid property path")]
    InvalidPropertyPath { address: ResourceAddress },
    /// A property value violates its path's sensitivity or root contract.
    #[error("desired resource `{address}` contains an invalid property value")]
    InvalidPropertyValue { address: ResourceAddress },
    /// An ignored path is sensitive or represents a structural collection root.
    #[error("desired resource `{address}` contains an unsafe ignored property")]
    InvalidIgnoredProperty { address: ResourceAddress },
    /// Ignore and replacement selectors overlap directly or structurally.
    #[error("desired resource `{address}` contains conflicting lifecycle paths")]
    ConflictingLifecyclePaths { address: ResourceAddress },
    /// A collection root and one of its child paths were both supplied.
    #[error("desired resource `{address}` contains conflicting property paths")]
    ConflictingPropertyPaths { address: ResourceAddress },
    /// A resource cannot depend on itself.
    #[error("desired resource `{address}` depends on itself")]
    SelfDependency { address: ResourceAddress },
    /// A resource dependency is absent from desired state.
    #[error("desired resource `{address}` depends on missing resource `{dependency}`")]
    MissingDependency {
        address: ResourceAddress,
        dependency: ResourceAddress,
    },
    /// A nested resource has no direct containment parent.
    #[error("desired resource `{address}` is missing its containment parent")]
    MissingContainment { address: ResourceAddress },
    /// A top-level resource cannot have a containment parent.
    #[error("desired resource `{address}` cannot have a containment parent")]
    UnexpectedContainment { address: ResourceAddress },
    /// A containment parent has the wrong resource kind.
    #[error("desired resource `{address}` has an invalid containment parent `{parent}`")]
    InvalidContainmentKind {
        address: ResourceAddress,
        parent: ResourceAddress,
    },
}

fn validate_desired_containment(
    address: &ResourceAddress,
    resource: &DesiredResource,
) -> Result<(), DesiredStateError> {
    match (
        address.kind().containment_parent_kind(),
        resource.containment.as_ref(),
    ) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(DesiredStateError::UnexpectedContainment {
            address: address.clone(),
        }),
        (Some(_), None) => Err(DesiredStateError::MissingContainment {
            address: address.clone(),
        }),
        (Some(required), Some(parent)) if parent.kind() != required => {
            Err(DesiredStateError::InvalidContainmentKind {
                address: address.clone(),
                parent: parent.clone(),
            })
        }
        (Some(_), Some(_)) => Ok(()),
    }
}

fn validate_desired_resource(
    address: &ResourceAddress,
    resource: &DesiredResource,
) -> Result<(), DesiredStateError> {
    for (path, value) in &resource.properties {
        if !path.valid_for_kind(address.kind()) || path.is_lifecycle_only() {
            return Err(DesiredStateError::InvalidPropertyPath {
                address: address.clone(),
            });
        }
        if !owned_value_valid(path, value) || !kind_value_valid(address.kind(), path, value) {
            return Err(DesiredStateError::InvalidPropertyValue {
                address: address.clone(),
            });
        }
    }
    for path in resource
        .ignore_changes
        .iter()
        .chain(&resource.replace_on_changes)
    {
        if !path.valid_for_kind(address.kind()) {
            return Err(DesiredStateError::InvalidPropertyPath {
                address: address.clone(),
            });
        }
    }
    if resource.ignore_changes.iter().any(|path| {
        path.is_sensitive() || matches!(path, PropertyPath::Source | PropertyPath::Environment)
    }) {
        return Err(DesiredStateError::InvalidIgnoredProperty {
            address: address.clone(),
        });
    }
    if resource.ignore_changes.iter().any(|ignored| {
        resource
            .replace_on_changes
            .iter()
            .any(|replacement| property_paths_overlap(ignored, replacement))
    }) {
        return Err(DesiredStateError::ConflictingLifecyclePaths {
            address: address.clone(),
        });
    }
    if resource.properties.contains_key(&PropertyPath::Source)
        && resource
            .ignore_changes
            .iter()
            .any(PropertyPath::is_source_child)
    {
        return Err(DesiredStateError::ConflictingLifecyclePaths {
            address: address.clone(),
        });
    }

    validate_root_child_combinations(address, &resource.properties).map_err(|()| {
        DesiredStateError::ConflictingPropertyPaths {
            address: address.clone(),
        }
    })?;
    if !owned_source_shape_valid(&resource.properties) {
        return Err(DesiredStateError::InvalidPropertyValue {
            address: address.clone(),
        });
    }
    Ok(())
}

fn property_paths_overlap(left: &PropertyPath, right: &PropertyPath) -> bool {
    left == right
        || matches!(left, PropertyPath::Source) && right.is_source_child()
        || matches!(right, PropertyPath::Source) && left.is_source_child()
        || matches!(left, PropertyPath::Environment) && right.is_environment_child()
        || matches!(right, PropertyPath::Environment) && left.is_environment_child()
}

fn owned_value_valid(path: &PropertyPath, value: &OwnedValue) -> bool {
    if matches!(
        path,
        PropertyPath::FileContent | PropertyPath::Command | PropertyPath::Script
    ) {
        return matches!(value, OwnedValue::Sensitive(_));
    }
    if path.is_sensitive() {
        return matches!(value, OwnedValue::Null | OwnedValue::Sensitive(_));
    }
    match path {
        PropertyPath::Source => matches!(value, OwnedValue::Null),
        PropertyPath::SourceRepository => matches!(value, OwnedValue::Value(_)),
        PropertyPath::Environment => {
            matches!(value, OwnedValue::Null | OwnedValue::EmptyCollection)
        }
        PropertyPath::Node => libsql_node_value_valid(value),
        PropertyPath::PublishedPort | PropertyPath::TargetPort => port_number_value_valid(value),
        PropertyPath::PublishMode => port_string_value_valid(value, &["ingress", "host"]),
        PropertyPath::Protocol => port_string_value_valid(value, &["tcp", "udp"]),
        PropertyPath::Regex | PropertyPath::Replacement => non_empty_string_value_valid(value),
        PropertyPath::Permanent => {
            matches!(value, OwnedValue::Value(value) if value.as_json().is_boolean())
        }
        PropertyPath::Target => matches!(
            value,
            OwnedValue::Value(value) if mount_target_text_valid(value.as_json())
        ),
        PropertyPath::MountType => port_string_value_valid(value, &["bind", "volume", "file"]),
        PropertyPath::MountPath
        | PropertyPath::HostPath
        | PropertyPath::VolumeName
        | PropertyPath::FilePath => matches!(
            value,
            OwnedValue::Value(value) if mount_text_valid(value.as_json())
        ),
        PropertyPath::ScheduleName | PropertyPath::ServiceName | PropertyPath::Timezone => {
            matches!(
                value,
                OwnedValue::Value(value) if schedule_text_valid(value.as_json())
            )
        }
        PropertyPath::CronExpression => matches!(
            value,
            OwnedValue::Value(value) if schedule_text_valid(value.as_json())
        ),
        PropertyPath::ShellType => port_string_value_valid(value, &["bash", "sh"]),
        PropertyPath::Enabled => {
            matches!(value, OwnedValue::Value(value) if value.as_json().is_boolean())
        }
        PropertyPath::Server
        | PropertyPath::BuildServer
        | PropertyPath::Registry
        | PropertyPath::BuildRegistry
        | PropertyPath::RollbackRegistry
        | PropertyPath::Destination => selector_owned_value_valid(path, value),
        PropertyPath::Schedule => matches!(
            value,
            OwnedValue::Value(value) if value.as_json().as_str().is_some_and(backup_schedule_valid)
        ),
        PropertyPath::Prefix => matches!(
            value,
            OwnedValue::Value(value) if value.as_json().as_str().is_some_and(backup_prefix_valid)
        ),
        PropertyPath::IncludeEncryptionKey => {
            matches!(value, OwnedValue::Value(value) if value.as_json().is_boolean())
        }
        PropertyPath::KeepLatest => match value {
            OwnedValue::Null => true,
            OwnedValue::Value(value) => keep_latest_json_valid(value.as_json()),
            OwnedValue::EmptyCollection | OwnedValue::Sensitive(_) => false,
        },
        _ => matches!(value, OwnedValue::Null | OwnedValue::Value(_)),
    }
}

/// Rejects values that are valid for a shared path but invalid for one leaf kind.
fn kind_value_valid(kind: ResourceKind, path: &PropertyPath, value: &OwnedValue) -> bool {
    match (kind, path) {
        (ResourceKind::Security, PropertyPath::Username) => non_empty_string_value_valid(value),
        (ResourceKind::Security, PropertyPath::Password) => {
            matches!(value, OwnedValue::Sensitive(_))
        }
        (ResourceKind::Schedule, PropertyPath::Target) => matches!(
            value,
            OwnedValue::Value(value) if schedule_target_text_valid(value.as_json())
        ),
        (ResourceKind::Schedule, PropertyPath::Description) => matches!(
            value,
            OwnedValue::Value(value) if schedule_text_valid(value.as_json())
        ),
        (ResourceKind::Backup, PropertyPath::Target) => matches!(
            value,
            OwnedValue::Value(value) if backup_target_text_valid(value.as_json())
        ),
        (ResourceKind::Backup, PropertyPath::Database) => matches!(
            value,
            OwnedValue::Value(value) if value.as_json().as_str().is_some_and(backup_database_valid)
        ),
        _ => true,
    }
}

fn non_empty_string_value_valid(value: &OwnedValue) -> bool {
    matches!(
        value,
        OwnedValue::Value(value)
            if value.as_json().as_str().is_some_and(|text| !text.is_empty())
    )
}

/// Returns whether a JSON value is one logical address inside the closed Mount target union.
fn mount_target_text_valid(value: &serde_json::Value) -> bool {
    value
        .as_str()
        .and_then(|text| text.parse::<ResourceAddress>().ok())
        .is_some_and(|address| address.kind().is_mount_target())
}

/// Returns whether a JSON value is one logical address inside the closed Schedule target union.
fn schedule_target_text_valid(value: &serde_json::Value) -> bool {
    value
        .as_str()
        .and_then(|text| text.parse::<ResourceAddress>().ok())
        .is_some_and(|address| address.kind().is_schedule_target())
}

/// Returns whether a JSON value is a bounded, control-free, trimmed, nonempty Schedule string.
fn schedule_text_valid(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|text| {
        !text.is_empty()
            && text.len() <= 4096
            && text.trim() == text
            && !text.chars().any(char::is_control)
    })
}

/// Returns whether a JSON value is one logical address inside the closed Backup target union.
fn backup_target_text_valid(value: &serde_json::Value) -> bool {
    value
        .as_str()
        .and_then(|text| text.parse::<ResourceAddress>().ok())
        .is_some_and(|address| address.kind().is_backup_target())
}

/// Mirrors the configuration grammar for a five- or six-field Backup cron schedule.
fn backup_schedule_valid(text: &str) -> bool {
    let fields = text.split(' ').collect::<Vec<_>>();
    text.len() <= 128
        && (5..=6).contains(&fields.len())
        && fields.iter().all(|field| {
            !field.is_empty()
                && field.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '*' | '/' | ',' | '?' | '#' | '-')
                })
        })
}

fn backup_prefix_valid(text: &str) -> bool {
    !text.is_empty() && text.len() <= 512 && !text.chars().any(char::is_control)
}

fn backup_database_valid(text: &str) -> bool {
    !text.is_empty() && text.len() <= 255 && !text.chars().any(char::is_control)
}

fn keep_latest_json_valid(value: &serde_json::Value) -> bool {
    value
        .as_u64()
        .is_some_and(|count| (1..=u64::from(u32::MAX)).contains(&count))
}

/// Returns whether a JSON value is a bounded, control-free, nonempty Mount string.
fn mount_text_valid(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|text| {
        !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
    })
}

/// Returns whether a JSON value is a stable external selector accepted at `path`.
///
/// Only server placement accepts the `local` form; every selector otherwise uses
/// one exact `name`. Physical external identities are never valid selectors.
fn selector_json_valid(path: &PropertyPath, value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.len() != 1 {
        return false;
    }
    match (object.get("local"), object.get("name")) {
        (Some(local), None) => *path == PropertyPath::Server && local == &serde_json::json!(true),
        (None, Some(name)) => name.as_str().is_some_and(external_name_valid),
        _ => false,
    }
}

/// Mirrors the configuration grammar for exact external record names.
fn external_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name.trim() == name
        && !name.chars().any(char::is_control)
}

fn selector_owned_value_valid(path: &PropertyPath, value: &OwnedValue) -> bool {
    match value {
        // Server placement and the backup destination cannot be cleared; local is explicit.
        OwnedValue::Null => !matches!(path, PropertyPath::Server | PropertyPath::Destination),
        OwnedValue::Value(value) => selector_json_valid(path, value.as_json()),
        OwnedValue::EmptyCollection | OwnedValue::Sensitive(_) => false,
    }
}

fn selector_observation_valid(path: &PropertyPath, observation: &PropertyObservation) -> bool {
    match observation {
        PropertyObservation::Known(value) => selector_json_valid(path, value.as_json()),
        PropertyObservation::KnownAbsent => {
            !matches!(path, PropertyPath::Server | PropertyPath::Destination)
        }
        PropertyObservation::Unknown(reason) => *reason != PropertyUnknownReason::Sensitive,
    }
}

fn port_number_value_valid(value: &OwnedValue) -> bool {
    matches!(
        value,
        OwnedValue::Value(value)
            if value
                .as_json()
                .as_u64()
                .is_some_and(|number| (1..=u64::from(u16::MAX)).contains(&number))
    )
}

fn port_string_value_valid(value: &OwnedValue, allowed: &[&str]) -> bool {
    matches!(
        value,
        OwnedValue::Value(value)
            if value
                .as_json()
                .as_str()
                .is_some_and(|candidate| allowed.contains(&candidate))
    )
}

fn libsql_node_value_valid(value: &OwnedValue) -> bool {
    let OwnedValue::Value(value) = value else {
        return matches!(value, OwnedValue::Null);
    };
    let Some(node) = value.as_json().as_object() else {
        return false;
    };

    match node.get("type").and_then(serde_json::Value::as_str) {
        Some("primary") => node.len() == 1,
        Some("replica") => {
            node.len() == 2
                && node
                    .get("primary_url")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|primary_url| !primary_url.is_empty())
        }
        _ => false,
    }
}

pub(crate) fn owned_source_shape_valid(properties: &BTreeMap<PropertyPath, OwnedValue>) -> bool {
    !properties.contains_key(&PropertyPath::SourceBranch)
        || matches!(
            properties.get(&PropertyPath::SourceRepository),
            Some(OwnedValue::Value(_))
        )
}

fn validate_root_child_combinations<V>(
    _address: &ResourceAddress,
    properties: &BTreeMap<PropertyPath, V>,
) -> Result<(), ()> {
    if properties.contains_key(&PropertyPath::Source)
        && properties.keys().any(PropertyPath::is_source_child)
    {
        return Err(());
    }
    if properties.contains_key(&PropertyPath::Environment)
        && properties.keys().any(PropertyPath::is_environment_child)
    {
        return Err(());
    }
    Ok(())
}

impl fmt::Debug for DesiredState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesiredState")
            .field("digest", &self.digest)
            .field("resource_count", &self.resources.len())
            .field("move_count", &self.moves.len())
            .field("removal_count", &self.removals.len())
            .finish()
    }
}

pub(crate) struct StoredResource {
    pub(crate) remote_id: RemoteId,
    pub(crate) protected: bool,
    pub(crate) properties: BTreeMap<PropertyPath, OwnedValue>,
    pub(crate) containment: Option<ResourceAddress>,
    pub(crate) dependencies: Vec<ResourceAddress>,
}

/// The durable managed snapshot used for three-way comparisons.
pub struct StoredState {
    pub(crate) lineage: Uuid,
    pub(crate) serial: u64,
    pub(crate) instance: InstanceIdentity,
    pub(crate) resources: BTreeMap<ResourceAddress, StoredResource>,
}

impl StoredState {
    /// Creates a deterministic ephemeral baseline for a workspace without state.
    #[must_use]
    pub fn absent(instance: InstanceIdentity) -> Self {
        Self {
            lineage: Uuid::nil(),
            serial: 0,
            instance,
            resources: BTreeMap::new(),
        }
    }

    /// Projects a durable state file into typed planner properties.
    pub fn try_from_state(state: &StateFile) -> Result<Self, StoredStateError> {
        let mut resources = BTreeMap::new();
        let mut physical_identities = BTreeMap::new();

        for (address, resource) in state.resources() {
            let identity = (resource.kind(), resource.remote_id().clone());
            if let Some(first) = physical_identities.insert(identity, address.clone()) {
                return Err(StoredStateError::DuplicateRemoteIdentity {
                    first,
                    second: address.clone(),
                });
            }

            let mut properties = project_managed_inputs(
                address,
                resource.kind(),
                resource.last_applied().as_json(),
            )?;
            for sensitive_path in resource.sensitive_inputs().paths() {
                let path: PropertyPath = sensitive_path.to_string().parse().map_err(|_| {
                    StoredStateError::UnsupportedProperty {
                        address: address.clone(),
                    }
                })?;
                if !path.is_sensitive() || !path.valid_for_kind(resource.kind()) {
                    return Err(StoredStateError::InvalidPropertyPath {
                        address: address.clone(),
                    });
                }
                let fingerprint = resource
                    .sensitive_inputs()
                    .fingerprint(sensitive_path)
                    .expect("a sensitive input path always has a receipt")
                    .clone();
                insert_projected(
                    address,
                    &mut properties,
                    path,
                    OwnedValue::Sensitive(SensitiveIntent::from_fingerprint(fingerprint)),
                )?;
            }
            validate_root_child_combinations(address, &properties).map_err(|()| {
                StoredStateError::ConflictingPropertyPaths {
                    address: address.clone(),
                }
            })?;

            resources.insert(
                address.clone(),
                StoredResource {
                    remote_id: resource.remote_id().clone(),
                    protected: resource.is_protected(),
                    properties,
                    containment: resource.containment().cloned(),
                    dependencies: resource.dependencies().to_vec(),
                },
            );
        }

        Ok(Self {
            lineage: state.lineage(),
            serial: state.serial(),
            instance: state.instance().clone(),
            resources,
        })
    }

    /// Returns the state lineage used by the plan.
    #[must_use]
    pub const fn lineage(&self) -> Uuid {
        self.lineage
    }

    /// Returns the state serial used by the plan.
    #[must_use]
    pub const fn serial(&self) -> u64 {
        self.serial
    }

    /// Returns the Dokploy instance owning this state.
    #[must_use]
    pub const fn instance(&self) -> &InstanceIdentity {
        &self.instance
    }

    /// Returns one value-free durable property projection.
    #[must_use]
    pub fn property(&self, address: &ResourceAddress, path: &PropertyPath) -> Option<&OwnedValue> {
        self.resources
            .get(address)
            .and_then(|resource| resource.properties.get(path))
    }

    /// Returns every value-free durable property projection of one resource.
    ///
    /// Offline callers use this to prove that desired state and durable state own
    /// exactly the same properties, which a per-path lookup cannot show.
    #[must_use]
    pub fn properties(
        &self,
        address: &ResourceAddress,
    ) -> Option<&BTreeMap<PropertyPath, OwnedValue>> {
        self.resources
            .get(address)
            .map(|resource| &resource.properties)
    }
}

impl fmt::Debug for StoredState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredState")
            .field("lineage", &self.lineage)
            .field("serial", &self.serial)
            .field("instance", &self.instance)
            .field("resource_count", &self.resources.len())
            .finish()
    }
}

/// Durable state that cannot be projected safely into planner input.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StoredStateError {
    /// Two addresses of the same resource kind share one physical identity.
    #[error("stored state maps one physical resource to both `{first}` and `{second}`")]
    DuplicateRemoteIdentity {
        /// The first logical address assigned to the physical resource.
        first: ResourceAddress,
        /// The second logical address assigned to the physical resource.
        second: ResourceAddress,
    },
    /// Durable managed inputs contain a property outside the typed MVP vocabulary.
    #[error("stored resource `{address}` contains an unsupported managed property")]
    UnsupportedProperty {
        /// The resource whose durable inputs cannot be projected.
        address: ResourceAddress,
    },
    /// A known property path is invalid for its stored resource kind.
    #[error("stored resource `{address}` contains an invalid property path")]
    InvalidPropertyPath { address: ResourceAddress },
    /// A stored root or value shape cannot be projected without guessing.
    #[error("stored resource `{address}` contains an invalid property value")]
    InvalidPropertyValue { address: ResourceAddress },
    /// A stored collection root conflicts with one of its child paths.
    #[error("stored resource `{address}` contains conflicting property paths")]
    ConflictingPropertyPaths { address: ResourceAddress },
}

fn project_managed_inputs(
    address: &ResourceAddress,
    kind: ResourceKind,
    inputs: &serde_json::Value,
) -> Result<BTreeMap<PropertyPath, OwnedValue>, StoredStateError> {
    let object = inputs
        .as_object()
        .expect("ManagedInputs guarantees an object root");
    let mut properties = BTreeMap::new();
    for (raw_path, raw_value) in object {
        match raw_path.as_str() {
            "source" => project_source(address, raw_value, &mut properties)?,
            "environment" => project_environment(address, raw_value, &mut properties)?,
            _ => {
                let path: PropertyPath =
                    raw_path
                        .parse()
                        .map_err(|_| StoredStateError::UnsupportedProperty {
                            address: address.clone(),
                        })?;
                if path.is_lifecycle_only() || !path.valid_for_kind(kind) {
                    return Err(StoredStateError::InvalidPropertyPath {
                        address: address.clone(),
                    });
                }
                let value = project_owned_value(&path, raw_value);
                insert_projected(address, &mut properties, path, value)?;
            }
        }
    }

    for (path, value) in &properties {
        if !path.valid_for_kind(kind) {
            return Err(StoredStateError::InvalidPropertyPath {
                address: address.clone(),
            });
        }
        if !owned_value_valid(path, value) || !kind_value_valid(kind, path, value) {
            return Err(StoredStateError::InvalidPropertyValue {
                address: address.clone(),
            });
        }
    }
    validate_root_child_combinations(address, &properties).map_err(|()| {
        StoredStateError::ConflictingPropertyPaths {
            address: address.clone(),
        }
    })?;
    if !owned_source_shape_valid(&properties) {
        return Err(StoredStateError::InvalidPropertyValue {
            address: address.clone(),
        });
    }
    Ok(properties)
}

fn project_source(
    address: &ResourceAddress,
    value: &serde_json::Value,
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
) -> Result<(), StoredStateError> {
    if value.is_null() {
        return insert_projected(address, properties, PropertyPath::Source, OwnedValue::Null);
    }
    let Some(source) = value.as_object() else {
        return Err(StoredStateError::InvalidPropertyValue {
            address: address.clone(),
        });
    };
    if source.is_empty() {
        return Err(StoredStateError::InvalidPropertyValue {
            address: address.clone(),
        });
    }
    for (name, value) in source {
        let path = match name.as_str() {
            "repository" => PropertyPath::SourceRepository,
            "branch" => PropertyPath::SourceBranch,
            _ => {
                return Err(StoredStateError::UnsupportedProperty {
                    address: address.clone(),
                });
            }
        };
        insert_projected(
            address,
            properties,
            path.clone(),
            project_owned_value(&path, value),
        )?;
    }
    Ok(())
}

fn project_environment(
    address: &ResourceAddress,
    value: &serde_json::Value,
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
) -> Result<(), StoredStateError> {
    if value.is_null() {
        return insert_projected(
            address,
            properties,
            PropertyPath::Environment,
            OwnedValue::Null,
        );
    }
    let Some(environment) = value.as_object() else {
        return Err(StoredStateError::InvalidPropertyValue {
            address: address.clone(),
        });
    };
    if environment.is_empty() {
        return insert_projected(
            address,
            properties,
            PropertyPath::Environment,
            OwnedValue::EmptyCollection,
        );
    }
    for (name, value) in environment {
        let path = PropertyPath::environment_variable(name.clone()).map_err(|_| {
            StoredStateError::InvalidPropertyPath {
                address: address.clone(),
            }
        })?;
        insert_projected(
            address,
            properties,
            path.clone(),
            project_owned_value(&path, value),
        )?;
    }
    Ok(())
}

fn project_owned_value(path: &PropertyPath, value: &serde_json::Value) -> OwnedValue {
    if value.is_null() {
        OwnedValue::Null
    } else if path.is_sensitive() {
        unreachable!("ManagedInputs rejects non-null sensitive values")
    } else {
        OwnedValue::Value(
            ComparableValue::try_from_json(value.clone())
                .expect("non-null state values are comparable"),
        )
    }
}

fn insert_projected(
    address: &ResourceAddress,
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    path: PropertyPath,
    value: OwnedValue,
) -> Result<(), StoredStateError> {
    if properties.insert(path, value).is_some() {
        return Err(StoredStateError::ConflictingPropertyPaths {
            address: address.clone(),
        });
    }
    Ok(())
}

/// A closed reason a remote property value cannot be compared safely.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertyUnknownReason {
    /// The property is write-only or sensitive.
    Sensitive,
    /// Dokploy did not return the property in a conclusive read.
    NotReturned,
    /// The property failed its owned remote contract.
    InvalidResponse,
}

/// A fresh observation for one property of a present remote resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PropertyObservation {
    /// The current remote property value is known.
    Known(ComparableValue),
    /// The property is conclusively absent remotely.
    KnownAbsent,
    /// The property cannot be compared safely.
    Unknown(PropertyUnknownReason),
}

/// A freshly observed remote resource and its per-property observations.
pub struct RemoteResource {
    remote_id: RemoteId,
    properties: BTreeMap<PropertyPath, PropertyObservation>,
}

impl RemoteResource {
    /// Creates one present remote observation.
    #[must_use]
    pub fn new(
        remote_id: RemoteId,
        properties: BTreeMap<PropertyPath, PropertyObservation>,
    ) -> Self {
        Self {
            remote_id,
            properties,
        }
    }

    /// Returns the opaque physical identifier without exposing it in plans.
    #[must_use]
    pub const fn remote_id(&self) -> &RemoteId {
        &self.remote_id
    }

    /// Returns one typed property observation.
    #[must_use]
    pub fn property(&self, path: &PropertyPath) -> Option<&PropertyObservation> {
        self.properties.get(path)
    }
}

impl fmt::Debug for RemoteResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteResource")
            .field("remote_id", &"[REDACTED]")
            .field("property_count", &self.properties.len())
            .finish()
    }
}

/// A closed, redaction-safe reason a required remote resource read could not complete.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteFailureKind {
    /// The Dokploy instance could not be reached.
    Unavailable,
    /// Dokploy rejected the supplied credentials.
    Unauthorized,
    /// The response did not satisfy the owned read contract.
    InvalidResponse,
}

/// The fresh observation for one relevant logical address.
#[derive(Debug)]
pub enum RemoteObservation {
    /// Dokploy conclusively reported that the resource does not exist.
    Missing,
    /// The resource exists and its safe comparable properties were read.
    Present(RemoteResource),
    /// The resource could not be observed conclusively.
    Unavailable(RemoteFailureKind),
}

/// How one desired external selector resolved against a fresh minimal collection.
///
/// Servers, registries, and backup destinations are external infrastructure.
/// Configuration stores only a stable selector; this value carries the freshly
/// resolved physical identity for execution and keyed saved-plan binding. The
/// identity is never serialized and never appears in debug output.
#[derive(Clone, Eq, PartialEq)]
pub enum ExternalResolution {
    /// The explicit local server selector, which has no physical identity.
    Local,
    /// Exactly one external record matched the exact name.
    Resolved(RemoteId),
    /// No external record matched the exact name.
    Unmatched,
    /// More than one external record matched the exact name.
    Ambiguous,
    /// The authoritative collection could not be read conclusively.
    Unavailable(RemoteFailureKind),
}

impl ExternalResolution {
    /// Returns whether the selector resolved to a usable identity.
    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        matches!(self, Self::Local | Self::Resolved(_))
    }

    /// Returns the resolved physical identity, which is absent for `local`.
    #[must_use]
    pub const fn remote_id(&self) -> Option<&RemoteId> {
        match self {
            Self::Resolved(remote_id) => Some(remote_id),
            Self::Local | Self::Unmatched | Self::Ambiguous | Self::Unavailable(_) => None,
        }
    }
}

impl fmt::Debug for ExternalResolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => formatter.write_str("Local"),
            Self::Resolved(_) => formatter.write_str("Resolved([REDACTED])"),
            Self::Unmatched => formatter.write_str("Unmatched"),
            Self::Ambiguous => formatter.write_str("Ambiguous"),
            Self::Unavailable(reason) => {
                formatter.debug_tuple("Unavailable").field(reason).finish()
            }
        }
    }
}

/// How a fresh remote observation compares with one exact durable checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceObservationMatch {
    /// Identity and every owned property are readable and equal.
    Exact,
    /// Identity and every readable owned property are equal, but sensitive values are write-only.
    ExactExceptSensitive,
    /// The resource is absent, has a different identity, or has a different owned value.
    Different,
    /// A required non-sensitive observation is unavailable or incomplete.
    Unavailable,
}

/// Compares one fresh observation with the exact resource recorded in durable state.
///
/// The result never exposes an owned value or sensitive receipt. Recovery uses
/// this seam to distinguish a confirmed checkpoint from an outcome that still
/// requires an operator decision.
pub fn compare_resource_observation(
    state: &StateFile,
    address: &ResourceAddress,
    observation: &RemoteObservation,
) -> Result<ResourceObservationMatch, StoredStateError> {
    let stored = StoredState::try_from_state(state)?;
    let Some(expected) = stored.resources.get(address) else {
        return Ok(ResourceObservationMatch::Different);
    };
    let RemoteObservation::Present(remote) = observation else {
        return Ok(match observation {
            RemoteObservation::Missing => ResourceObservationMatch::Different,
            RemoteObservation::Unavailable(_) => ResourceObservationMatch::Unavailable,
            RemoteObservation::Present(_) => unreachable!("present observation was matched"),
        });
    };
    if expected.remote_id != *remote.remote_id() {
        return Ok(ResourceObservationMatch::Different);
    }

    let mut sensitive_unverifiable = false;
    for (path, expected_value) in &expected.properties {
        let matches = match (expected_value, remote.property(path)) {
            (
                OwnedValue::Sensitive(_),
                Some(PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)),
            ) => {
                sensitive_unverifiable = true;
                true
            }
            (OwnedValue::Null, Some(PropertyObservation::KnownAbsent)) => true,
            (OwnedValue::EmptyCollection, Some(PropertyObservation::Known(value))) => value
                .as_json()
                .as_object()
                .is_some_and(serde_json::Map::is_empty),
            (OwnedValue::Value(expected), Some(PropertyObservation::Known(actual))) => {
                expected == actual
            }
            (_, Some(PropertyObservation::Unknown(_)) | None) => {
                return Ok(ResourceObservationMatch::Unavailable);
            }
            _ => false,
        };
        if !matches {
            return Ok(ResourceObservationMatch::Different);
        }
    }

    Ok(if sensitive_unverifiable {
        ResourceObservationMatch::ExactExceptSensitive
    } else {
        ResourceObservationMatch::Exact
    })
}

/// A complete set of fresh observations for the requested reconciliation.
pub struct RemoteState {
    pub(crate) instance: InstanceIdentity,
    observations: BTreeMap<ResourceAddress, RemoteObservation>,
    mutation_contracts: BTreeMap<ResourceAddress, crate::MutationContract>,
    external: BTreeMap<(ResourceAddress, PropertyPath), ExternalResolution>,
}

impl RemoteState {
    /// Builds a remote snapshot while rejecting duplicate logical and physical identities.
    pub fn try_new(
        instance: InstanceIdentity,
        observations: impl IntoIterator<Item = (ResourceAddress, RemoteObservation)>,
    ) -> Result<Self, RemoteStateError> {
        let observations = observations.into_iter().collect::<Vec<_>>();
        let contracts: Vec<_> = observations
            .iter()
            .map(|(address, _)| (address.clone(), crate::MutationContract::permissive()))
            .collect();
        Self::try_new_with_contracts(instance, observations, contracts)
    }

    /// Builds a remote snapshot with an explicit adapter-projected mutation contract.
    pub fn try_new_with_contracts(
        instance: InstanceIdentity,
        observations: impl IntoIterator<Item = (ResourceAddress, RemoteObservation)>,
        mutation_contracts: impl IntoIterator<Item = (ResourceAddress, crate::MutationContract)>,
    ) -> Result<Self, RemoteStateError> {
        let mut normalized = BTreeMap::new();
        let mut physical_identities = BTreeMap::new();
        for (address, observation) in observations {
            if normalized.contains_key(&address) {
                return Err(RemoteStateError::DuplicateAddress { address });
            }

            if let RemoteObservation::Present(resource) = &observation {
                validate_remote_resource(&address, resource)?;
                let identity = (address.kind(), resource.remote_id().clone());
                if let Some(first) = physical_identities.insert(identity, address.clone()) {
                    return Err(RemoteStateError::DuplicateRemoteIdentity {
                        first,
                        second: address,
                    });
                }
            }

            normalized.insert(address, observation);
        }

        let mutation_contracts = mutation_contracts.into_iter().collect::<BTreeMap<_, _>>();
        if mutation_contracts.keys().ne(normalized.keys()) {
            return Err(RemoteStateError::MutationContractMismatch);
        }

        Ok(Self {
            instance,
            observations: normalized,
            mutation_contracts,
            external: BTreeMap::new(),
        })
    }

    /// Attaches the fresh external selector resolutions used by desired resources.
    ///
    /// Each key names one desired selector property of an observed resource.
    /// Resolved identities stay in this non-serializable snapshot, take part in
    /// the keyed binding receipt, and never enter plans or durable state.
    pub fn with_external_resolutions(
        mut self,
        resolutions: impl IntoIterator<Item = ((ResourceAddress, PropertyPath), ExternalResolution)>,
    ) -> Result<Self, RemoteStateError> {
        let mut external = BTreeMap::new();
        for ((address, path), resolution) in resolutions {
            let valid = path.is_external_selector()
                && path.valid_for_kind(address.kind())
                && self.observations.contains_key(&address)
                && (path == PropertyPath::Server || resolution != ExternalResolution::Local);
            if !valid {
                return Err(RemoteStateError::InvalidExternalResolution { address });
            }
            if external
                .insert((address.clone(), path), resolution)
                .is_some()
            {
                return Err(RemoteStateError::InvalidExternalResolution { address });
            }
        }
        self.external = external;

        Ok(self)
    }

    /// Returns the fresh resolution of one desired external selector property.
    #[must_use]
    pub fn external_resolution(
        &self,
        address: &ResourceAddress,
        path: &PropertyPath,
    ) -> Option<&ExternalResolution> {
        self.external.get(&(address.clone(), path.clone()))
    }

    /// Returns the Dokploy instance that was observed.
    #[must_use]
    pub const fn instance(&self) -> &InstanceIdentity {
        &self.instance
    }

    /// Returns one observation, or `None` when the address was not observed.
    #[must_use]
    pub fn observation(&self, address: &ResourceAddress) -> Option<&RemoteObservation> {
        self.observations.get(address)
    }

    /// Binds every fresh identity and property observation into one keyed receipt.
    ///
    /// The returned bytes are safe to persist in a saved-plan envelope; remote
    /// identifiers and low-entropy property values never leave this method.
    #[must_use]
    pub fn binding_receipt(&self, key: &[u8]) -> [u8; 32] {
        const DOMAIN: &[u8] = b"dokploy-iac\0remote-state\0hmac-sha256-v1";

        let mut mac =
            Hmac::<Sha256>::new_from_slice(key).expect("HMAC-SHA-256 accepts keys of any length");
        receipt_field(&mut mac, b"domain", DOMAIN);
        receipt_field(&mut mac, b"instance", self.instance.as_str().as_bytes());
        receipt_u64(&mut mac, self.observations.len());
        for (address, observation) in &self.observations {
            receipt_field(&mut mac, b"address", address.to_string().as_bytes());
            match observation {
                RemoteObservation::Missing => receipt_field(&mut mac, b"status", b"missing"),
                RemoteObservation::Unavailable(reason) => {
                    receipt_field(&mut mac, b"status", b"unavailable");
                    receipt_field(
                        &mut mac,
                        b"reason",
                        match reason {
                            RemoteFailureKind::Unavailable => b"unavailable",
                            RemoteFailureKind::Unauthorized => b"unauthorized",
                            RemoteFailureKind::InvalidResponse => b"invalid_response",
                        },
                    );
                }
                RemoteObservation::Present(resource) => {
                    receipt_field(&mut mac, b"status", b"present");
                    receipt_field(
                        &mut mac,
                        b"remote_id",
                        resource.remote_id.as_str().as_bytes(),
                    );
                    receipt_u64(&mut mac, resource.properties.len());
                    for (path, property) in &resource.properties {
                        receipt_field(&mut mac, b"property", path.to_string().as_bytes());
                        match property {
                            PropertyObservation::Known(value) => {
                                receipt_field(&mut mac, b"state", b"known");
                                let encoded = serde_json::to_vec(value.as_json())
                                    .expect("comparable JSON always serializes");
                                receipt_field(&mut mac, b"value", &encoded);
                            }
                            PropertyObservation::KnownAbsent => {
                                receipt_field(&mut mac, b"state", b"absent");
                            }
                            PropertyObservation::Unknown(reason) => {
                                receipt_field(&mut mac, b"state", b"unknown");
                                receipt_field(
                                    &mut mac,
                                    b"reason",
                                    match reason {
                                        PropertyUnknownReason::Sensitive => b"sensitive",
                                        PropertyUnknownReason::NotReturned => b"not_returned",
                                        PropertyUnknownReason::InvalidResponse => {
                                            b"invalid_response"
                                        }
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }

        if !self.external.is_empty() {
            receipt_field(&mut mac, b"external", b"selectors");
            receipt_u64(&mut mac, self.external.len());
            for ((address, path), resolution) in &self.external {
                receipt_field(&mut mac, b"address", address.to_string().as_bytes());
                receipt_field(&mut mac, b"selector", path.to_string().as_bytes());
                match resolution {
                    ExternalResolution::Local => receipt_field(&mut mac, b"resolution", b"local"),
                    ExternalResolution::Resolved(remote_id) => {
                        receipt_field(&mut mac, b"resolution", b"resolved");
                        receipt_field(&mut mac, b"external_id", remote_id.as_str().as_bytes());
                    }
                    ExternalResolution::Unmatched => {
                        receipt_field(&mut mac, b"resolution", b"unmatched");
                    }
                    ExternalResolution::Ambiguous => {
                        receipt_field(&mut mac, b"resolution", b"ambiguous");
                    }
                    ExternalResolution::Unavailable(reason) => {
                        receipt_field(&mut mac, b"resolution", b"unavailable");
                        receipt_field(
                            &mut mac,
                            b"reason",
                            match reason {
                                RemoteFailureKind::Unavailable => b"unavailable",
                                RemoteFailureKind::Unauthorized => b"unauthorized",
                                RemoteFailureKind::InvalidResponse => b"invalid_response",
                            },
                        );
                    }
                }
            }
        }

        mac.finalize().into_bytes().into()
    }

    pub(crate) fn mutation_contract(
        &self,
        address: &ResourceAddress,
    ) -> Option<&crate::MutationContract> {
        self.mutation_contracts.get(address)
    }
}

fn receipt_u64(mac: &mut Hmac<Sha256>, value: usize) {
    let value = u64::try_from(value).expect("in-memory collection lengths fit in u64");
    mac.update(&value.to_be_bytes());
}

fn receipt_field(mac: &mut Hmac<Sha256>, label: &[u8], value: &[u8]) {
    let label_length = u64::try_from(label.len()).expect("receipt labels fit in u64");
    let value_length = u64::try_from(value.len()).expect("memory slices fit in u64");
    mac.update(&label_length.to_be_bytes());
    mac.update(label);
    mac.update(&value_length.to_be_bytes());
    mac.update(value);
}

impl fmt::Debug for RemoteState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteState")
            .field("instance", &self.instance)
            .field("observation_count", &self.observations.len())
            .field("external_resolution_count", &self.external.len())
            .finish()
    }
}

/// A malformed collection of remote observations.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RemoteStateError {
    /// The same logical address was observed more than once.
    #[error("remote state contains duplicate observation for `{address}`")]
    DuplicateAddress { address: ResourceAddress },
    /// One physical resource of the same kind was assigned to multiple logical addresses.
    #[error("remote state maps one physical resource to both `{first}` and `{second}`")]
    DuplicateRemoteIdentity {
        /// The first logical address assigned to the physical resource.
        first: ResourceAddress,
        /// The second logical address assigned to the physical resource.
        second: ResourceAddress,
    },
    /// An observation path is invalid for its resource kind or is lifecycle-only.
    #[error("remote resource `{address}` contains an invalid property path")]
    InvalidPropertyPath { address: ResourceAddress },
    /// An observation violates the sensitivity or collection-root contract.
    #[error("remote resource `{address}` contains an invalid property observation")]
    InvalidPropertyObservation { address: ResourceAddress },
    /// A collection root and one of its child paths were both observed.
    #[error("remote resource `{address}` contains conflicting property paths")]
    ConflictingPropertyPaths { address: ResourceAddress },
    /// Observation and mutation-contract address sets differ.
    #[error("remote observations and mutation contracts cover different addresses")]
    MutationContractMismatch,
    /// An external selector resolution names an invalid or unobserved selector property.
    #[error("remote resource `{address}` contains an invalid external selector resolution")]
    InvalidExternalResolution { address: ResourceAddress },
}

fn validate_remote_resource(
    address: &ResourceAddress,
    resource: &RemoteResource,
) -> Result<(), RemoteStateError> {
    for (path, observation) in &resource.properties {
        if !path.valid_for_kind(address.kind()) || path.is_lifecycle_only() {
            return Err(RemoteStateError::InvalidPropertyPath {
                address: address.clone(),
            });
        }
        let valid = if path.is_sensitive() {
            matches!(
                observation,
                PropertyObservation::KnownAbsent | PropertyObservation::Unknown(_)
            )
        } else {
            match path {
                PropertyPath::Source => !matches!(
                    observation,
                    PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)
                ),
                PropertyPath::Environment => match observation {
                    PropertyObservation::Known(_) => true,
                    PropertyObservation::KnownAbsent => true,
                    PropertyObservation::Unknown(_) => true,
                },
                PropertyPath::PublishedPort | PropertyPath::TargetPort => match observation {
                    PropertyObservation::Known(value) => value
                        .as_json()
                        .as_u64()
                        .is_some_and(|number| (1..=u64::from(u16::MAX)).contains(&number)),
                    PropertyObservation::Unknown(reason) => {
                        *reason != PropertyUnknownReason::Sensitive
                    }
                    PropertyObservation::KnownAbsent => false,
                },
                PropertyPath::PublishMode => {
                    port_observation_string_valid(observation, &["ingress", "host"])
                }
                PropertyPath::Protocol => {
                    port_observation_string_valid(observation, &["tcp", "udp"])
                }
                PropertyPath::Regex | PropertyPath::Replacement => {
                    non_empty_observation_valid(observation)
                }
                PropertyPath::Permanent => match observation {
                    PropertyObservation::Known(value) => value.as_json().is_boolean(),
                    PropertyObservation::Unknown(reason) => {
                        *reason != PropertyUnknownReason::Sensitive
                    }
                    PropertyObservation::KnownAbsent => false,
                },
                PropertyPath::Target if address.kind() == ResourceKind::Schedule => {
                    match observation {
                        PropertyObservation::Known(value) => {
                            schedule_target_text_valid(value.as_json())
                        }
                        PropertyObservation::Unknown(reason) => {
                            *reason != PropertyUnknownReason::Sensitive
                        }
                        PropertyObservation::KnownAbsent => false,
                    }
                }
                PropertyPath::ScheduleName | PropertyPath::CronExpression => match observation {
                    PropertyObservation::Known(value) => schedule_text_valid(value.as_json()),
                    PropertyObservation::Unknown(reason) => {
                        *reason != PropertyUnknownReason::Sensitive
                    }
                    PropertyObservation::KnownAbsent => false,
                },
                PropertyPath::ServiceName | PropertyPath::Timezone => match observation {
                    PropertyObservation::Known(value) => schedule_text_valid(value.as_json()),
                    PropertyObservation::Unknown(reason) => {
                        *reason != PropertyUnknownReason::Sensitive
                    }
                    PropertyObservation::KnownAbsent => true,
                },
                PropertyPath::ShellType => {
                    port_observation_string_valid(observation, &["bash", "sh"])
                }
                // A Backup's flag is nullable remotely; a Schedule's never is.
                PropertyPath::Enabled => match observation {
                    PropertyObservation::Known(value) => value.as_json().is_boolean(),
                    PropertyObservation::Unknown(reason) => {
                        *reason != PropertyUnknownReason::Sensitive
                    }
                    PropertyObservation::KnownAbsent => address.kind() == ResourceKind::Backup,
                },
                PropertyPath::Target => match observation {
                    PropertyObservation::Known(value) if address.kind() == ResourceKind::Backup => {
                        backup_target_text_valid(value.as_json())
                    }
                    PropertyObservation::Known(value) => mount_target_text_valid(value.as_json()),
                    PropertyObservation::Unknown(reason) => {
                        *reason != PropertyUnknownReason::Sensitive
                    }
                    PropertyObservation::KnownAbsent => false,
                },
                PropertyPath::Username if address.kind() == ResourceKind::Security => {
                    non_empty_observation_valid(observation)
                }
                PropertyPath::MountType => {
                    port_observation_string_valid(observation, &["bind", "volume", "file"])
                }
                PropertyPath::MountPath => match observation {
                    PropertyObservation::Known(value) => mount_text_valid(value.as_json()),
                    PropertyObservation::Unknown(reason) => {
                        *reason != PropertyUnknownReason::Sensitive
                    }
                    PropertyObservation::KnownAbsent => false,
                },
                PropertyPath::HostPath | PropertyPath::VolumeName | PropertyPath::FilePath => {
                    match observation {
                        PropertyObservation::Known(value) => mount_text_valid(value.as_json()),
                        PropertyObservation::Unknown(reason) => {
                            *reason != PropertyUnknownReason::Sensitive
                        }
                        PropertyObservation::KnownAbsent => true,
                    }
                }
                PropertyPath::Server
                | PropertyPath::BuildServer
                | PropertyPath::Registry
                | PropertyPath::BuildRegistry
                | PropertyPath::RollbackRegistry
                | PropertyPath::Destination => selector_observation_valid(path, observation),
                PropertyPath::Schedule | PropertyPath::Prefix => {
                    non_empty_observation_valid(observation)
                }
                PropertyPath::Database if address.kind() == ResourceKind::Backup => {
                    non_empty_observation_valid(observation)
                }
                PropertyPath::IncludeEncryptionKey => match observation {
                    PropertyObservation::Known(value) => value.as_json().is_boolean(),
                    PropertyObservation::KnownAbsent => false,
                    PropertyObservation::Unknown(reason) => {
                        *reason != PropertyUnknownReason::Sensitive
                    }
                },
                PropertyPath::KeepLatest => match observation {
                    PropertyObservation::Known(value) => keep_latest_json_valid(value.as_json()),
                    PropertyObservation::KnownAbsent => true,
                    PropertyObservation::Unknown(reason) => {
                        *reason != PropertyUnknownReason::Sensitive
                    }
                },
                _ => !matches!(
                    observation,
                    PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)
                ),
            }
        };
        if !valid {
            return Err(RemoteStateError::InvalidPropertyObservation {
                address: address.clone(),
            });
        }
    }
    validate_root_child_combinations(address, &resource.properties).map_err(|()| {
        RemoteStateError::ConflictingPropertyPaths {
            address: address.clone(),
        }
    })?;
    if !remote_source_shape_valid(&resource.properties) {
        return Err(RemoteStateError::InvalidPropertyObservation {
            address: address.clone(),
        });
    }
    Ok(())
}

fn non_empty_observation_valid(observation: &PropertyObservation) -> bool {
    match observation {
        PropertyObservation::Known(value) => value
            .as_json()
            .as_str()
            .is_some_and(|candidate| !candidate.is_empty()),
        PropertyObservation::Unknown(reason) => *reason != PropertyUnknownReason::Sensitive,
        PropertyObservation::KnownAbsent => false,
    }
}

fn port_observation_string_valid(observation: &PropertyObservation, allowed: &[&str]) -> bool {
    match observation {
        PropertyObservation::Known(value) => value
            .as_json()
            .as_str()
            .is_some_and(|candidate| allowed.contains(&candidate)),
        PropertyObservation::Unknown(reason) => *reason != PropertyUnknownReason::Sensitive,
        PropertyObservation::KnownAbsent => false,
    }
}

fn remote_source_shape_valid(properties: &BTreeMap<PropertyPath, PropertyObservation>) -> bool {
    let repository = properties.get(&PropertyPath::SourceRepository);
    let branch = properties.get(&PropertyPath::SourceBranch);
    if branch.is_some() && repository.is_none() {
        return false;
    }
    !matches!(
        (repository, branch),
        (
            Some(PropertyObservation::KnownAbsent),
            Some(PropertyObservation::Known(_))
        )
    )
}
