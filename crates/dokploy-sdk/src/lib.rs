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
    ApplicationSearchItem, ApplicationSummary, CreateApplication, CreateDomain, CreateEnvironment,
    CreatePostgres, CreateProject, CreateRedis, CreatedApplication, CreatedDomain,
    CreatedEnvironment, CreatedPostgres, CreatedProject, CreatedRedis, DomainCollection,
    DomainDetails, DomainId, EnvironmentCollection, EnvironmentDetails, EnvironmentId,
    EnvironmentSummary, EnvironmentTopology, PostgresCollection, PostgresDetails, PostgresId,
    PostgresSearchItem, PostgresSummary, ProjectDetails, ProjectId, ProjectTopology,
    RedisCollection, RedisDetails, RedisId, RedisSearchItem, RedisSummary, ResponseField, ServerId,
};
