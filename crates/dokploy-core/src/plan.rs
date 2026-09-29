use std::{collections::BTreeMap, fmt};

use dokploy_state::ResourceAddress;
use serde::Serialize;
use uuid::Uuid;

use crate::{ConfigDigest, OwnedValue, PropertyPath, PropertyUnknownReason, RemoteFailureKind};

pub(crate) const PLAN_FORMAT_VERSION: u32 = 1;

/// The remote convergence action selected for one managed resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Create a resource that is absent remotely.
    Create,
    /// Update a present resource in place.
    Update,
    /// Replace a resource in a later planner slice.
    Replace,
    /// Delete a resource still present remotely.
    Delete,
    /// Preserve physical identity under a new logical address.
    Move,
    /// Relinquish management while retaining a present object, or forget an absent one.
    Forget,
    /// Perform no remote mutation while allowing a state checkpoint.
    NoOp,
}

/// Why a planned convergence action exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOrigin {
    /// Declarative configuration or ownership changed relative to stored state.
    Config,
    /// Remote infrastructure drifted relative to stored state.
    Drift,
    /// Both configuration and remote infrastructure changed.
    ConfigAndDrift,
}

/// A redaction-safe property state used in structured plan metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueState {
    /// The property is not owned by this configuration.
    Unmanaged,
    /// The property is explicitly owned as null or absent.
    Null,
    /// The property has an opaque, non-null value.
    Present,
    /// The property has value-free sensitive ownership intent.
    Sensitive,
    /// The property was not observed conclusively.
    Unknown,
}

/// A value-free three-way property change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub(crate) key: PropertyPath,
    pub(crate) stored: ValueState,
    pub(crate) desired: ValueState,
    pub(crate) remote: ValueState,
    pub(crate) origin: ChangeOrigin,
}

impl FieldChange {
    /// Returns the typed property path.
    #[must_use]
    pub const fn key(&self) -> &PropertyPath {
        &self.key
    }

    /// Returns the previous owned-state shape.
    #[must_use]
    pub const fn stored(&self) -> ValueState {
        self.stored
    }

    /// Returns the desired ownership shape.
    #[must_use]
    pub const fn desired(&self) -> ValueState {
        self.desired
    }

    /// Returns the observed remote shape.
    #[must_use]
    pub const fn remote(&self) -> ValueState {
        self.remote
    }

    /// Returns the origin of this property change.
    #[must_use]
    pub const fn origin(&self) -> ChangeOrigin {
        self.origin
    }
}

/// A state-only resource metadata checkpoint.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataChangeKind {
    /// Durable protection changed.
    Protection,
    /// Canonical resource dependencies changed.
    Dependencies,
}

/// Remote work coupled to a logical-address move.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveAction {
    /// Only the durable logical address changes.
    StateOnly,
    /// The logical address and remote managed properties both change.
    Update,
}

/// One deterministic resource-level plan entry.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedChange {
    address: ResourceAddress,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_address: Option<ResourceAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    move_action: Option<MoveAction>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    preserved_paths: Vec<PropertyPath>,
    kind: ChangeKind,
    origin: ChangeOrigin,
    fields: Vec<FieldChange>,
    metadata: Vec<MetadataChangeKind>,
    #[serde(skip)]
    checkpoint: CheckpointTarget,
}

impl fmt::Debug for PlannedChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlannedChange")
            .field("address", &self.address)
            .field("previous_address", &self.previous_address)
            .field("move_action", &self.move_action)
            .field("preserved_paths", &self.preserved_paths)
            .field("kind", &self.kind)
            .field("origin", &self.origin)
            .field("fields", &self.fields)
            .field("metadata", &self.metadata)
            .field("checkpoint", &"[REDACTED]")
            .finish()
    }
}

