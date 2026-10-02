//! Stable, handwritten models and services for the Dokploy API.

mod client;
mod error;
mod imperative;
mod models;
mod services;

pub use client::{Dokploy, DokployBuilder, MAX_JSON_RESPONSE_BYTES};
pub use error::{BuildError, DokployError, Error};
pub use imperative::{Imperative, ImperativeMethod, ImperativeRequest};
pub use models::{
    ApplicationCollection, ApplicationDetails, ApplicationEnvironmentDocument,
    ApplicationEnvironmentShape, ApplicationId, ApplicationSearchItem, ApplicationSummary,
    BackupCollection, BackupDetails, BackupId, BackupTarget, ChangeLibSqlPassword,
    ChangeMariaDbPassword, ChangeMongoPassword, ChangeMySqlPassword, ComposeCollection,
    ComposeDetails, ComposeId, ComposeScheduleCollection, ComposeSearchItem, ComposeType,
    ComposeVolumePolicy, CreateApplication, CreateBackup, CreateCompose, CreateDomain,
    CreateEnvironment, CreateLibSql, CreateMariaDb, CreateMongo, CreateMount, CreateMySql,
    CreatePort, CreatePostgres, CreateProject, CreateRedirect, CreateRedis, CreateSchedule,
    CreateSecurity, CreateTag, CreatedApplication, CreatedBackup, CreatedCompose, CreatedDomain,
    CreatedEnvironment, CreatedLibSql, CreatedMariaDb, CreatedMongo, CreatedMount, CreatedMySql,
    CreatedPort, CreatedPostgres, CreatedProject, CreatedRedirect, CreatedRedis, CreatedSchedule,
    CreatedSecurity, CreatedTag, DestinationCollection, DestinationId, DestinationSummary,
    DomainCollection, DomainDetails, DomainId, EnvironmentCollection, EnvironmentDetails,
    EnvironmentId, EnvironmentSummary, EnvironmentTopology, LibSqlCollection, LibSqlDetails,
    LibSqlId, LibSqlNode, LibSqlSearchItem, LibSqlTopologySummary, MariaDbCollection,
    MariaDbDetails, MariaDbId, MariaDbSearchItem, MongoCollection, MongoDetails, MongoId,
    MongoSearchItem, MountCollection, MountDetails, MountId, MountType, MySqlCollection,
    MySqlDetails, MySqlId, MySqlSearchItem, Nullable, PortCollection, PortDetails, PortId,
    PortProtocol, PostgresCollection, PostgresDetails, PostgresId, PostgresSearchItem,
    PostgresSummary, ProjectDetails, ProjectId, ProjectTag, ProjectTopology, PublishMode,
    RedirectCollection, RedirectDetails, RedirectId, RedisCollection, RedisDetails, RedisId,
    RedisSearchItem, RedisSummary, RegistryCollection, RegistryId, RegistrySummary, ResponseField,
    ScheduleCollection, ScheduleDetails, ScheduleId, ScheduleTarget, SecurityCollection,
    SecurityDetails, SecurityId, ServerCollection, ServerId, ServerPlacement, ServerSummary,
    ServiceTarget, ShellType, TagCollection, TagDetails, TagId, UpdateApplication, UpdateBackup,
    UpdateCompose, UpdateDomain, UpdateEnvironment, UpdateLibSql, UpdateMariaDb, UpdateMongo,
    UpdateMount, UpdateMySql, UpdatePort, UpdatePostgres, UpdateProject, UpdateRedirect,
    UpdateRedis, UpdateSchedule, UpdateSecurity, UpdateTag,
};
