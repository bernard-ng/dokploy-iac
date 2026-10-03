//! Generated low-level Dokploy request and endpoint bindings.
//!
//! This crate is an internal implementation detail of `dokploy-sdk`. Its
//! generated response types must not be used as reconciliation read models.

mod endpoint;

#[allow(clippy::all)]
#[doc(hidden)]
pub mod generated;

pub use endpoint::{
    BodyShape, Endpoint, EndpointMethod, GeneratedRequest, RequestContract, RequestField,
    validate_request,
};
pub use generated::*;
