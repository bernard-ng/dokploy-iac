//! Durable domain types for Dokploy infrastructure state.

mod journal;
mod resource;
mod sensitive;
mod state;
mod storage;
mod strict_json;

pub use journal::{
    ExpectedCheckpoint, ExpectedCheckpointError, FailureCode, JournalAction,
    JournalDurabilityStage, JournalError, OperationJournal, PlanDigest, PlanDigestError,
    RecoveryError, RecoveryReason, RecoverySession, RecoveryStatus, RecoveryStep,
    RecoveryStepOutcome, RecoverySummary, StepToken,
};
pub use resource::{
    ResourceAddress, ResourceAddressParseError, ResourceKind, ResourceKindParseError, ResourceName,
    ResourceNameError, StateScope,
};
pub use sensitive::{
    FingerprintKeyId, FingerprintKeyIdError, SensitiveFingerprint, SensitiveInputs,
    SensitiveInputsError, SensitivePropertyPath, SensitivePropertyPathError,
};
pub use state::{
    InstanceIdentity, InstanceIdentityError, ManagedInputs, ManagedInputsError, RemoteId,
    RemoteIdError, ResourceState, ResourceStateError, StateDecodeError, StateError, StateFile,
    StateRevision,
};
pub use storage::{
    DurabilityStage, ExpectedState, PersistenceTarget, StateStore, StateStoreError, WriteSession,
};
