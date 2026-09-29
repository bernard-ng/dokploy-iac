//! Pure, deterministic planning for Dokploy infrastructure.
//!
//! This crate deliberately has no SDK, transport, runtime, or CLI dependency.
//! Callers supply three in-memory snapshots and receive a plan; planning cannot
//! read or mutate remote infrastructure.

mod dependency;
mod plan;
mod planner;
mod property;
mod snapshot;

pub use plan::{
    ChangeKind, ChangeOrigin, CheckpointTarget, CheckpointValueRef, DriftChange, DriftKind,
    FieldChange, MetadataChangeKind, Plan, PlanDiagnostic, PlanDiagnosticCode, PlannedChange,
    ResourceCheckpoint, UnsupportedDirectiveKind, ValueState,
};
pub use planner::plan;
pub use property::{
    ComparableValue, ComparableValueError, EnvironmentVariableName, EnvironmentVariableNameError,
    OwnedValue, PropertyPath, PropertyPathError,
};
pub use snapshot::{
    ConfigDigest, ConfigDigestError, DesiredResource, DesiredState, DesiredStateError,
    MoveDirective, PropertyObservation, PropertyUnknownReason, ProtectionIntent, RemoteFailureKind,
    RemoteObservation, RemoteResource, RemoteState, RemoteStateError, RemovalDirective,
    StoredState, StoredStateError,
};