impl PlannedChange {
    pub(crate) fn resource(
        address: ResourceAddress,
        kind: ChangeKind,
        origin: ChangeOrigin,
        fields: Vec<FieldChange>,
        metadata: Vec<MetadataChangeKind>,
        checkpoint: CheckpointTarget,
    ) -> Self {
        assert_ne!(kind, ChangeKind::Move, "move changes require move_change");
        assert!(
            !matches!(checkpoint, CheckpointTarget::Move { .. }),
            "ordinary changes cannot carry move checkpoints"
        );
        Self {
            address,
            previous_address: None,
            move_action: None,
            preserved_paths: Vec::new(),
            kind,
            origin,
            fields,
            metadata,
            checkpoint,
        }
    }

    pub(crate) fn move_change(
        from: ResourceAddress,
        to: ResourceAddress,
        action: MoveAction,
        origin: ChangeOrigin,
        fields: Vec<FieldChange>,
        metadata: Vec<MetadataChangeKind>,
        target: ResourceCheckpoint,
    ) -> Self {
        Self {
            address: to,
            previous_address: Some(from.clone()),
            move_action: Some(action),
            preserved_paths: Vec::new(),
            kind: ChangeKind::Move,
            origin,
            fields,
            metadata,
            checkpoint: CheckpointTarget::Move { from, target },
        }
    }

    /// Returns the logical resource address.
    #[must_use]
    pub const fn address(&self) -> &ResourceAddress {
        &self.address
    }

    /// Returns the prior logical address for a move.
    #[must_use]
    pub const fn previous_address(&self) -> Option<&ResourceAddress> {
        self.previous_address.as_ref()
    }

    /// Returns the remote work coupled to a move, when this is a move entry.
    #[must_use]
    pub const fn move_action(&self) -> Option<MoveAction> {
        self.move_action
    }

    /// Returns paths that a future mutation must preserve rather than write.
    #[must_use]
    pub fn preserved_paths(&self) -> &[PropertyPath] {
        &self.preserved_paths
    }

    pub(crate) fn preserving(mut self, preserved_paths: Vec<PropertyPath>) -> Self {
        assert!(
            matches!(
                self.kind,
                ChangeKind::Update | ChangeKind::NoOp | ChangeKind::Move
            ),
            "only existing-resource actions can preserve ignored paths"
        );
        self.preserved_paths = preserved_paths;
        self
    }

    /// Returns the selected convergence action.
    #[must_use]
    pub const fn kind(&self) -> ChangeKind {
        self.kind
    }

    /// Returns why the action is present.
    #[must_use]
    pub const fn origin(&self) -> ChangeOrigin {
        self.origin
    }

    /// Returns value-free property changes in path order.
    #[must_use]
    pub fn fields(&self) -> &[FieldChange] {
        &self.fields
    }

    /// Returns state-only metadata changes in stable order.
    #[must_use]
    pub fn metadata(&self) -> &[MetadataChangeKind] {
        &self.metadata
    }

    /// Returns the immutable state checkpoint selected during planning.
    #[must_use]
    pub const fn checkpoint(&self) -> &CheckpointTarget {
        &self.checkpoint
    }
}

/// The exact durable state outcome selected for a planned change.
#[derive(Clone, Eq, PartialEq)]
pub enum CheckpointTarget {
    /// The logical resource will no longer exist in managed state.
    Absent,
    /// The logical resource remains managed with this validated state.
    Present(ResourceCheckpoint),
    /// Atomically remove the source address and checkpoint the target address.
    Move {
        /// The managed address that must be removed.
        from: ResourceAddress,
        /// The exact target resource state.
        target: ResourceCheckpoint,
    },
}

impl CheckpointTarget {
    /// Returns whether the change removes the resource from managed state.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Returns the present-resource target, when the resource remains managed.
    #[must_use]
    pub const fn present(&self) -> Option<&ResourceCheckpoint> {
        match self {
            Self::Present(target) => Some(target),
            Self::Absent | Self::Move { .. } => None,
        }
    }

    /// Returns the exact target state of an atomic move checkpoint.
    #[must_use]
    pub const fn move_target(&self) -> Option<&ResourceCheckpoint> {
        match self {
            Self::Move { target, .. } => Some(target),
            Self::Absent | Self::Present(_) => None,
        }
    }

