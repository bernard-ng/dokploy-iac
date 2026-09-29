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
    ApplicationCollection, ApplicationDetails, ApplicationEnvironmentShape, ApplicationId,
    ApplicationSearchItem, ApplicationSummary, EnvironmentCollection, EnvironmentDetails,
    EnvironmentId, EnvironmentSummary, EnvironmentTopology, PostgresCollection, PostgresDetails,
    PostgresId, PostgresSearchItem, PostgresSummary, ProjectDetails, ProjectId, ProjectTopology,
    ResponseField, ServerId,
};
