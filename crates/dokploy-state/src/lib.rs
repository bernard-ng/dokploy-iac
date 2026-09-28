//! Durable domain types for Dokploy infrastructure state.

mod resource;
mod state;

pub use resource::{
    ResourceAddress, ResourceAddressParseError, ResourceKind, ResourceKindParseError, ResourceName,
    ResourceNameError,
};
pub use state::{
    InstanceIdentity, InstanceIdentityError, ManagedInputs, ManagedInputsError, RemoteId,
    RemoteIdError, ResourceState, StateError, StateFile, StateRevision,
};
