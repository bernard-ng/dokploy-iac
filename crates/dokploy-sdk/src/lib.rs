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
    ChangeLibSqlPassword, ChangeMariaDbPassword, ChangeMongoPassword, ChangeMySqlPassword,
    ComposeCollection, ComposeDetails, ComposeId, ComposeSearchItem, ComposeType,
    ComposeVolumePolicy, CreateApplication, CreateCompose, CreateDomain, CreateEnvironment,
    CreateLibSql, CreateMariaDb, CreateMongo, CreateMount, CreateMySql, CreatePort, CreatePostgres,
    CreateProject, CreateRedirect, CreateRedis, CreateSecurity, CreatedApplication, CreatedCompose,
    CreatedDomain, CreatedEnvironment, CreatedLibSql, CreatedMariaDb, CreatedMongo, CreatedMount,
    CreatedMySql, CreatedPort, CreatedPostgres, CreatedProject, CreatedRedirect, CreatedRedis,
    CreatedSecurity, DomainCollection, DomainDetails, DomainId, EnvironmentCollection,
    EnvironmentDetails, EnvironmentId, EnvironmentSummary, EnvironmentTopology, LibSqlCollection,
    LibSqlDetails, LibSqlId, LibSqlNode, LibSqlSearchItem, MariaDbCollection, MariaDbDetails,
    MariaDbId, MariaDbSearchItem, MongoCollection, MongoDetails, MongoId, MongoSearchItem,
    MountCollection, MountDetails, MountId, MountType, MySqlCollection, MySqlDetails, MySqlId,
    MySqlSearchItem, Nullable, PortCollection, PortDetails, PortId, PortProtocol,
    PostgresCollection, PostgresDetails, PostgresId, PostgresSearchItem, PostgresSummary,
    ProjectDetails, ProjectId, ProjectTopology, PublishMode, RedirectCollection, RedirectDetails,
    RedirectId, RedisCollection, RedisDetails, RedisId, RedisSearchItem, RedisSummary,
    ResponseField, SecurityCollection, SecurityDetails, SecurityId, ServerId, ServiceTarget,
    UpdateApplication, UpdateCompose, UpdateDomain, UpdateEnvironment, UpdateLibSql, UpdateMariaDb,
    UpdateMongo, UpdateMount, UpdateMySql, UpdatePort, UpdatePostgres, UpdateProject,
    UpdateRedirect, UpdateRedis, UpdateSecurity,
};
