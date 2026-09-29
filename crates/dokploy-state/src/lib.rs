//! Durable domain types for Dokploy infrastructure state.

mod journal;
mod resource;
mod state;
mod storage;
mod strict_json;

pub use journal::{
    FailureCode, JournalAction, JournalDurabilityStage, JournalError, OperationJournal, PlanDigest,
    PlanDigestError, RecoveryReason, RecoveryStatus, RecoveryStep, RecoverySummary, StepToken,
};
pub use resource::{
    ResourceAddress, ResourceAddressParseError, ResourceKind, ResourceKindParseError, ResourceName,
    ResourceNameError,
};
pub use state::{
    InstanceIdentity, InstanceIdentityError, ManagedInputs, ManagedInputsError, RemoteId,
    RemoteIdError, ResourceState, StateDecodeError, StateError, StateFile, StateRevision,
};
pub use storage::{
    DurabilityStage, ExpectedState, PersistenceTarget, StateStore, StateStoreError, WriteSession,
};