    /// Returns the source address removed by an atomic move checkpoint.
    #[must_use]
    pub const fn move_from(&self) -> Option<&ResourceAddress> {
        match self {
            Self::Move { from, .. } => Some(from),
            Self::Absent | Self::Present(_) => None,
        }
    }
}

impl fmt::Debug for CheckpointTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => formatter.write_str("Absent"),
            Self::Present(_) => formatter.write_str("Present([REDACTED])"),
            Self::Move { from, .. } => formatter
                .debug_struct("Move")
                .field("from", from)
                .field("target", &"[REDACTED]")
                .finish(),
        }
    }
}

/// A validated target for the next durable state checkpoint.
#[derive(Clone, Eq, PartialEq)]
pub struct ResourceCheckpoint {
    pub(crate) protected: bool,
    pub(crate) dependencies: Vec<ResourceAddress>,
    pub(crate) properties: BTreeMap<PropertyPath, OwnedValue>,
}

impl ResourceCheckpoint {
    /// Returns the effective durable protection value.
    #[must_use]
    pub const fn protected(&self) -> bool {
        self.protected
    }

    /// Returns canonical dependencies.
    #[must_use]
    pub fn dependencies(&self) -> &[ResourceAddress] {
        &self.dependencies
    }

    /// Returns owned paths in canonical order without exposing values.
    #[must_use]
    pub fn property_paths(&self) -> Vec<&PropertyPath> {
        self.properties.keys().collect()
    }

    /// Returns one validated checkpoint value through a sensitivity-aware view.
    #[must_use]
    pub fn property(&self, path: &PropertyPath) -> Option<CheckpointValueRef<'_>> {
        self.properties.get(path).map(|value| match value {
            OwnedValue::Null => CheckpointValueRef::Null,
            OwnedValue::EmptyCollection => CheckpointValueRef::EmptyCollection,
            OwnedValue::Value(value) => CheckpointValueRef::NonSensitive(value.as_json()),
            OwnedValue::Sensitive => CheckpointValueRef::Sensitive,
        })
    }
}

impl fmt::Debug for ResourceCheckpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceCheckpoint")
            .field("protected", &self.protected)
            .field("dependency_count", &self.dependencies.len())
            .field("property_count", &self.properties.len())
            .finish()
    }
}

/// A safe borrowed view of one exact checkpoint property value.
pub enum CheckpointValueRef<'a> {
    /// Explicit clear intent.
    Null,
    /// An explicitly owned empty environment collection.
    EmptyCollection,
    /// An exact non-sensitive value needed by a future executor.
    NonSensitive(&'a serde_json::Value),
    /// Value-free sensitive ownership intent.
    Sensitive,
}

impl fmt::Debug for CheckpointValueRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("Null"),
            Self::EmptyCollection => formatter.write_str("EmptyCollection"),
            Self::NonSensitive(_) => formatter.write_str("NonSensitive([REDACTED])"),
            Self::Sensitive => formatter.write_str("Sensitive([REDACTED])"),
        }
    }
}

/// The externally observed drift category for a managed resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    /// Comparable managed properties changed outside this tool.
    Updated,
    /// A previously managed resource disappeared remotely.
    Deleted,
}

/// One deterministic resource-level drift record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftChange {
    pub(crate) address: ResourceAddress,
    pub(crate) kind: DriftKind,
    pub(crate) properties: Vec<PropertyPath>,
}

impl DriftChange {
    /// Returns the drifted logical resource address.
    #[must_use]
    pub const fn address(&self) -> &ResourceAddress {
        &self.address
    }

    /// Returns how the remote resource differs from stored state.
    #[must_use]
    pub const fn kind(&self) -> DriftKind {
        self.kind
    }

    /// Returns drifted owned property paths in stable order.
    #[must_use]
    pub fn properties(&self) -> &[PropertyPath] {
        &self.properties
    }
}

