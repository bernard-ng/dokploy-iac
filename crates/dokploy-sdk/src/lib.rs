//! Stable, handwritten models and services for the Dokploy API.

mod client;
mod error;
mod imperative;
mod models;
mod services;

pub use client::{Dokploy, DokployBuilder};
pub use error::{BuildError, DokployError, Error};
pub use imperative::{Imperative, ImperativeMethod, ImperativeRequest};
pub use models::{
    ApplicationDetails, ApplicationId, ApplicationSummary, EnvironmentId, EnvironmentTopology,
    PostgresDetails, PostgresId, PostgresSummary, ProjectDetails, ProjectId, ProjectTopology,
    ResponseField, ServerId,
};
