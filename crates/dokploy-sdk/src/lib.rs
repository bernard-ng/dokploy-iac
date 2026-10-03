//! The Dokploy HTTP client: a connection, the generic [`Transport`] the engine sends through,
//! and the raw `api` commands. It knows no kind: what a resource looks like is the spec's
//! business (ADR 0002, ADR 0007).

mod client;
mod error;
mod imperative;
mod transport;

pub use client::{Dokploy, DokployBuilder, MAX_JSON_RESPONSE_BYTES};
pub use error::{BuildError, DokployError, Error};
pub use imperative::{Imperative, ImperativeMethod, ImperativeRequest};
pub use transport::{OperationRequest, Transport};