/// Deferred syntax understood by snapshots but not this planner checkpoint.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedDirectiveKind {
    /// Per-property replacement rule.
    Replacement,
}

/// A stable planner diagnostic category.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PlanDiagnosticCode {
    /// Stored and remote snapshots refer to different Dokploy instances.
    InstanceMismatch,
    /// A required logical address has no remote observation.
    MissingObservation,
    /// A required remote resource read failed.
    RemoteUnavailable,
    /// The observed physical resource differs from the managed identity.
    RemoteIdentityMismatch,
    /// A desired but unmanaged address already exists remotely.
    UnmanagedAddressCollision,
    /// Durable protection forbids deletion.
    ProtectedDelete,
    /// A declared feature is intentionally unsupported by this checkpoint.
    UnsupportedDirective,
    /// A desired property has no remote observation.
    MissingPropertyObservation,
    /// A desired property observation is unknown.
    UnknownPropertyObservation,
    /// Desired dependencies contain a cycle.
    DesiredDependencyCycle,
    /// Stored dependencies among removal actions contain a cycle.
    StoredDependencyCycle,
    /// A move declaration conflicts with the desired or stored snapshots.
    InvalidMoveDirective,
    /// A removal declaration conflicts with the desired or stored snapshots.
    InvalidRemovalDirective,
    /// A move source is not conclusively available under managed identity.
    MoveSourceMissing,
    /// A move target is already occupied in stored or remote state.
    MoveTargetCollision,
    /// Applying ignore ownership would create an invalid durable property shape.
    InvalidIgnoredCheckpoint,
}

impl PlanDiagnosticCode {
    /// Returns the stable machine-readable code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InstanceMismatch => "DOKPLAN001",
            Self::MissingObservation => "DOKPLAN002",
            Self::RemoteUnavailable => "DOKPLAN003",
            Self::RemoteIdentityMismatch => "DOKPLAN004",
            Self::UnmanagedAddressCollision => "DOKPLAN005",
            Self::ProtectedDelete => "DOKPLAN006",
            Self::UnsupportedDirective => "DOKPLAN007",
            Self::MissingPropertyObservation => "DOKPLAN008",
            Self::UnknownPropertyObservation => "DOKPLAN009",
            Self::DesiredDependencyCycle => "DOKPLAN010",
            Self::StoredDependencyCycle => "DOKPLAN011",
            Self::InvalidMoveDirective => "DOKPLAN012",
            Self::InvalidRemovalDirective => "DOKPLAN013",
            Self::MoveSourceMissing => "DOKPLAN014",
            Self::MoveTargetCollision => "DOKPLAN015",
            Self::InvalidIgnoredCheckpoint => "DOKPLAN016",
        }
    }
}

/// A redaction-safe problem that blocks or makes a plan incomplete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanDiagnostic {
    pub(crate) code: PlanDiagnosticCode,
    pub(crate) address: Option<ResourceAddress>,
    pub(crate) related_address: Option<ResourceAddress>,
    pub(crate) property: Option<PropertyPath>,
    pub(crate) remote_failure: Option<RemoteFailureKind>,
    pub(crate) property_unknown: Option<PropertyUnknownReason>,
    pub(crate) unsupported: Option<UnsupportedDirectiveKind>,
}

impl PlanDiagnostic {
    /// Returns the stable diagnostic category.
    #[must_use]
    pub const fn code(&self) -> PlanDiagnosticCode {
        self.code
    }

    /// Returns the affected logical address, when applicable.
    #[must_use]
    pub const fn address(&self) -> Option<&ResourceAddress> {
        self.address.as_ref()
    }

    /// Returns a second logical address for a move diagnostic.
    #[must_use]
    pub const fn related_address(&self) -> Option<&ResourceAddress> {
        self.related_address.as_ref()
    }

    /// Returns the affected property, when applicable.
    #[must_use]
    pub const fn property(&self) -> Option<&PropertyPath> {
        self.property.as_ref()
    }

