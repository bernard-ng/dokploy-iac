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
    ApplicationCollection, ApplicationDetails, ApplicationEnvironmentDocument,
    ApplicationEnvironmentShape, ApplicationId, ApplicationSearchItem, ApplicationSummary,
    ChangeMariaDbPassword, ChangeMongoPassword, ChangeMySqlPassword, CreateApplication,
    CreateDomain, CreateEnvironment, CreateMariaDb, CreateMongo, CreateMySql, CreatePostgres,
    CreateProject, CreateRedis, CreatedApplication, CreatedDomain, CreatedEnvironment,
    CreatedMariaDb, CreatedMongo, CreatedMySql, CreatedPostgres, CreatedProject, CreatedRedis,
    DomainCollection, DomainDetails, DomainId, EnvironmentCollection, EnvironmentDetails,
    EnvironmentId, EnvironmentSummary, EnvironmentTopology, MariaDbCollection, MariaDbDetails,
    MariaDbId, MariaDbSearchItem, MongoCollection, MongoDetails, MongoId, MongoSearchItem,
    MySqlCollection, MySqlDetails, MySqlId, MySqlSearchItem, Nullable, PostgresCollection,
    PostgresDetails, PostgresId, PostgresSearchItem, PostgresSummary, ProjectDetails, ProjectId,
    ProjectTopology, RedisCollection, RedisDetails, RedisId, RedisSearchItem, RedisSummary,
    ResponseField, ServerId, UpdateApplication, UpdateDomain, UpdateEnvironment, UpdateMariaDb,
    UpdateMongo, UpdateMySql, UpdatePostgres, UpdateProject, UpdateRedis,
};
