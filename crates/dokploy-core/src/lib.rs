//! Pure, deterministic planning for Dokploy infrastructure.
//!
//! This crate deliberately has no SDK, transport, runtime, or CLI dependency.
//! Callers supply three in-memory snapshots and receive a plan; planning cannot
//! read or mutate remote infrastructure.

mod dependency;
mod mutation;
mod plan;
mod planner;
mod property;
mod snapshot;
mod spec_property;

pub use mutation::{MutationContract, MutationMode, PropertyMutation, ReplacementOrder};
pub use plan::{
    ChangeKind, ChangeOrigin, CheckpointMaterializationError, CheckpointTarget, CheckpointValueRef,
    DriftChange, DriftKind, ExternalSelectorFailure, FieldChange, MetadataChangeKind, MoveAction,
    Plan, PlanDiagnostic, PlanDiagnosticCode, PlannedChange, ResourceCheckpoint,
    UnsupportedDirectiveKind, ValueState,
};
pub use planner::plan;
pub use property::{
    ComparableValue, ComparableValueError, EnvironmentVariableName, EnvironmentVariableNameError,
    OwnedValue, PropertyPath, PropertyPathError, SensitiveIntent, SpecPath,
};
pub use snapshot::{
    ConfigDigest, ConfigDigestError, DesiredResource, DesiredState, DesiredStateError,
    ExternalResolution, MoveDirective, PropertyObservation, PropertyUnknownReason,
    ProtectionIntent, RemoteFailureKind, RemoteObservation, RemoteResource, RemoteState,
    RemoteStateError, RemovalDirective, ResourceObservationMatch, StoredState, StoredStateError,
    compare_resource_observation, compare_resource_observation_with_specs,
};
pub use spec_property::register_spec_kinds;