    /// Returns the closed resource-read failure category, when applicable.
    #[must_use]
    pub const fn remote_failure(&self) -> Option<RemoteFailureKind> {
        self.remote_failure
    }

    /// Returns the closed property uncertainty category, when applicable.
    #[must_use]
    pub const fn property_unknown(&self) -> Option<PropertyUnknownReason> {
        self.property_unknown
    }

    /// Returns the deferred directive kind, when applicable.
    #[must_use]
    pub const fn unsupported(&self) -> Option<UnsupportedDirectiveKind> {
        self.unsupported
    }
}

/// A pure, deterministic reconciliation plan.
#[derive(Debug)]
pub struct Plan {
    pub(crate) format_version: u32,
    pub(crate) lineage: Uuid,
    pub(crate) state_serial: u64,
    pub(crate) config_digest: ConfigDigest,
    pub(crate) changes: Vec<PlannedChange>,
    pub(crate) drift: Vec<DriftChange>,
    pub(crate) complete: bool,
    pub(crate) applyable: bool,
    pub(crate) diagnostics: Vec<PlanDiagnostic>,
}

impl Plan {
    /// Returns the structured plan format version.
    #[must_use]
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Returns the durable state lineage used to create the plan.
    #[must_use]
    pub const fn lineage(&self) -> Uuid {
        self.lineage
    }

    /// Returns the durable state serial used to create the plan.
    #[must_use]
    pub const fn state_serial(&self) -> u64 {
        self.state_serial
    }

    /// Returns the configuration digest used to create the plan.
    #[must_use]
    pub const fn config_digest(&self) -> &ConfigDigest {
        &self.config_digest
    }

    /// Returns changes in deterministic dependency-safe execution order.
    #[must_use]
    pub fn changes(&self) -> &[PlannedChange] {
        &self.changes
    }

    /// Returns drift in deterministic logical-address order.
    #[must_use]
    pub fn drift(&self) -> &[DriftChange] {
        &self.drift
    }

    /// Returns whether every required remote resource and property was observed conclusively.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.complete
    }

    /// Returns whether the plan has no blocking diagnostics.
    #[must_use]
    pub const fn applyable(&self) -> bool {
        self.applyable
    }

    /// Returns diagnostics in stable code, address, and property-path order.
    #[must_use]
    pub fn diagnostics(&self) -> &[PlanDiagnostic] {
        &self.diagnostics
    }

    /// Serializes the redaction-safe plan document with stable field ordering.
    #[must_use]
    pub fn to_json_bytes(&self) -> Vec<u8> {
        let diagnostics: Vec<_> = self
            .diagnostics
            .iter()
            .map(|diagnostic| PlanDiagnosticDocument {
                code: diagnostic.code.as_str(),
                address: diagnostic.address.as_ref(),
                related_address: diagnostic.related_address.as_ref(),
                property: diagnostic.property.as_ref(),
                remote_failure: diagnostic.remote_failure,
                property_unknown: diagnostic.property_unknown,
                unsupported: diagnostic.unsupported,
            })
            .collect();
        let document = PlanDocument {
            format_version: self.format_version,
            lineage: self.lineage,
            state_serial: self.state_serial,
            config_digest: self.config_digest.as_str(),
            changes: &self.changes,
            drift: &self.drift,
            complete: self.complete,
            applyable: self.applyable,
            diagnostics,
        };

        serde_json::to_vec(&document)
            .expect("the closed redaction-safe plan document is always serializable")
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanDocument<'a> {
    format_version: u32,
    lineage: Uuid,
    state_serial: u64,
    config_digest: &'a str,
    changes: &'a [PlannedChange],
    drift: &'a [DriftChange],
    complete: bool,
    applyable: bool,
    diagnostics: Vec<PlanDiagnosticDocument<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanDiagnosticDocument<'a> {
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<&'a ResourceAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    related_address: Option<&'a ResourceAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    property: Option<&'a PropertyPath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_failure: Option<RemoteFailureKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    property_unknown: Option<PropertyUnknownReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unsupported: Option<UnsupportedDirectiveKind>,
}
