use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use dokploy_api::{
    APPLICATION_CREATE, APPLICATION_DELETE, APPLICATION_DEPLOY, APPLICATION_ONE,
    APPLICATION_SEARCH, APPLICATION_UPDATE, ApplicationCreateRequest, ApplicationCreateRequestBody,
    ApplicationDeleteRequest, ApplicationIdRequestBody, ApplicationOneRequest,
    ApplicationOneRequestQuery, ApplicationRedeployRequestBody, ApplicationSearchRequest,
    ApplicationSearchRequestQuery, BACKUP_CREATE, BACKUP_ONE, BACKUP_REMOVE, BACKUP_UPDATE,
    BackupIdRequestBody, BackupOneRequest, BackupOneRequestQuery, BackupRemoveRequest,
    COMPOSE_CREATE, COMPOSE_DELETE, COMPOSE_ONE, COMPOSE_SEARCH, COMPOSE_UPDATE,
    ComposeDeleteRequest, ComposeDeleteRequestBody, ComposeOneRequest, ComposeOneRequestQuery,
    ComposeSearchRequest, ComposeSearchRequestQuery, DESTINATION_ALL, DOMAIN_BY_APPLICATION_ID,
    DOMAIN_CREATE, DOMAIN_DELETE, DOMAIN_ONE, DOMAIN_UPDATE, DokployApiClient,
    DomainByApplicationIdRequest, DomainByApplicationIdRequestQuery, DomainCreateRequest,
    DomainCreateRequestBody, DomainDeleteRequest, DomainIdRequestBody, DomainOneRequest,
    DomainOneRequestQuery, ENVIRONMENT_BY_PROJECT_ID, ENVIRONMENT_CREATE, ENVIRONMENT_ONE,
    ENVIRONMENT_REMOVE, ENVIRONMENT_UPDATE, Endpoint, EndpointMethod,
    EnvironmentByProjectIdRequest, EnvironmentByProjectIdRequestQuery, EnvironmentCreateRequest,
    EnvironmentCreateRequestBody, EnvironmentIdRequestBody, EnvironmentOneRequest,
    EnvironmentOneRequestQuery, EnvironmentRemoveRequest, LIBSQL_CREATE, LIBSQL_ONE, LIBSQL_REMOVE,
    LIBSQL_UPDATE, LibsqlIdRequestBody, LibsqlOneRequest, LibsqlOneRequestQuery,
    LibsqlRemoveRequest, MARIADB_CHANGE_PASSWORD, MARIADB_CREATE, MARIADB_ONE, MARIADB_REMOVE,
    MARIADB_SEARCH, MARIADB_UPDATE, MONGO_CHANGE_PASSWORD, MONGO_CREATE, MONGO_ONE, MONGO_REMOVE,
    MONGO_SEARCH, MONGO_UPDATE, MOUNTS_CREATE, MOUNTS_LIST_BY_SERVICE_ID, MOUNTS_ONE,
    MOUNTS_REMOVE, MOUNTS_UPDATE, MYSQL_CHANGE_PASSWORD, MYSQL_CREATE, MYSQL_ONE, MYSQL_REMOVE,
    MYSQL_SEARCH, MYSQL_UPDATE, MariadbIdRequestBody, MariadbOneRequest, MariadbOneRequestQuery,
    MariadbRemoveRequest, MariadbSearchRequest, MariadbSearchRequestQuery, MongoIdRequestBody,
    MongoOneRequest, MongoOneRequestQuery, MongoRemoveRequest, MongoSearchRequest,
    MongoSearchRequestQuery, MountIdRequestBody, MountsListByServiceIdRequest,
    MountsListByServiceIdRequestQuery, MountsOneRequest, MountsOneRequestQuery,
    MountsRemoveRequest, MysqlIdRequestBody, MysqlOneRequest, MysqlOneRequestQuery,
    MysqlRemoveRequest, MysqlSearchRequest, MysqlSearchRequestQuery, PORT_CREATE, PORT_DELETE,
    PORT_ONE, PORT_UPDATE, POSTGRES_CREATE, POSTGRES_ONE, POSTGRES_REMOVE, POSTGRES_SEARCH,
    POSTGRES_UPDATE, PROJECT_ALL, PROJECT_CREATE, PROJECT_ONE, PROJECT_REMOVE, PROJECT_UPDATE,
    PortDeleteRequest, PortIdRequestBody, PortOneRequest, PortOneRequestQuery,
    PostgresIdRequestBody, PostgresOneRequest, PostgresOneRequestQuery, PostgresRemoveRequest,
    PostgresSearchRequest, PostgresSearchRequestQuery, ProjectAllRequest, ProjectCreateRequest,
    ProjectCreateRequestBody, ProjectIdRequestBody, ProjectOneRequest, ProjectOneRequestQuery,
    ProjectRemoveRequest, REDIRECTS_CREATE, REDIRECTS_DELETE, REDIRECTS_ONE, REDIRECTS_UPDATE,
    REDIS_CREATE, REDIS_ONE, REDIS_REMOVE, REDIS_SEARCH, REDIS_UPDATE, REGISTRY_ALL,
    RedirectIdRequestBody, RedirectsDeleteRequest, RedirectsOneRequest, RedirectsOneRequestQuery,
    RedisIdRequestBody, RedisOneRequest, RedisOneRequestQuery, RedisRemoveRequest,
    RedisSearchRequest, RedisSearchRequestQuery, SCHEDULE_CREATE, SCHEDULE_DELETE, SCHEDULE_LIST,
    SCHEDULE_ONE, SCHEDULE_UPDATE, SECURITY_CREATE, SECURITY_DELETE, SECURITY_ONE, SECURITY_UPDATE,
    SERVER_ALL, ScheduleCreateRequestBodyScheduleType, ScheduleDeleteRequest,
    ScheduleIdRequestBody, ScheduleListRequest, ScheduleListRequestQuery, ScheduleOneRequest,
    ScheduleOneRequestQuery, SecurityDeleteRequest, SecurityIdRequestBody, SecurityOneRequest,
    SecurityOneRequestQuery, endpoint_by_operation, validate_request,
};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

use crate::error::{BuildError, DokployError, Error};
use crate::imperative::{
    Imperative, ImperativeBody, ImperativeMethod, ImperativeRequest, MultipartField,
};
use crate::models::{
    ApplicationCollection, ApplicationCreateResponse, ApplicationDetails,
    ApplicationEnvironmentDocument, ApplicationEnvironmentResponse,
    ApplicationPortCollectionResponse, ApplicationRedirectCollectionResponse,
    ApplicationSearchPage, ApplicationSecurityCollectionResponse,
    ApplicationSecurityProofCollectionResponse, BackupCollection, BackupDetails, ComposeCollection,
    ComposeCreateResponse, ComposeDetails, ComposeSearchPage, DestinationCollection,
    DestinationSummary, DomainCollection, DomainCreateResponse, DomainDetails,
    EnvironmentCollection, EnvironmentCreateResponse, EnvironmentDetails,
    LibSqlBackupCollectionResponse, LibSqlCollection, LibSqlDetails,
    MariaDbBackupCollectionResponse, MariaDbCollection, MariaDbCreateResponse, MariaDbDetails,
    MariaDbSearchPage, MongoBackupCollectionResponse, MongoCollection, MongoCreateResponse,
    MongoDetails, MongoSearchPage, MountCollection, MountDetails, MySqlBackupCollectionResponse,
    MySqlCollection, MySqlCreateResponse, MySqlDetails, MySqlSearchPage, PortCollection,
    PortDetails, PostgresBackupCollectionResponse, PostgresCollection, PostgresCreateResponse,
    PostgresDetails, PostgresSearchPage, ProjectCreateResponse, ProjectDetails, ProjectTopology,
    RedirectCollection, RedirectDetails, RedisCollection, RedisCreateResponse, RedisDetails,
    RedisSearchPage, RegistryCollection, RegistrySummary, ScheduleCollection, ScheduleDetails,
    ScheduleProofDetails, SecurityCollection, SecurityDetails, SecurityProofDetails,
    ServerCollection, ServerSummary,
};
use crate::services::{
    Applications, Backups, Composes, Destinations, Domains, Environments, LibSql, MariaDb, Mongo,
    Mounts, MySql, Ports, Postgres, Projects, Redirects, Redis, Registries, Schedules, Security,
    Servers,
};
use crate::{
    ApplicationId, BackupId, BackupTarget, ChangeLibSqlPassword, ChangeMariaDbPassword,
    ChangeMongoPassword, ChangeMySqlPassword, ComposeId, ComposeScheduleCollection,
    ComposeVolumePolicy, CreateApplication, CreateBackup, CreateCompose, CreateDomain,
    CreateEnvironment, CreateLibSql, CreateMariaDb, CreateMongo, CreateMount, CreateMySql,
    CreatePort, CreatePostgres, CreateProject, CreateRedirect, CreateRedis, CreateSchedule,
    CreateSecurity, CreatedApplication, CreatedBackup, CreatedCompose, CreatedDomain,
    CreatedEnvironment, CreatedLibSql, CreatedMariaDb, CreatedMongo, CreatedMount, CreatedMySql,
    CreatedPort, CreatedPostgres, CreatedProject, CreatedRedirect, CreatedRedis, CreatedSchedule,
    CreatedSecurity, DomainId, EnvironmentId, LibSqlId, MariaDbId, MongoId, MountId, MySqlId,
    PortId, PostgresId, ProjectId, RedirectId, RedisId, ScheduleId, ScheduleTarget, SecurityId,
    ServiceTarget, UpdateApplication, UpdateBackup, UpdateCompose, UpdateDomain, UpdateEnvironment,
    UpdateLibSql, UpdateMariaDb, UpdateMongo, UpdateMount, UpdateMySql, UpdatePort, UpdatePostgres,
    UpdateProject, UpdateRedirect, UpdateRedis, UpdateSchedule, UpdateSecurity,
};

const API_KEY_HEADER: &str = "x-api-key";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("dokploy-iac/", env!("CARGO_PKG_VERSION"));
/// Maximum decoded JSON response body accepted by the SDK.
///
/// Sixteen MiB leaves headroom for large Dokploy topology and imperative
/// responses while bounding memory consumed at the untrusted HTTP seam.
pub const MAX_JSON_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const APPLICATION_SEARCH_PAGE_SIZE: usize = 100;
const APPLICATION_SEARCH_ITEM_LIMIT: usize = 10_000;
const COMPOSE_SEARCH_PAGE_SIZE: usize = 100;
const COMPOSE_SEARCH_ITEM_LIMIT: usize = 10_000;
const LIBSQL_TOPOLOGY_ITEM_LIMIT: usize = 10_000;
const MARIADB_SEARCH_PAGE_SIZE: usize = 100;
const MARIADB_SEARCH_ITEM_LIMIT: usize = 10_000;
const MONGO_SEARCH_PAGE_SIZE: usize = 100;
const MONGO_SEARCH_ITEM_LIMIT: usize = 10_000;
const MOUNT_LIST_ITEM_LIMIT: usize = 10_000;
const BACKUP_LIST_ITEM_LIMIT: usize = 10_000;
const PORT_LIST_ITEM_LIMIT: usize = 10_000;
const REDIRECT_LIST_ITEM_LIMIT: usize = 10_000;
const SCHEDULE_LIST_ITEM_LIMIT: usize = 10_000;
const SECURITY_LIST_ITEM_LIMIT: usize = 10_000;
const EXTERNAL_SELECTOR_ITEM_LIMIT: usize = 10_000;
const MYSQL_SEARCH_PAGE_SIZE: usize = 100;
const MYSQL_SEARCH_ITEM_LIMIT: usize = 10_000;
const POSTGRES_SEARCH_PAGE_SIZE: usize = 100;
const POSTGRES_SEARCH_ITEM_LIMIT: usize = 10_000;
const REDIS_SEARCH_PAGE_SIZE: usize = 100;
const REDIS_SEARCH_ITEM_LIMIT: usize = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ErrorBodyPolicy {
    Preserve,
    Sanitize,
}

fn error_body_policy(endpoint: Endpoint) -> ErrorBodyPolicy {
    // This is the complete preserve allowlist. Every omitted endpoint fails closed.
    if matches!(
        endpoint,
        PROJECT_CREATE
            | PROJECT_UPDATE
            | ENVIRONMENT_CREATE
            | ENVIRONMENT_UPDATE
            | APPLICATION_SEARCH
            | COMPOSE_SEARCH
            | DOMAIN_BY_APPLICATION_ID
            | DOMAIN_CREATE
            | DOMAIN_DELETE
            | DOMAIN_ONE
            | DOMAIN_UPDATE
            | PORT_CREATE
            | PORT_DELETE
            | PORT_ONE
            | PORT_UPDATE
            | REDIRECTS_CREATE
            | REDIRECTS_DELETE
            | REDIRECTS_ONE
            | REDIRECTS_UPDATE
            | POSTGRES_SEARCH
            | MYSQL_SEARCH
            | MYSQL_UPDATE
            | MARIADB_SEARCH
            | MARIADB_UPDATE
            | MONGO_SEARCH
            | MONGO_UPDATE
            | REDIS_SEARCH
    ) {
        ErrorBodyPolicy::Preserve
    } else {
        ErrorBodyPolicy::Sanitize
    }
}

/// A configured client for the Dokploy API.
#[derive(Clone)]
pub struct Dokploy {
    pub(crate) inner: Arc<ClientInner>,
}

pub(crate) struct ClientInner {
    api: DokployApiClient,
}

impl Dokploy {
    /// Starts configuration of a Dokploy client.
    #[must_use]
    pub fn builder() -> DokployBuilder {
        DokployBuilder::default()
    }

    /// Returns the normalized API base URL used for every request.
    ///
    /// The URL contains no credentials, query, or fragment. Callers can parse
    /// it into their own instance-identity type without coupling this SDK to
    /// that domain.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.inner.api.base_url
    }

    /// Returns access to project read operations.
    #[must_use]
    pub fn projects(&self) -> Projects<'_> {
        Projects::new(self)
    }

    /// Returns access to external server selector reads.
    #[must_use]
    pub fn servers(&self) -> Servers<'_> {
        Servers::new(self)
    }

    /// Returns access to external registry selector reads.
    #[must_use]
    pub fn registries(&self) -> Registries<'_> {
        Registries::new(self)
    }

    /// Returns access to external backup-destination selector reads.
    #[must_use]
    pub fn destinations(&self) -> Destinations<'_> {
        Destinations::new(self)
    }

    /// Returns access to database Backup read and mutation operations.
    #[must_use]
    pub fn backups(&self) -> Backups<'_> {
        Backups::new(self)
    }

    /// Returns access to application read operations.
    #[must_use]
    pub fn applications(&self) -> Applications<'_> {
        Applications::new(self)
    }

    /// Returns access to Compose read and mutation operations.
    #[must_use]
    pub fn composes(&self) -> Composes<'_> {
        Composes::new(self)
    }

    /// Returns access to environment read operations.
    #[must_use]
    pub fn environments(&self) -> Environments<'_> {
        Environments::new(self)
    }

    /// Returns access to LibSQL read and mutation operations.
    #[must_use]
    pub fn libsql(&self) -> LibSql<'_> {
        LibSql::new(self)
    }

    /// Returns access to MariaDB read and mutation operations.
    #[must_use]
    pub fn mariadb(&self) -> MariaDb<'_> {
        MariaDb::new(self)
    }

    /// Returns access to MongoDB read and mutation operations.
    #[must_use]
    pub fn mongo(&self) -> Mongo<'_> {
        Mongo::new(self)
    }

    /// Returns access to Mount read and mutation operations.
    #[must_use]
    pub fn mounts(&self) -> Mounts<'_> {
        Mounts::new(self)
    }

    /// Returns access to application Port read and mutation operations.
    #[must_use]
    pub fn ports(&self) -> Ports<'_> {
        Ports::new(self)
    }

    /// Returns access to application Redirect read and mutation operations.
    #[must_use]
    pub fn redirects(&self) -> Redirects<'_> {
        Redirects::new(self)
    }

    /// Returns access to application Security read and mutation operations.
    #[must_use]
    pub fn security(&self) -> Security<'_> {
        Security::new(self)
    }

    /// Returns access to application and Compose Schedule operations.
    #[must_use]
    pub fn schedules(&self) -> Schedules<'_> {
        Schedules::new(self)
    }

    /// Returns access to MySQL read and mutation operations.
    #[must_use]
    pub fn mysql(&self) -> MySql<'_> {
        MySql::new(self)
    }

    /// Returns access to Postgres read operations.
    #[must_use]
    pub fn postgres(&self) -> Postgres<'_> {
        Postgres::new(self)
    }

    /// Returns access to Redis read operations.
    #[must_use]
    pub fn redis(&self) -> Redis<'_> {
        Redis::new(self)
    }

    /// Returns access to domain read operations.
    #[must_use]
    pub fn domains(&self) -> Domains<'_> {
        Domains::new(self)
    }

    /// Returns broad raw access to operations from the pinned OpenAPI contract.
    #[must_use]
    pub fn imperative(&self) -> Imperative<'_> {
        Imperative::new(self)
    }

    pub(crate) async fn project_all(&self) -> Result<ProjectTopology, Error> {
        let request = ProjectAllRequest {};
        validate_generated_request(PROJECT_ALL, &request)?;

        self.read_json(PROJECT_ALL).await
    }

    pub(crate) async fn server_all(&self) -> Result<ServerCollection, Error> {
        let servers: Vec<ServerSummary> = self.read_json_secret(SERVER_ALL).await?;
        validate_external_selector_collection(
            SERVER_ALL,
            &servers,
            |server| server.server_id.as_str(),
            |server| !server.name.is_empty() && !server.server_type.is_empty(),
        )?;

        Ok(ServerCollection { servers })
    }

    pub(crate) async fn registry_all(&self) -> Result<RegistryCollection, Error> {
        let registries: Vec<RegistrySummary> = self.read_json_secret(REGISTRY_ALL).await?;
        validate_external_selector_collection(
            REGISTRY_ALL,
            &registries,
            |registry| registry.registry_id.as_str(),
            |registry| !registry.registry_name.is_empty(),
        )?;

        Ok(RegistryCollection { registries })
    }

    pub(crate) async fn destination_all(&self) -> Result<DestinationCollection, Error> {
        let destinations: Vec<DestinationSummary> = self.read_json_secret(DESTINATION_ALL).await?;
        validate_external_selector_collection(
            DESTINATION_ALL,
            &destinations,
            |destination| destination.destination_id.as_str(),
            |destination| !destination.name.is_empty(),
        )?;

        Ok(DestinationCollection { destinations })
    }

    pub(crate) async fn project_get(&self, project_id: &str) -> Result<ProjectDetails, Error> {
        let request = ProjectOneRequest {
            query: ProjectOneRequestQuery {
                project_id: project_id.to_owned(),
            },
        };
        validate_generated_request(PROJECT_ONE, &request)?;

        self.read_query_json(PROJECT_ONE, &request.query).await
    }

    pub(crate) async fn project_create(
        &self,
        input: CreateProject,
    ) -> Result<CreatedProject, Error> {
        let request = ProjectCreateRequest {
            body: ProjectCreateRequestBody {
                name: input.name,
                description: input.description,
                env: None,
            },
        };
        validate_generated_request(PROJECT_CREATE, &request)?;
        let response: ProjectCreateResponse =
            self.mutate_body_json(PROJECT_CREATE, &request.body).await?;

        Ok(CreatedProject::from_response(response))
    }

    pub(crate) async fn project_update(&self, input: UpdateProject) -> Result<(), Error> {
        self.mutate_body_ok(PROJECT_UPDATE, &input).await
    }

    pub(crate) async fn project_delete(&self, project_id: ProjectId) -> Result<(), Error> {
        let request = ProjectRemoveRequest {
            body: ProjectIdRequestBody {
                project_id: project_id.as_str().to_owned(),
            },
        };
        validate_generated_request(PROJECT_REMOVE, &request)?;

        self.mutate_body_ok(PROJECT_REMOVE, &request.body).await
    }

    pub(crate) async fn application_get(
        &self,
        application_id: &str,
    ) -> Result<ApplicationDetails, Error> {
        let request = ApplicationOneRequest {
            query: ApplicationOneRequestQuery {
                application_id: application_id.to_owned(),
            },
        };
        validate_generated_request(APPLICATION_ONE, &request)?;

        self.read_query_json(APPLICATION_ONE, &request.query).await
    }

    pub(crate) async fn application_create(
        &self,
        input: CreateApplication,
    ) -> Result<CreatedApplication, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                APPLICATION_CREATE.operation(),
                "application create fields are invalid",
            ));
        }
        let expected_server_placement = input.server_placement().cloned();
        let request = ApplicationCreateRequest {
            body: ApplicationCreateRequestBody {
                name: input.name.clone(),
                app_name: None,
                description: None,
                environment_id: input.environment_id.as_str().to_owned(),
                server_id: expected_server_placement.as_ref().and_then(
                    |placement| match placement {
                        crate::ServerPlacement::Local => None,
                        crate::ServerPlacement::Server(server_id) => {
                            Some(server_id.as_str().to_owned())
                        }
                    },
                ),
                source_type: None,
            },
        };
        validate_generated_request(APPLICATION_CREATE, &request)?;
        let response: ApplicationCreateResponse =
            self.mutate_body_json(APPLICATION_CREATE, &input).await?;
        if response.application_id.as_str().is_empty()
            || expected_server_placement
                .as_ref()
                .is_some_and(|placement| !placement.matches_response(&response.server_id))
        {
            return Err(post_mutation_proof_unknown(APPLICATION_CREATE));
        }

        Ok(CreatedApplication::from_response(response))
    }

    pub(crate) async fn application_update(&self, input: UpdateApplication) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                APPLICATION_UPDATE.operation(),
                "application update requires an identity and at least one field",
            ));
        }

        self.mutate_body_ok(APPLICATION_UPDATE, &input).await
    }

    pub(crate) async fn application_deploy(
        &self,
        application_id: ApplicationId,
    ) -> Result<(), Error> {
        let body = ApplicationRedeployRequestBody {
            application_id: application_id.as_str().to_owned(),
            title: None,
            description: None,
        };

        self.mutate_body_ok(APPLICATION_DEPLOY, &body).await
    }

    pub(crate) async fn application_delete(
        &self,
        application_id: ApplicationId,
    ) -> Result<(), Error> {
        let request = ApplicationDeleteRequest {
            body: ApplicationIdRequestBody {
                application_id: application_id.as_str().to_owned(),
            },
        };
        validate_generated_request(APPLICATION_DELETE, &request)?;

        self.mutate_body_ok(APPLICATION_DELETE, &request.body).await
    }

    pub(crate) async fn application_environment(
        &self,
        application_id: ApplicationId,
    ) -> Result<ApplicationEnvironmentDocument, Error> {
        let request = ApplicationOneRequest {
            query: ApplicationOneRequestQuery {
                application_id: application_id.as_str().to_owned(),
            },
        };
        validate_generated_request(APPLICATION_ONE, &request)?;
        let response: ApplicationEnvironmentResponse = self
            .read_query_json(APPLICATION_ONE, &request.query)
            .await?;

        Ok(ApplicationEnvironmentDocument::from_response(response))
    }

    pub(crate) async fn applications_by_environment(
        &self,
        environment_id: &str,
    ) -> Result<ApplicationCollection, Error> {
        if environment_id.is_empty() {
            return Err(invalid_request(
                APPLICATION_SEARCH.operation(),
                "environment ID cannot be empty",
            ));
        }
        let mut applications = Vec::new();
        let mut expected_total = None;

        loop {
            let request = ApplicationSearchRequest {
                query: ApplicationSearchRequestQuery {
                    environment_id: Some(environment_id.to_owned()),
                    limit: Some(APPLICATION_SEARCH_PAGE_SIZE as f64),
                    offset: Some(applications.len() as f64),
                    ..ApplicationSearchRequestQuery::default()
                },
            };
            validate_generated_request(APPLICATION_SEARCH, &request)?;
            let page: ApplicationSearchPage = self
                .read_query_json(APPLICATION_SEARCH, &request.query)
                .await?;

            let expected = *expected_total.get_or_insert(page.total);
            if page.total != expected
                || expected > APPLICATION_SEARCH_ITEM_LIMIT as u64
                || page.items.len() > APPLICATION_SEARCH_PAGE_SIZE
            {
                return Err(Error::UnexpectedResponse {
                    operation: APPLICATION_SEARCH.operation(),
                });
            }
            let expected = usize::try_from(expected).map_err(|_| Error::UnexpectedResponse {
                operation: APPLICATION_SEARCH.operation(),
            })?;
            let page_would_exceed_total = applications
                .len()
                .checked_add(page.items.len())
                .is_none_or(|count| count > expected);
            if page_would_exceed_total || (page.items.is_empty() && applications.len() < expected) {
                return Err(Error::UnexpectedResponse {
                    operation: APPLICATION_SEARCH.operation(),
                });
            }

            applications.extend(page.items);
            if applications.len() == expected {
                return Ok(ApplicationCollection { applications });
            }
        }
    }

    pub(crate) async fn compose_get(&self, compose_id: &str) -> Result<ComposeDetails, Error> {
        let request = ComposeOneRequest {
            query: ComposeOneRequestQuery {
                compose_id: compose_id.to_owned(),
            },
        };
        validate_generated_request(COMPOSE_ONE, &request)?;

        self.read_query_json(COMPOSE_ONE, &request.query).await
    }

    pub(crate) async fn compose_create(
        &self,
        input: CreateCompose,
    ) -> Result<CreatedCompose, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                COMPOSE_CREATE.operation(),
                "Compose create fields are invalid",
            ));
        }
        let expected_environment_id = input.environment_id().clone();
        let expected_name = input.name().to_owned();
        let expected_server_placement = input.server_placement().cloned();
        let response: ComposeCreateResponse = self.mutate_body_json(COMPOSE_CREATE, &input).await?;
        let server_matches = expected_server_placement
            .as_ref()
            .is_none_or(|placement| placement.matches_response(&response.server_id));
        if response.compose_id.as_str().is_empty()
            || response.environment_id != expected_environment_id
            || response.name != expected_name
            || !server_matches
        {
            return Err(post_mutation_proof_unknown(COMPOSE_CREATE));
        }

        Ok(CreatedCompose::new(response.compose_id))
    }

    pub(crate) async fn compose_update(&self, input: UpdateCompose) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                COMPOSE_UPDATE.operation(),
                "Compose update requires an identity and at least one valid field",
            ));
        }

        self.mutate_body_ok(COMPOSE_UPDATE, &input).await
    }

    pub(crate) async fn compose_delete(
        &self,
        compose_id: ComposeId,
        volume_policy: ComposeVolumePolicy,
    ) -> Result<(), Error> {
        let request = ComposeDeleteRequest {
            body: ComposeDeleteRequestBody {
                compose_id: compose_id.as_str().to_owned(),
                delete_volumes: volume_policy.delete_volumes(),
            },
        };
        validate_generated_request(COMPOSE_DELETE, &request)?;

        self.mutate_body_ok(COMPOSE_DELETE, &request.body).await
    }

    pub(crate) async fn composes_by_environment(
        &self,
        environment_id: &str,
    ) -> Result<ComposeCollection, Error> {
        if environment_id.is_empty() {
            return Err(invalid_request(
                COMPOSE_SEARCH.operation(),
                "environment ID cannot be empty",
            ));
        }
        let mut composes = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut expected_total = None;

        loop {
            let request = ComposeSearchRequest {
                query: ComposeSearchRequestQuery {
                    environment_id: Some(environment_id.to_owned()),
                    limit: Some(COMPOSE_SEARCH_PAGE_SIZE as f64),
                    offset: Some(composes.len() as f64),
                    ..ComposeSearchRequestQuery::default()
                },
            };
            validate_generated_request(COMPOSE_SEARCH, &request)?;
            let page: ComposeSearchPage =
                self.read_query_json(COMPOSE_SEARCH, &request.query).await?;

            let expected = *expected_total.get_or_insert(page.total);
            let has_conflicting_identity = page.items.iter().any(|item| {
                item.environment_id.as_str() != environment_id
                    || !seen_ids.insert(item.compose_id.as_str().to_owned())
            });
            if page.total != expected
                || expected > COMPOSE_SEARCH_ITEM_LIMIT as u64
                || page.items.len() > COMPOSE_SEARCH_PAGE_SIZE
                || has_conflicting_identity
            {
                return Err(Error::UnexpectedResponse {
                    operation: COMPOSE_SEARCH.operation(),
                });
            }
            let expected = usize::try_from(expected).map_err(|_| Error::UnexpectedResponse {
                operation: COMPOSE_SEARCH.operation(),
            })?;
            let page_would_exceed_total = composes
                .len()
                .checked_add(page.items.len())
                .is_none_or(|count| count > expected);
            if page_would_exceed_total || (page.items.is_empty() && composes.len() < expected) {
                return Err(Error::UnexpectedResponse {
                    operation: COMPOSE_SEARCH.operation(),
                });
            }

            composes.extend(page.items);
            if composes.len() == expected {
                return Ok(ComposeCollection { composes });
            }
        }
    }

    pub(crate) async fn mount_get(&self, mount_id: &str) -> Result<MountDetails, Error> {
        let request = MountsOneRequest {
            query: MountsOneRequestQuery {
                mount_id: mount_id.to_owned(),
            },
        };
        validate_generated_request(MOUNTS_ONE, &request)?;

        self.read_query_json(MOUNTS_ONE, &request.query).await
    }

    pub(crate) async fn mounts_by_target(
        &self,
        target: &ServiceTarget,
    ) -> Result<MountCollection, Error> {
        if target.service_id().is_empty() {
            return Err(invalid_request(
                MOUNTS_LIST_BY_SERVICE_ID.operation(),
                "Mount target identity cannot be empty",
            ));
        }
        let request = MountsListByServiceIdRequest {
            query: MountsListByServiceIdRequestQuery {
                service_type: target.service_type().to_owned(),
                service_id: target.service_id().to_owned(),
            },
        };
        validate_generated_request(MOUNTS_LIST_BY_SERVICE_ID, &request)?;
        let mounts: Vec<MountDetails> = self
            .read_query_json(MOUNTS_LIST_BY_SERVICE_ID, &request.query)
            .await?;
        let mut seen_ids = HashSet::new();
        let contradictory = mounts.len() > MOUNT_LIST_ITEM_LIMIT
            || mounts.iter().any(|mount| {
                mount.target != *target
                    || mount.mount_id.as_str().is_empty()
                    || !seen_ids.insert(mount.mount_id.as_str().to_owned())
            });
        if contradictory {
            return Err(Error::UnexpectedResponse {
                operation: MOUNTS_LIST_BY_SERVICE_ID.operation(),
            });
        }

        Ok(MountCollection { mounts })
    }

    pub(crate) async fn mount_create(&self, input: CreateMount) -> Result<CreatedMount, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MOUNTS_CREATE.operation(),
                "Mount create fields are invalid",
            ));
        }
        let response: MountDetails = self.mutate_body_json(MOUNTS_CREATE, &input).await?;
        if !input.matches(&response) {
            return Err(Error::UnexpectedResponse {
                operation: MOUNTS_CREATE.operation(),
            });
        }

        Ok(CreatedMount::new(response.mount_id))
    }

    pub(crate) async fn mount_update(&self, input: UpdateMount) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MOUNTS_UPDATE.operation(),
                "Mount update requires an identity and at least one valid field",
            ));
        }

        self.mutate_body_ok(MOUNTS_UPDATE, &input).await
    }

    pub(crate) async fn mount_delete(&self, mount_id: MountId) -> Result<(), Error> {
        let request = MountsRemoveRequest {
            body: MountIdRequestBody {
                mount_id: mount_id.as_str().to_owned(),
            },
        };
        validate_generated_request(MOUNTS_REMOVE, &request)?;

        self.mutate_body_ok(MOUNTS_REMOVE, &request.body).await
    }

    pub(crate) async fn port_get(&self, port_id: &str) -> Result<PortDetails, Error> {
        let request = PortOneRequest {
            query: PortOneRequestQuery {
                port_id: port_id.to_owned(),
            },
        };
        validate_generated_request(PORT_ONE, &request)?;
        let details: PortDetails = self.read_query_json(PORT_ONE, &request.query).await?;
        if !details.is_valid() || details.port_id.as_str() != port_id {
            return Err(Error::UnexpectedResponse {
                operation: PORT_ONE.operation(),
            });
        }

        Ok(details)
    }

    pub(crate) async fn ports_by_application(
        &self,
        application_id: &ApplicationId,
    ) -> Result<PortCollection, Error> {
        let request = ApplicationOneRequest {
            query: ApplicationOneRequestQuery {
                application_id: application_id.as_str().to_owned(),
            },
        };
        validate_generated_request(APPLICATION_ONE, &request)?;
        let response: ApplicationPortCollectionResponse = self
            .read_query_json(APPLICATION_ONE, &request.query)
            .await?;
        let mut seen_ids = HashSet::new();
        let contradictory = response.application_id != *application_id
            || response.ports.len() > PORT_LIST_ITEM_LIMIT
            || response.ports.iter().any(|port| {
                !port.is_valid()
                    || port.application_id != *application_id
                    || !seen_ids.insert(port.port_id.as_str().to_owned())
            });
        if contradictory {
            return Err(Error::UnexpectedResponse {
                operation: APPLICATION_ONE.operation(),
            });
        }

        Ok(PortCollection::new(response.application_id, response.ports))
    }

    pub(crate) async fn port_create(&self, input: CreatePort) -> Result<CreatedPort, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                PORT_CREATE.operation(),
                "Port create fields are invalid",
            ));
        }
        let response: PortDetails = self.mutate_body_json(PORT_CREATE, &input).await?;
        if !input.matches(&response) {
            return Err(Error::UnexpectedResponse {
                operation: PORT_CREATE.operation(),
            });
        }

        Ok(CreatedPort::new(response.port_id))
    }

    pub(crate) async fn port_update(&self, input: UpdatePort) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                PORT_UPDATE.operation(),
                "Port update fields are invalid",
            ));
        }

        self.mutate_body_ok(PORT_UPDATE, &input).await
    }

    pub(crate) async fn port_delete(&self, port_id: PortId) -> Result<(), Error> {
        let request = PortDeleteRequest {
            body: PortIdRequestBody {
                port_id: port_id.as_str().to_owned(),
            },
        };
        validate_generated_request(PORT_DELETE, &request)?;

        self.mutate_body_ok(PORT_DELETE, &request.body).await
    }

    pub(crate) async fn redirect_get(&self, redirect_id: &str) -> Result<RedirectDetails, Error> {
        self.redirect_get_with_collection(redirect_id)
            .await
            .map(|(details, _)| details)
    }

    async fn redirect_get_with_collection(
        &self,
        redirect_id: &str,
    ) -> Result<(RedirectDetails, RedirectCollection), Error> {
        let request = RedirectsOneRequest {
            query: RedirectsOneRequestQuery {
                redirect_id: redirect_id.to_owned(),
            },
        };
        validate_generated_request(REDIRECTS_ONE, &request)?;
        let details: RedirectDetails = self.read_query_json(REDIRECTS_ONE, &request.query).await?;
        if !details.is_valid() || details.redirect_id.as_str() != redirect_id {
            return Err(Error::UnexpectedResponse {
                operation: REDIRECTS_ONE.operation(),
            });
        }
        let collection = self
            .redirects_by_application(&details.application_id)
            .await?;
        let matching = collection
            .redirects()
            .iter()
            .find(|redirect| redirect.redirect_id.as_str() == redirect_id);
        if matching != Some(&details) {
            return Err(Error::UnexpectedResponse {
                operation: REDIRECTS_ONE.operation(),
            });
        }

        Ok((details, collection))
    }

    pub(crate) async fn redirects_by_application(
        &self,
        application_id: &ApplicationId,
    ) -> Result<RedirectCollection, Error> {
        let request = ApplicationOneRequest {
            query: ApplicationOneRequestQuery {
                application_id: application_id.as_str().to_owned(),
            },
        };
        validate_generated_request(APPLICATION_ONE, &request)?;
        let response: ApplicationRedirectCollectionResponse = self
            .read_query_json(APPLICATION_ONE, &request.query)
            .await?;
        let mut seen_ids = HashSet::new();
        let contradictory = response.application_id != *application_id
            || response.redirects.len() > REDIRECT_LIST_ITEM_LIMIT
            || response.redirects.iter().any(|redirect| {
                !redirect.is_valid()
                    || redirect.application_id != *application_id
                    || !seen_ids.insert(redirect.redirect_id.as_str().to_owned())
            });
        if contradictory {
            return Err(Error::UnexpectedResponse {
                operation: APPLICATION_ONE.operation(),
            });
        }

        Ok(RedirectCollection::new(
            response.application_id,
            response.redirects,
        ))
    }

    pub(crate) async fn redirect_create(
        &self,
        input: CreateRedirect,
    ) -> Result<CreatedRedirect, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                REDIRECTS_CREATE.operation(),
                "Redirect create fields are invalid",
            ));
        }
        let before = self
            .redirects_by_application(input.application_id())
            .await?;
        if before
            .redirects()
            .iter()
            .any(|redirect| redirect.regex == input.regex())
        {
            return Err(Error::UnexpectedResponse {
                operation: REDIRECTS_CREATE.operation(),
            });
        }
        let before_ids = before
            .redirects()
            .iter()
            .map(|redirect| redirect.redirect_id.as_str().to_owned())
            .collect::<HashSet<_>>();

        let accepted: bool = self.mutate_body_json(REDIRECTS_CREATE, &input).await?;
        if !accepted {
            return Err(Error::UnexpectedResponse {
                operation: REDIRECTS_CREATE.operation(),
            });
        }

        let after = self
            .redirects_by_application(input.application_id())
            .await
            .map_err(|_| post_mutation_proof_unknown(REDIRECTS_CREATE))?;
        let after_ids = after
            .redirects()
            .iter()
            .map(|redirect| redirect.redirect_id.as_str().to_owned())
            .collect::<HashSet<_>>();
        let new = after
            .redirects()
            .iter()
            .filter(|redirect| !before_ids.contains(redirect.redirect_id.as_str()))
            .collect::<Vec<_>>();
        if before_ids.is_subset(&after_ids)
            && let [created] = new.as_slice()
            && input.matches(created)
        {
            return Ok(CreatedRedirect::new(created.redirect_id.clone()));
        }

        Err(post_mutation_proof_unknown(REDIRECTS_CREATE))
    }

    pub(crate) async fn redirect_update(&self, input: UpdateRedirect) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                REDIRECTS_UPDATE.operation(),
                "Redirect update fields are invalid",
            ));
        }

        let (existing, before) = self
            .redirect_get_with_collection(input.redirect_id().as_str())
            .await?;
        if before.redirects().iter().any(|redirect| {
            redirect.redirect_id != *input.redirect_id() && redirect.regex == input.regex()
        }) {
            return Err(Error::UnexpectedResponse {
                operation: REDIRECTS_UPDATE.operation(),
            });
        }

        self.mutate_body_ok(REDIRECTS_UPDATE, &input).await?;

        let after = self
            .redirects_by_application(&existing.application_id)
            .await
            .map_err(|_| post_mutation_proof_unknown(REDIRECTS_UPDATE))?;
        let matching_regex = after
            .redirects()
            .iter()
            .filter(|redirect| redirect.regex == input.regex())
            .collect::<Vec<_>>();
        if !matches!(matching_regex.as_slice(), [updated] if input.matches(updated)) {
            return Err(post_mutation_proof_unknown(REDIRECTS_UPDATE));
        }

        Ok(())
    }

    pub(crate) async fn redirect_delete(&self, redirect_id: RedirectId) -> Result<(), Error> {
        let request = RedirectsDeleteRequest {
            body: RedirectIdRequestBody {
                redirect_id: redirect_id.as_str().to_owned(),
            },
        };
        validate_generated_request(REDIRECTS_DELETE, &request)?;
        let (existing, _) = self
            .redirect_get_with_collection(redirect_id.as_str())
            .await?;

        self.mutate_body_ok(REDIRECTS_DELETE, &request.body).await?;

        let after = self
            .redirects_by_application(&existing.application_id)
            .await
            .map_err(|_| post_mutation_proof_unknown(REDIRECTS_DELETE))?;
        if after
            .redirects()
            .iter()
            .any(|redirect| redirect.redirect_id == redirect_id)
        {
            return Err(post_mutation_proof_unknown(REDIRECTS_DELETE));
        }

        Ok(())
    }

    pub(crate) async fn security_get(&self, security_id: &str) -> Result<SecurityDetails, Error> {
        self.security_get_with_collection(security_id)
            .await
            .map(|(details, _)| details)
    }

    async fn security_get_with_collection(
        &self,
        security_id: &str,
    ) -> Result<(SecurityDetails, SecurityCollection), Error> {
        let request = SecurityOneRequest {
            query: SecurityOneRequestQuery {
                security_id: security_id.to_owned(),
            },
        };
        validate_generated_request(SECURITY_ONE, &request)?;
        let details: SecurityDetails = self
            .read_query_json_secret(SECURITY_ONE, &request.query)
            .await?;
        if !details.is_valid() || details.security_id.as_str() != security_id {
            return Err(Error::UnexpectedResponse {
                operation: SECURITY_ONE.operation(),
            });
        }
        let collection = self
            .security_by_application(&details.application_id)
            .await?;
        let matching = collection
            .entries()
            .iter()
            .find(|entry| entry.security_id.as_str() == security_id);
        if matching != Some(&details) {
            return Err(Error::UnexpectedResponse {
                operation: SECURITY_ONE.operation(),
            });
        }

        Ok((details, collection))
    }

    pub(crate) async fn security_by_application(
        &self,
        application_id: &ApplicationId,
    ) -> Result<SecurityCollection, Error> {
        let request = ApplicationOneRequest {
            query: ApplicationOneRequestQuery {
                application_id: application_id.as_str().to_owned(),
            },
        };
        validate_generated_request(APPLICATION_ONE, &request)?;
        let response: ApplicationSecurityCollectionResponse = self
            .read_query_json_secret(APPLICATION_ONE, &request.query)
            .await?;
        let mut seen_ids = HashSet::new();
        let mut seen_usernames = HashSet::new();
        let contradictory = response.application_id != *application_id
            || response.entries.len() > SECURITY_LIST_ITEM_LIMIT
            || response.entries.iter().any(|entry| {
                !entry.is_valid()
                    || entry.application_id != *application_id
                    || !seen_ids.insert(entry.security_id.as_str().to_owned())
                    || !seen_usernames.insert(entry.username.clone())
            });
        if contradictory {
            return Err(Error::UnexpectedResponse {
                operation: APPLICATION_ONE.operation(),
            });
        }

        Ok(SecurityCollection::new(
            response.application_id,
            response.entries,
        ))
    }

    pub(crate) async fn security_create(
        &self,
        input: CreateSecurity,
    ) -> Result<CreatedSecurity, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                SECURITY_CREATE.operation(),
                "Security create fields are invalid",
            ));
        }
        let before = self.security_by_application(input.application_id()).await?;
        if before
            .entries()
            .iter()
            .any(|entry| entry.username == input.username())
        {
            return Err(Error::UnexpectedResponse {
                operation: SECURITY_CREATE.operation(),
            });
        }
        let before_ids = before
            .entries()
            .iter()
            .map(|entry| entry.security_id.as_str().to_owned())
            .collect::<HashSet<_>>();

        let accepted: bool = self
            .mutate_body_json_secret(SECURITY_CREATE, &input)
            .await?;
        if !accepted {
            return Err(Error::UnexpectedResponse {
                operation: SECURITY_CREATE.operation(),
            });
        }

        let after = self
            .security_by_application(input.application_id())
            .await
            .map_err(|_| post_mutation_proof_unknown(SECURITY_CREATE))?;
        let after_ids = after
            .entries()
            .iter()
            .map(|entry| entry.security_id.as_str().to_owned())
            .collect::<HashSet<_>>();
        let new = after
            .entries()
            .iter()
            .filter(|entry| !before_ids.contains(entry.security_id.as_str()))
            .collect::<Vec<_>>();
        if before_ids.is_subset(&after_ids)
            && let [created] = new.as_slice()
            && input.matches(created)
        {
            return Ok(CreatedSecurity::new(created.security_id.clone()));
        }

        Err(post_mutation_proof_unknown(SECURITY_CREATE))
    }

    async fn security_proofs_by_application(
        &self,
        application_id: &ApplicationId,
    ) -> Result<Vec<SecurityProofDetails>, Error> {
        let request = ApplicationOneRequest {
            query: ApplicationOneRequestQuery {
                application_id: application_id.as_str().to_owned(),
            },
        };
        validate_generated_request(APPLICATION_ONE, &request)?;
        let response: ApplicationSecurityProofCollectionResponse = self
            .read_query_json_secret(APPLICATION_ONE, &request.query)
            .await?;
        let mut seen_ids = HashSet::new();
        let mut seen_usernames = HashSet::new();
        let contradictory = response.application_id != *application_id
            || response.entries.len() > SECURITY_LIST_ITEM_LIMIT
            || response.entries.iter().any(|proof| {
                let entry = proof.details();

                !entry.is_valid()
                    || entry.application_id != *application_id
                    || !seen_ids.insert(entry.security_id.as_str().to_owned())
                    || !seen_usernames.insert(entry.username.clone())
            });
        if contradictory {
            return Err(Error::UnexpectedResponse {
                operation: APPLICATION_ONE.operation(),
            });
        }

        Ok(response.entries)
    }

    pub(crate) async fn security_update(&self, input: UpdateSecurity) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                SECURITY_UPDATE.operation(),
                "Security update fields are invalid",
            ));
        }

        let (existing, before) = self
            .security_get_with_collection(input.security_id().as_str())
            .await?;
        if before.entries().iter().any(|entry| {
            entry.security_id != *input.security_id() && entry.username == input.username()
        }) {
            return Err(Error::UnexpectedResponse {
                operation: SECURITY_UPDATE.operation(),
            });
        }

        self.mutate_body_ok_secret(SECURITY_UPDATE, &input).await?;

        let after = self
            .security_proofs_by_application(&existing.application_id)
            .await
            .map_err(|_| post_mutation_proof_unknown(SECURITY_UPDATE))?;
        let updated = after
            .iter()
            .find(|proof| proof.details().security_id == *input.security_id());
        if !updated.is_some_and(|proof| input.matches(proof)) {
            return Err(post_mutation_proof_unknown(SECURITY_UPDATE));
        }

        Ok(())
    }

    pub(crate) async fn security_delete(&self, security_id: SecurityId) -> Result<(), Error> {
        let request = SecurityDeleteRequest {
            body: SecurityIdRequestBody {
                security_id: security_id.as_str().to_owned(),
            },
        };
        validate_generated_request(SECURITY_DELETE, &request)?;
        let (existing, _) = self
            .security_get_with_collection(security_id.as_str())
            .await?;

        self.mutate_body_ok(SECURITY_DELETE, &request.body).await?;

        let after = self
            .security_by_application(&existing.application_id)
            .await
            .map_err(|_| post_mutation_proof_unknown(SECURITY_DELETE))?;
        if after
            .entries()
            .iter()
            .any(|entry| entry.security_id == security_id)
        {
            return Err(post_mutation_proof_unknown(SECURITY_DELETE));
        }

        Ok(())
    }

    pub(crate) async fn schedule_get(&self, schedule_id: &str) -> Result<ScheduleDetails, Error> {
        self.schedule_get_with_collection(schedule_id)
            .await
            .map(|(details, _)| details)
    }

    async fn schedule_get_with_collection(
        &self,
        schedule_id: &str,
    ) -> Result<(ScheduleDetails, ScheduleCollection), Error> {
        let request = ScheduleOneRequest {
            query: ScheduleOneRequestQuery {
                schedule_id: schedule_id.to_owned(),
            },
        };
        validate_generated_request(SCHEDULE_ONE, &request)?;
        let details: ScheduleDetails = self
            .read_query_json_secret(SCHEDULE_ONE, &request.query)
            .await?;
        if !details.is_valid() || details.schedule_id.as_str() != schedule_id {
            return Err(Error::UnexpectedResponse {
                operation: SCHEDULE_ONE.operation(),
            });
        }
        let collection = self.schedules_by_target(&details.target).await?;
        let matching = collection
            .schedules()
            .iter()
            .find(|schedule| schedule.schedule_id.as_str() == schedule_id);
        if matching != Some(&details) {
            return Err(Error::UnexpectedResponse {
                operation: SCHEDULE_ONE.operation(),
            });
        }

        Ok((details, collection))
    }

    pub(crate) async fn schedules_by_target(
        &self,
        target: &ScheduleTarget,
    ) -> Result<ScheduleCollection, Error> {
        if !target.is_valid() {
            return Err(invalid_request(
                SCHEDULE_LIST.operation(),
                "Schedule target fields are invalid",
            ));
        }
        let schedule_type = match target {
            ScheduleTarget::Application(_) => ScheduleCreateRequestBodyScheduleType::Application,
            ScheduleTarget::Compose { .. } => ScheduleCreateRequestBodyScheduleType::Compose,
        };
        let schedules = self.schedule_list(target.id(), schedule_type).await?;
        let mut seen_names = HashSet::new();
        if schedules
            .iter()
            .any(|schedule| schedule.target != *target || !seen_names.insert(schedule.name.clone()))
        {
            return Err(Error::UnexpectedResponse {
                operation: SCHEDULE_LIST.operation(),
            });
        }

        Ok(ScheduleCollection::new(target.clone(), schedules))
    }

    /// Reads every Schedule of one Compose across all of its services.
    ///
    /// `schedule.list` takes only the Compose identity, so the response may span
    /// several service names. Names must be unique within each service.
    pub(crate) async fn schedules_by_compose(
        &self,
        compose_id: &ComposeId,
    ) -> Result<ComposeScheduleCollection, Error> {
        if compose_id.as_str().is_empty() {
            return Err(invalid_request(
                SCHEDULE_LIST.operation(),
                "Schedule target fields are invalid",
            ));
        }
        let schedules = self
            .schedule_list(
                compose_id.as_str(),
                ScheduleCreateRequestBodyScheduleType::Compose,
            )
            .await?;
        let mut seen_names = HashSet::new();
        let contradictory = schedules.iter().any(|schedule| match &schedule.target {
            ScheduleTarget::Compose {
                compose_id: owner,
                service_name,
            } => {
                owner != compose_id
                    || !seen_names.insert((service_name.clone(), schedule.name.clone()))
            }
            ScheduleTarget::Application(_) => true,
        });
        if contradictory {
            return Err(Error::UnexpectedResponse {
                operation: SCHEDULE_LIST.operation(),
            });
        }

        Ok(ComposeScheduleCollection::new(
            compose_id.clone(),
            schedules,
        ))
    }

    /// Reads one bounded `schedule.list` response with unique, valid identities.
    async fn schedule_list(
        &self,
        id: &str,
        schedule_type: ScheduleCreateRequestBodyScheduleType,
    ) -> Result<Vec<ScheduleDetails>, Error> {
        let request = ScheduleListRequest {
            query: ScheduleListRequestQuery {
                id: id.to_owned(),
                schedule_type,
            },
        };
        validate_generated_request(SCHEDULE_LIST, &request)?;
        let schedules: Vec<ScheduleDetails> = self
            .read_query_json_secret(SCHEDULE_LIST, &request.query)
            .await?;
        let mut seen_ids = HashSet::new();
        if schedules.len() > SCHEDULE_LIST_ITEM_LIMIT
            || schedules.iter().any(|schedule| {
                !schedule.is_valid() || !seen_ids.insert(schedule.schedule_id.as_str().to_owned())
            })
        {
            return Err(Error::UnexpectedResponse {
                operation: SCHEDULE_LIST.operation(),
            });
        }

        Ok(schedules)
    }

    pub(crate) async fn schedule_create(
        &self,
        input: CreateSchedule,
    ) -> Result<CreatedSchedule, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                SCHEDULE_CREATE.operation(),
                "Schedule create fields are invalid",
            ));
        }
        let before = self.schedules_by_target(input.target()).await?;
        if before
            .schedules()
            .iter()
            .any(|schedule| schedule.name == input.name())
        {
            return Err(Error::UnexpectedResponse {
                operation: SCHEDULE_CREATE.operation(),
            });
        }

        let created: ScheduleProofDetails = self
            .mutate_body_json_secret(SCHEDULE_CREATE, &input)
            .await?;
        if !input.matches(&created) {
            return Err(post_mutation_proof_unknown(SCHEDULE_CREATE));
        }
        let created = created.into_details();
        let after = self
            .schedules_by_target(input.target())
            .await
            .map_err(|_| post_mutation_proof_unknown(SCHEDULE_CREATE))?;
        let before_ids = before
            .schedules()
            .iter()
            .map(|schedule| schedule.schedule_id.as_str().to_owned())
            .collect::<HashSet<_>>();
        let after_ids = after
            .schedules()
            .iter()
            .map(|schedule| schedule.schedule_id.as_str().to_owned())
            .collect::<HashSet<_>>();
        let new_ids = after_ids.difference(&before_ids).collect::<Vec<_>>();
        let matching = after.schedules().iter().find(|schedule| {
            schedule.schedule_id == created.schedule_id && schedule.name == created.name
        });
        if !before_ids.is_subset(&after_ids)
            || new_ids.len() != 1
            || new_ids[0].as_str() != created.schedule_id.as_str()
            || matching != Some(&created)
        {
            return Err(post_mutation_proof_unknown(SCHEDULE_CREATE));
        }

        Ok(CreatedSchedule::new(created.schedule_id))
    }

    pub(crate) async fn schedule_update(&self, input: UpdateSchedule) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                SCHEDULE_UPDATE.operation(),
                "Schedule update fields are invalid",
            ));
        }
        let (existing, before) = self
            .schedule_get_with_collection(input.schedule_id().as_str())
            .await?;
        if existing.target != *input.target()
            || before.schedules().iter().any(|schedule| {
                schedule.schedule_id != *input.schedule_id() && schedule.name == input.name()
            })
        {
            return Err(Error::UnexpectedResponse {
                operation: SCHEDULE_UPDATE.operation(),
            });
        }
        let updated: ScheduleProofDetails = self
            .mutate_body_json_secret(SCHEDULE_UPDATE, &input)
            .await?;
        if !input.matches(&updated) {
            return Err(post_mutation_proof_unknown(SCHEDULE_UPDATE));
        }
        let updated = updated.into_details();
        let after = self
            .schedules_by_target(input.target())
            .await
            .map_err(|_| post_mutation_proof_unknown(SCHEDULE_UPDATE))?;
        let matching = after
            .schedules()
            .iter()
            .find(|schedule| schedule.schedule_id == updated.schedule_id);
        if matching != Some(&updated) {
            return Err(post_mutation_proof_unknown(SCHEDULE_UPDATE));
        }

        Ok(())
    }

    pub(crate) async fn schedule_delete(
        &self,
        schedule_id: ScheduleId,
        target: ScheduleTarget,
    ) -> Result<(), Error> {
        if !target.is_valid() {
            return Err(invalid_request(
                SCHEDULE_DELETE.operation(),
                "Schedule target fields are invalid",
            ));
        }
        let request = ScheduleDeleteRequest {
            body: ScheduleIdRequestBody {
                schedule_id: schedule_id.as_str().to_owned(),
            },
        };
        validate_generated_request(SCHEDULE_DELETE, &request)?;
        let (existing, _) = self
            .schedule_get_with_collection(schedule_id.as_str())
            .await?;
        if existing.target != target {
            return Err(Error::UnexpectedResponse {
                operation: SCHEDULE_DELETE.operation(),
            });
        }

        self.mutate_body_ok(SCHEDULE_DELETE, &request.body).await?;

        let after = self
            .schedules_by_target(&target)
            .await
            .map_err(|_| post_mutation_proof_unknown(SCHEDULE_DELETE))?;
        if after
            .schedules()
            .iter()
            .any(|schedule| schedule.schedule_id == schedule_id)
        {
            return Err(post_mutation_proof_unknown(SCHEDULE_DELETE));
        }

        Ok(())
    }

    pub(crate) async fn backup_get(&self, backup_id: &str) -> Result<BackupDetails, Error> {
        self.backup_get_with_collection(backup_id)
            .await
            .map(|(details, _)| details)
    }

    async fn backup_get_with_collection(
        &self,
        backup_id: &str,
    ) -> Result<(BackupDetails, BackupCollection), Error> {
        let request = BackupOneRequest {
            query: BackupOneRequestQuery {
                backup_id: backup_id.to_owned(),
            },
        };
        validate_generated_request(BACKUP_ONE, &request)?;
        let details: BackupDetails = self
            .read_query_json_secret(BACKUP_ONE, &request.query)
            .await?;
        if !details.is_valid() || details.backup_id.as_str() != backup_id {
            return Err(Error::UnexpectedResponse {
                operation: BACKUP_ONE.operation(),
            });
        }
        self.validate_backup_destination(&details.destination_id, BACKUP_ONE)
            .await?;
        let collection = self.backups_by_target(&details.target).await?;
        let matching = collection
            .backups()
            .iter()
            .find(|backup| backup.backup_id.as_str() == backup_id);
        if matching != Some(&details) {
            return Err(Error::UnexpectedResponse {
                operation: BACKUP_ONE.operation(),
            });
        }

        Ok((details, collection))
    }

    pub(crate) async fn backups_by_target(
        &self,
        target: &BackupTarget,
    ) -> Result<BackupCollection, Error> {
        if !target.is_valid() {
            return Err(invalid_request(
                BACKUP_ONE.operation(),
                "Backup target identity cannot be empty",
            ));
        }

        let (parent_id, backups) = match target {
            BackupTarget::Postgres(id) => {
                let request = PostgresOneRequest {
                    query: PostgresOneRequestQuery {
                        postgres_id: id.as_str().to_owned(),
                    },
                };
                validate_generated_request(POSTGRES_ONE, &request)?;
                let response: PostgresBackupCollectionResponse = self
                    .read_query_json_secret(POSTGRES_ONE, &request.query)
                    .await?;
                (response.postgres_id.as_str().to_owned(), response.backups)
            }
            BackupTarget::MySql(id) => {
                let request = MysqlOneRequest {
                    query: MysqlOneRequestQuery {
                        mysql_id: id.as_str().to_owned(),
                    },
                };
                validate_generated_request(MYSQL_ONE, &request)?;
                let response: MySqlBackupCollectionResponse = self
                    .read_query_json_secret(MYSQL_ONE, &request.query)
                    .await?;
                (response.mysql_id.as_str().to_owned(), response.backups)
            }
            BackupTarget::MariaDb(id) => {
                let request = MariadbOneRequest {
                    query: MariadbOneRequestQuery {
                        mariadb_id: id.as_str().to_owned(),
                    },
                };
                validate_generated_request(MARIADB_ONE, &request)?;
                let response: MariaDbBackupCollectionResponse = self
                    .read_query_json_secret(MARIADB_ONE, &request.query)
                    .await?;
                (response.mariadb_id.as_str().to_owned(), response.backups)
            }
            BackupTarget::Mongo(id) => {
                let request = MongoOneRequest {
                    query: MongoOneRequestQuery {
                        mongo_id: id.as_str().to_owned(),
                    },
                };
                validate_generated_request(MONGO_ONE, &request)?;
                let response: MongoBackupCollectionResponse = self
                    .read_query_json_secret(MONGO_ONE, &request.query)
                    .await?;
                (response.mongo_id.as_str().to_owned(), response.backups)
            }
            BackupTarget::LibSql(id) => {
                let request = LibsqlOneRequest {
                    query: LibsqlOneRequestQuery {
                        libsql_id: id.as_str().to_owned(),
                    },
                };
                validate_generated_request(LIBSQL_ONE, &request)?;
                let response: LibSqlBackupCollectionResponse = self
                    .read_query_json_secret(LIBSQL_ONE, &request.query)
                    .await?;
                (response.libsql_id.as_str().to_owned(), response.backups)
            }
        };
        let mut seen_ids = HashSet::new();
        let mut seen_collisions = HashSet::new();
        let contradictory = parent_id != target.id()
            || backups.len() > BACKUP_LIST_ITEM_LIMIT
            || backups.iter().any(|backup| {
                !backup.is_valid()
                    || backup.target != *target
                    || !seen_ids.insert(backup.backup_id.as_str().to_owned())
                    || !seen_collisions.insert(backup.collision_key())
            });
        if contradictory {
            return Err(Error::UnexpectedResponse {
                operation: BACKUP_ONE.operation(),
            });
        }

        Ok(BackupCollection::new(target.clone(), backups))
    }

    async fn validate_backup_destination(
        &self,
        destination_id: &crate::DestinationId,
        endpoint: Endpoint,
    ) -> Result<(), Error> {
        let destinations = self.destination_all().await?;
        if destinations
            .destinations()
            .iter()
            .any(|destination| destination.destination_id == *destination_id)
        {
            return Ok(());
        }

        Err(Error::UnexpectedResponse {
            operation: endpoint.operation(),
        })
    }

    pub(crate) async fn backup_create(&self, input: CreateBackup) -> Result<CreatedBackup, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                BACKUP_CREATE.operation(),
                "Backup create fields are invalid",
            ));
        }
        self.validate_backup_destination(input.destination_id(), BACKUP_CREATE)
            .await?;
        let before = self.backups_by_target(input.target()).await?;
        let collision_key = input.collision_key();
        if before
            .backups()
            .iter()
            .any(|backup| backup.collision_key() == collision_key)
        {
            return Err(Error::UnexpectedResponse {
                operation: BACKUP_CREATE.operation(),
            });
        }

        self.mutate_body_ok_secret(BACKUP_CREATE, &input).await?;

        let after = self
            .backups_by_target(input.target())
            .await
            .map_err(|_| post_mutation_proof_unknown(BACKUP_CREATE))?;
        let before_ids = before
            .backups()
            .iter()
            .map(|backup| backup.backup_id.as_str().to_owned())
            .collect::<HashSet<_>>();
        let after_ids = after
            .backups()
            .iter()
            .map(|backup| backup.backup_id.as_str().to_owned())
            .collect::<HashSet<_>>();
        let new_ids = after_ids.difference(&before_ids).collect::<Vec<_>>();
        if !before_ids.is_subset(&after_ids) || new_ids.len() != 1 {
            return Err(post_mutation_proof_unknown(BACKUP_CREATE));
        }
        let created = after
            .backups()
            .iter()
            .find(|backup| backup.backup_id.as_str() == new_ids[0].as_str());
        if !created.is_some_and(|backup| input.matches(backup)) {
            return Err(post_mutation_proof_unknown(BACKUP_CREATE));
        }

        Ok(CreatedBackup::new(
            created
                .expect("the matching Backup was checked")
                .backup_id
                .clone(),
        ))
    }

    pub(crate) async fn backup_update(&self, input: UpdateBackup) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                BACKUP_UPDATE.operation(),
                "Backup update fields are invalid",
            ));
        }
        self.validate_backup_destination(input.destination_id(), BACKUP_UPDATE)
            .await?;
        let (existing, before) = self
            .backup_get_with_collection(input.backup_id().as_str())
            .await?;
        let collision_key = input.collision_key();
        if existing.target != *input.target()
            || before.backups().iter().any(|backup| {
                backup.backup_id != *input.backup_id() && backup.collision_key() == collision_key
            })
        {
            return Err(Error::UnexpectedResponse {
                operation: BACKUP_UPDATE.operation(),
            });
        }

        self.mutate_body_ok_secret(BACKUP_UPDATE, &input).await?;

        let (updated, _) = self
            .backup_get_with_collection(input.backup_id().as_str())
            .await
            .map_err(|_| post_mutation_proof_unknown(BACKUP_UPDATE))?;
        if !input.matches(&updated) {
            return Err(post_mutation_proof_unknown(BACKUP_UPDATE));
        }

        Ok(())
    }

    pub(crate) async fn backup_delete(
        &self,
        backup_id: BackupId,
        target: BackupTarget,
    ) -> Result<(), Error> {
        if backup_id.as_str().is_empty() || !target.is_valid() {
            return Err(invalid_request(
                BACKUP_REMOVE.operation(),
                "Backup identity and target cannot be empty",
            ));
        }
        let request = BackupRemoveRequest {
            body: BackupIdRequestBody {
                backup_id: backup_id.as_str().to_owned(),
            },
        };
        validate_generated_request(BACKUP_REMOVE, &request)?;
        let (existing, _) = self.backup_get_with_collection(backup_id.as_str()).await?;
        if existing.target != target {
            return Err(Error::UnexpectedResponse {
                operation: BACKUP_REMOVE.operation(),
            });
        }

        self.mutate_body_ok_secret(BACKUP_REMOVE, &request.body)
            .await?;

        let after = self
            .backups_by_target(&target)
            .await
            .map_err(|_| post_mutation_proof_unknown(BACKUP_REMOVE))?;
        if after
            .backups()
            .iter()
            .any(|backup| backup.backup_id == backup_id)
        {
            return Err(post_mutation_proof_unknown(BACKUP_REMOVE));
        }

        Ok(())
    }

    pub(crate) async fn environment_get(
        &self,
        environment_id: &str,
    ) -> Result<EnvironmentDetails, Error> {
        let request = EnvironmentOneRequest {
            query: EnvironmentOneRequestQuery {
                environment_id: environment_id.to_owned(),
            },
        };
        validate_generated_request(ENVIRONMENT_ONE, &request)?;

        self.read_query_json(ENVIRONMENT_ONE, &request.query).await
    }

    pub(crate) async fn environments_by_project(
        &self,
        project_id: &str,
    ) -> Result<EnvironmentCollection, Error> {
        let request = EnvironmentByProjectIdRequest {
            query: EnvironmentByProjectIdRequestQuery {
                project_id: project_id.to_owned(),
            },
        };
        validate_generated_request(ENVIRONMENT_BY_PROJECT_ID, &request)?;

        self.read_query_json(ENVIRONMENT_BY_PROJECT_ID, &request.query)
            .await
    }

    pub(crate) async fn environment_create(
        &self,
        input: CreateEnvironment,
    ) -> Result<CreatedEnvironment, Error> {
        let request = EnvironmentCreateRequest {
            body: EnvironmentCreateRequestBody {
                name: input.name,
                description: input.description,
                project_id: input.project_id.as_str().to_owned(),
            },
        };
        validate_generated_request(ENVIRONMENT_CREATE, &request)?;
        let response: EnvironmentCreateResponse = self
            .mutate_body_json(ENVIRONMENT_CREATE, &request.body)
            .await?;

        Ok(CreatedEnvironment::from_response(response))
    }

    pub(crate) async fn environment_update(&self, input: UpdateEnvironment) -> Result<(), Error> {
        self.mutate_body_ok(ENVIRONMENT_UPDATE, &input).await
    }

    pub(crate) async fn environment_delete(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<(), Error> {
        let request = EnvironmentRemoveRequest {
            body: EnvironmentIdRequestBody {
                environment_id: environment_id.as_str().to_owned(),
            },
        };
        validate_generated_request(ENVIRONMENT_REMOVE, &request)?;

        self.mutate_body_ok(ENVIRONMENT_REMOVE, &request.body).await
    }

    pub(crate) async fn postgres_get(&self, postgres_id: &str) -> Result<PostgresDetails, Error> {
        let request = PostgresOneRequest {
            query: PostgresOneRequestQuery {
                postgres_id: postgres_id.to_owned(),
            },
        };
        validate_generated_request(POSTGRES_ONE, &request)?;

        self.read_query_json(POSTGRES_ONE, &request.query).await
    }

    pub(crate) async fn libsql_get(&self, libsql_id: &str) -> Result<LibSqlDetails, Error> {
        let request = LibsqlOneRequest {
            query: LibsqlOneRequestQuery {
                libsql_id: libsql_id.to_owned(),
            },
        };
        validate_generated_request(LIBSQL_ONE, &request)?;

        self.read_query_json(LIBSQL_ONE, &request.query).await
    }

    pub(crate) async fn libsql_by_environment(
        &self,
        project_id: &str,
        environment_id: &str,
    ) -> Result<LibSqlCollection, Error> {
        if project_id.is_empty() || environment_id.is_empty() {
            return Err(invalid_request(
                PROJECT_ONE.operation(),
                "project and environment IDs cannot be empty",
            ));
        }
        let project = self.project_get(project_id).await?;
        if project.project_id.as_str() != project_id {
            return Err(Error::UnexpectedResponse {
                operation: PROJECT_ONE.operation(),
            });
        }
        let mut environments = project
            .environments
            .into_iter()
            .filter(|environment| environment.environment_id.as_str() == environment_id);
        let Some(environment) = environments.next() else {
            return Err(Error::UnexpectedResponse {
                operation: PROJECT_ONE.operation(),
            });
        };
        if environments.next().is_some() || environment.libsql.len() > LIBSQL_TOPOLOGY_ITEM_LIMIT {
            return Err(Error::UnexpectedResponse {
                operation: PROJECT_ONE.operation(),
            });
        }

        let Some(libsql) = environment
            .libsql
            .into_iter()
            .map(crate::models::LibSqlTopologySummary::into_search_item)
            .collect::<Option<Vec<_>>>()
        else {
            return Err(Error::UnexpectedResponse {
                operation: PROJECT_ONE.operation(),
            });
        };

        Ok(LibSqlCollection { libsql })
    }

    pub(crate) async fn libsql_create(&self, input: CreateLibSql) -> Result<CreatedLibSql, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                LIBSQL_CREATE.operation(),
                "LibSQL create fields are invalid",
            ));
        }
        let before = self
            .libsql_by_environment(input.project_id().as_str(), input.environment_id().as_str())
            .await?;
        if before
            .libsql()
            .iter()
            .any(|libsql| libsql.name == input.name())
        {
            return Err(Error::UnexpectedResponse {
                operation: LIBSQL_CREATE.operation(),
            });
        }
        let expected_server_placement = input.server_placement().cloned();

        let created: bool = self.mutate_body_json(LIBSQL_CREATE, &input).await?;
        if !created {
            return Err(post_mutation_proof_unknown(LIBSQL_CREATE));
        }

        let after = self
            .libsql_by_environment(input.project_id().as_str(), input.environment_id().as_str())
            .await
            .map_err(|_| post_mutation_proof_unknown(LIBSQL_CREATE))?;
        let matches = after
            .libsql()
            .iter()
            .filter(|libsql| {
                libsql.name == input.name()
                    && expected_server_placement
                        .as_ref()
                        .is_none_or(|placement| placement.matches_response(&libsql.server_id))
            })
            .collect::<Vec<_>>();
        if let [created] = matches.as_slice() {
            return Ok(CreatedLibSql::new(created.libsql_id.clone()));
        }

        Err(post_mutation_proof_unknown(LIBSQL_CREATE))
    }

    pub(crate) async fn libsql_update(&self, input: UpdateLibSql) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                LIBSQL_UPDATE.operation(),
                "LibSQL update requires an identity and at least one non-empty field",
            ));
        }

        self.mutate_body_ok(LIBSQL_UPDATE, &input).await
    }

    pub(crate) async fn libsql_change_password(
        &self,
        input: ChangeLibSqlPassword,
    ) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                LIBSQL_UPDATE.operation(),
                "LibSQL password change fields are invalid",
            ));
        }

        self.mutate_body_ok(LIBSQL_UPDATE, &input).await
    }

    pub(crate) async fn libsql_delete(&self, libsql_id: LibSqlId) -> Result<(), Error> {
        let request = LibsqlRemoveRequest {
            body: LibsqlIdRequestBody {
                libsql_id: libsql_id.as_str().to_owned(),
            },
        };
        validate_generated_request(LIBSQL_REMOVE, &request)?;

        self.mutate_body_ok(LIBSQL_REMOVE, &request.body).await
    }

    pub(crate) async fn mysql_get(&self, mysql_id: &str) -> Result<MySqlDetails, Error> {
        let request = MysqlOneRequest {
            query: MysqlOneRequestQuery {
                mysql_id: mysql_id.to_owned(),
            },
        };
        validate_generated_request(MYSQL_ONE, &request)?;

        self.read_query_json(MYSQL_ONE, &request.query).await
    }

    pub(crate) async fn mariadb_get(&self, mariadb_id: &str) -> Result<MariaDbDetails, Error> {
        let request = MariadbOneRequest {
            query: MariadbOneRequestQuery {
                mariadb_id: mariadb_id.to_owned(),
            },
        };
        validate_generated_request(MARIADB_ONE, &request)?;

        self.read_query_json(MARIADB_ONE, &request.query).await
    }

    pub(crate) async fn mongo_get(&self, mongo_id: &str) -> Result<MongoDetails, Error> {
        let request = MongoOneRequest {
            query: MongoOneRequestQuery {
                mongo_id: mongo_id.to_owned(),
            },
        };
        validate_generated_request(MONGO_ONE, &request)?;

        self.read_query_json(MONGO_ONE, &request.query).await
    }

    pub(crate) async fn mongo_create(&self, input: CreateMongo) -> Result<CreatedMongo, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MONGO_CREATE.operation(),
                "MongoDB create fields are invalid",
            ));
        }
        let expected_server_placement = input.server_placement().cloned();
        let response: MongoCreateResponse = self.mutate_body_json(MONGO_CREATE, &input).await?;
        if response.mongo_id.as_str().is_empty()
            || expected_server_placement
                .as_ref()
                .is_some_and(|placement| !placement.matches_response(&response.server_id))
        {
            return Err(post_mutation_proof_unknown(MONGO_CREATE));
        }

        Ok(CreatedMongo::from_response(response))
    }

    pub(crate) async fn mongo_update(&self, input: UpdateMongo) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MONGO_UPDATE.operation(),
                "MongoDB update requires an identity and at least one non-empty field",
            ));
        }

        self.mutate_body_ok(MONGO_UPDATE, &input).await
    }

    pub(crate) async fn mongo_change_password(
        &self,
        input: ChangeMongoPassword,
    ) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MONGO_CHANGE_PASSWORD.operation(),
                "MongoDB password change fields are invalid",
            ));
        }

        self.mutate_body_ok(MONGO_CHANGE_PASSWORD, &input).await
    }

    pub(crate) async fn mongo_delete(&self, mongo_id: MongoId) -> Result<(), Error> {
        let request = MongoRemoveRequest {
            body: MongoIdRequestBody {
                mongo_id: mongo_id.as_str().to_owned(),
            },
        };
        validate_generated_request(MONGO_REMOVE, &request)?;

        self.mutate_body_ok(MONGO_REMOVE, &request.body).await
    }

    pub(crate) async fn mongo_by_environment(
        &self,
        environment_id: &str,
    ) -> Result<MongoCollection, Error> {
        if environment_id.is_empty() {
            return Err(invalid_request(
                MONGO_SEARCH.operation(),
                "environment ID cannot be empty",
            ));
        }
        let mut mongo = Vec::new();
        let mut expected_total = None;

        loop {
            let request = MongoSearchRequest {
                query: MongoSearchRequestQuery {
                    environment_id: Some(environment_id.to_owned()),
                    limit: Some(MONGO_SEARCH_PAGE_SIZE as f64),
                    offset: Some(mongo.len() as f64),
                    ..MongoSearchRequestQuery::default()
                },
            };
            validate_generated_request(MONGO_SEARCH, &request)?;
            let page: MongoSearchPage = self.read_query_json(MONGO_SEARCH, &request.query).await?;

            let expected = *expected_total.get_or_insert(page.total);
            if page.total != expected
                || expected > MONGO_SEARCH_ITEM_LIMIT as u64
                || page.items.len() > MONGO_SEARCH_PAGE_SIZE
            {
                return Err(Error::UnexpectedResponse {
                    operation: MONGO_SEARCH.operation(),
                });
            }
            let expected = usize::try_from(expected).map_err(|_| Error::UnexpectedResponse {
                operation: MONGO_SEARCH.operation(),
            })?;
            let page_would_exceed_total = mongo
                .len()
                .checked_add(page.items.len())
                .is_none_or(|count| count > expected);
            if page_would_exceed_total || (page.items.is_empty() && mongo.len() < expected) {
                return Err(Error::UnexpectedResponse {
                    operation: MONGO_SEARCH.operation(),
                });
            }

            mongo.extend(page.items);
            if mongo.len() == expected {
                return Ok(MongoCollection { mongo });
            }
        }
    }

    pub(crate) async fn mariadb_create(
        &self,
        input: CreateMariaDb,
    ) -> Result<CreatedMariaDb, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MARIADB_CREATE.operation(),
                "MariaDB create fields are invalid",
            ));
        }
        let expected_server_placement = input.server_placement().cloned();
        let response: MariaDbCreateResponse = self.mutate_body_json(MARIADB_CREATE, &input).await?;
        if response.mariadb_id.as_str().is_empty()
            || expected_server_placement
                .as_ref()
                .is_some_and(|placement| !placement.matches_response(&response.server_id))
        {
            return Err(post_mutation_proof_unknown(MARIADB_CREATE));
        }

        Ok(CreatedMariaDb::from_response(response))
    }

    pub(crate) async fn mariadb_update(&self, input: UpdateMariaDb) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MARIADB_UPDATE.operation(),
                "MariaDB update requires an identity and at least one non-empty field",
            ));
        }

        self.mutate_body_ok(MARIADB_UPDATE, &input).await
    }

    pub(crate) async fn mariadb_change_password(
        &self,
        input: ChangeMariaDbPassword,
    ) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MARIADB_CHANGE_PASSWORD.operation(),
                "MariaDB password change fields are invalid",
            ));
        }

        self.mutate_body_ok(MARIADB_CHANGE_PASSWORD, &input).await
    }

    pub(crate) async fn mariadb_delete(&self, mariadb_id: MariaDbId) -> Result<(), Error> {
        let request = MariadbRemoveRequest {
            body: MariadbIdRequestBody {
                mariadb_id: mariadb_id.as_str().to_owned(),
            },
        };
        validate_generated_request(MARIADB_REMOVE, &request)?;

        self.mutate_body_ok(MARIADB_REMOVE, &request.body).await
    }

    pub(crate) async fn mariadb_by_environment(
        &self,
        environment_id: &str,
    ) -> Result<MariaDbCollection, Error> {
        if environment_id.is_empty() {
            return Err(invalid_request(
                MARIADB_SEARCH.operation(),
                "environment ID cannot be empty",
            ));
        }
        let mut mariadb = Vec::new();
        let mut expected_total = None;

        loop {
            let request = MariadbSearchRequest {
                query: MariadbSearchRequestQuery {
                    environment_id: Some(environment_id.to_owned()),
                    limit: Some(MARIADB_SEARCH_PAGE_SIZE as f64),
                    offset: Some(mariadb.len() as f64),
                    ..MariadbSearchRequestQuery::default()
                },
            };
            validate_generated_request(MARIADB_SEARCH, &request)?;
            let page: MariaDbSearchPage =
                self.read_query_json(MARIADB_SEARCH, &request.query).await?;

            let expected = *expected_total.get_or_insert(page.total);
            if page.total != expected
                || expected > MARIADB_SEARCH_ITEM_LIMIT as u64
                || page.items.len() > MARIADB_SEARCH_PAGE_SIZE
            {
                return Err(Error::UnexpectedResponse {
                    operation: MARIADB_SEARCH.operation(),
                });
            }
            let expected = usize::try_from(expected).map_err(|_| Error::UnexpectedResponse {
                operation: MARIADB_SEARCH.operation(),
            })?;
            let page_would_exceed_total = mariadb
                .len()
                .checked_add(page.items.len())
                .is_none_or(|count| count > expected);
            if page_would_exceed_total || (page.items.is_empty() && mariadb.len() < expected) {
                return Err(Error::UnexpectedResponse {
                    operation: MARIADB_SEARCH.operation(),
                });
            }

            mariadb.extend(page.items);
            if mariadb.len() == expected {
                return Ok(MariaDbCollection { mariadb });
            }
        }
    }

    pub(crate) async fn mysql_create(&self, input: CreateMySql) -> Result<CreatedMySql, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MYSQL_CREATE.operation(),
                "MySQL create fields cannot be empty",
            ));
        }
        let expected_server_placement = input.server_placement().cloned();
        let response: MySqlCreateResponse = self.mutate_body_json(MYSQL_CREATE, &input).await?;
        if response.mysql_id.as_str().is_empty()
            || expected_server_placement
                .as_ref()
                .is_some_and(|placement| !placement.matches_response(&response.server_id))
        {
            return Err(post_mutation_proof_unknown(MYSQL_CREATE));
        }

        Ok(CreatedMySql::from_response(response))
    }

    pub(crate) async fn mysql_update(&self, input: UpdateMySql) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MYSQL_UPDATE.operation(),
                "MySQL update requires an identity and at least one non-empty field",
            ));
        }

        self.mutate_body_ok(MYSQL_UPDATE, &input).await
    }

    pub(crate) async fn mysql_change_password(
        &self,
        input: ChangeMySqlPassword,
    ) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                MYSQL_CHANGE_PASSWORD.operation(),
                "MySQL password change fields are invalid",
            ));
        }

        self.mutate_body_ok(MYSQL_CHANGE_PASSWORD, &input).await
    }

    pub(crate) async fn mysql_delete(&self, mysql_id: MySqlId) -> Result<(), Error> {
        let request = MysqlRemoveRequest {
            body: MysqlIdRequestBody {
                mysql_id: mysql_id.as_str().to_owned(),
            },
        };
        validate_generated_request(MYSQL_REMOVE, &request)?;

        self.mutate_body_ok(MYSQL_REMOVE, &request.body).await
    }

    pub(crate) async fn mysql_by_environment(
        &self,
        environment_id: &str,
    ) -> Result<MySqlCollection, Error> {
        if environment_id.is_empty() {
            return Err(invalid_request(
                MYSQL_SEARCH.operation(),
                "environment ID cannot be empty",
            ));
        }
        let mut mysql = Vec::new();
        let mut expected_total = None;

        loop {
            let request = MysqlSearchRequest {
                query: MysqlSearchRequestQuery {
                    environment_id: Some(environment_id.to_owned()),
                    limit: Some(MYSQL_SEARCH_PAGE_SIZE as f64),
                    offset: Some(mysql.len() as f64),
                    ..MysqlSearchRequestQuery::default()
                },
            };
            validate_generated_request(MYSQL_SEARCH, &request)?;
            let page: MySqlSearchPage = self.read_query_json(MYSQL_SEARCH, &request.query).await?;

            let expected = *expected_total.get_or_insert(page.total);
            if page.total != expected
                || expected > MYSQL_SEARCH_ITEM_LIMIT as u64
                || page.items.len() > MYSQL_SEARCH_PAGE_SIZE
            {
                return Err(Error::UnexpectedResponse {
                    operation: MYSQL_SEARCH.operation(),
                });
            }
            let expected = usize::try_from(expected).map_err(|_| Error::UnexpectedResponse {
                operation: MYSQL_SEARCH.operation(),
            })?;
            let page_would_exceed_total = mysql
                .len()
                .checked_add(page.items.len())
                .is_none_or(|count| count > expected);
            if page_would_exceed_total || (page.items.is_empty() && mysql.len() < expected) {
                return Err(Error::UnexpectedResponse {
                    operation: MYSQL_SEARCH.operation(),
                });
            }

            mysql.extend(page.items);
            if mysql.len() == expected {
                return Ok(MySqlCollection { mysql });
            }
        }
    }

    pub(crate) async fn postgres_create(
        &self,
        input: CreatePostgres,
    ) -> Result<CreatedPostgres, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                POSTGRES_CREATE.operation(),
                "Postgres create fields cannot be empty",
            ));
        }
        let expected_server_placement = input.server_placement().cloned();
        let response: PostgresCreateResponse =
            self.mutate_body_json(POSTGRES_CREATE, &input).await?;
        if response.postgres_id.as_str().is_empty()
            || expected_server_placement
                .as_ref()
                .is_some_and(|placement| !placement.matches_response(&response.server_id))
        {
            return Err(post_mutation_proof_unknown(POSTGRES_CREATE));
        }

        Ok(CreatedPostgres::from_response(response))
    }

    pub(crate) async fn postgres_update(&self, input: UpdatePostgres) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                POSTGRES_UPDATE.operation(),
                "Postgres update requires an identity and at least one field",
            ));
        }

        self.mutate_body_ok(POSTGRES_UPDATE, &input).await
    }

    pub(crate) async fn postgres_delete(&self, postgres_id: PostgresId) -> Result<(), Error> {
        let request = PostgresRemoveRequest {
            body: PostgresIdRequestBody {
                postgres_id: postgres_id.as_str().to_owned(),
            },
        };
        validate_generated_request(POSTGRES_REMOVE, &request)?;

        self.mutate_body_ok(POSTGRES_REMOVE, &request.body).await
    }

    pub(crate) async fn postgres_by_environment(
        &self,
        environment_id: &str,
    ) -> Result<PostgresCollection, Error> {
        if environment_id.is_empty() {
            return Err(invalid_request(
                POSTGRES_SEARCH.operation(),
                "environment ID cannot be empty",
            ));
        }
        let mut postgres = Vec::new();
        let mut expected_total = None;

        loop {
            let request = PostgresSearchRequest {
                query: PostgresSearchRequestQuery {
                    environment_id: Some(environment_id.to_owned()),
                    limit: Some(POSTGRES_SEARCH_PAGE_SIZE as f64),
                    offset: Some(postgres.len() as f64),
                    ..PostgresSearchRequestQuery::default()
                },
            };
            validate_generated_request(POSTGRES_SEARCH, &request)?;
            let page: PostgresSearchPage = self
                .read_query_json(POSTGRES_SEARCH, &request.query)
                .await?;

            let expected = *expected_total.get_or_insert(page.total);
            if page.total != expected
                || expected > POSTGRES_SEARCH_ITEM_LIMIT as u64
                || page.items.len() > POSTGRES_SEARCH_PAGE_SIZE
            {
                return Err(Error::UnexpectedResponse {
                    operation: POSTGRES_SEARCH.operation(),
                });
            }
            let expected = usize::try_from(expected).map_err(|_| Error::UnexpectedResponse {
                operation: POSTGRES_SEARCH.operation(),
            })?;
            let page_would_exceed_total = postgres
                .len()
                .checked_add(page.items.len())
                .is_none_or(|count| count > expected);
            if page_would_exceed_total || (page.items.is_empty() && postgres.len() < expected) {
                return Err(Error::UnexpectedResponse {
                    operation: POSTGRES_SEARCH.operation(),
                });
            }

            postgres.extend(page.items);
            if postgres.len() == expected {
                return Ok(PostgresCollection { postgres });
            }
        }
    }

    pub(crate) async fn redis_get(&self, redis_id: &str) -> Result<RedisDetails, Error> {
        let request = RedisOneRequest {
            query: RedisOneRequestQuery {
                redis_id: redis_id.to_owned(),
            },
        };
        validate_generated_request(REDIS_ONE, &request)?;

        self.read_query_json(REDIS_ONE, &request.query).await
    }

    pub(crate) async fn redis_create(&self, input: CreateRedis) -> Result<CreatedRedis, Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                REDIS_CREATE.operation(),
                "Redis create fields cannot be empty",
            ));
        }
        let expected_server_placement = input.server_placement().cloned();
        let response: RedisCreateResponse = self.mutate_body_json(REDIS_CREATE, &input).await?;
        if response.redis_id.as_str().is_empty()
            || expected_server_placement
                .as_ref()
                .is_some_and(|placement| !placement.matches_response(&response.server_id))
        {
            return Err(post_mutation_proof_unknown(REDIS_CREATE));
        }

        Ok(CreatedRedis::from_response(response))
    }

    pub(crate) async fn redis_update(&self, input: UpdateRedis) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                REDIS_UPDATE.operation(),
                "Redis update fields cannot be empty",
            ));
        }

        self.mutate_body_ok(REDIS_UPDATE, &input).await
    }

    pub(crate) async fn redis_delete(&self, redis_id: RedisId) -> Result<(), Error> {
        let request = RedisRemoveRequest {
            body: RedisIdRequestBody {
                redis_id: redis_id.as_str().to_owned(),
            },
        };
        validate_generated_request(REDIS_REMOVE, &request)?;

        self.mutate_body_ok(REDIS_REMOVE, &request.body).await
    }

    pub(crate) async fn redis_by_environment(
        &self,
        environment_id: &str,
    ) -> Result<RedisCollection, Error> {
        if environment_id.is_empty() {
            return Err(invalid_request(
                REDIS_SEARCH.operation(),
                "environment ID cannot be empty",
            ));
        }
        let mut redis = Vec::new();
        let mut expected_total = None;

        loop {
            let request = RedisSearchRequest {
                query: RedisSearchRequestQuery {
                    environment_id: Some(environment_id.to_owned()),
                    limit: Some(REDIS_SEARCH_PAGE_SIZE as f64),
                    offset: Some(redis.len() as f64),
                    ..RedisSearchRequestQuery::default()
                },
            };
            validate_generated_request(REDIS_SEARCH, &request)?;
            let page: RedisSearchPage = self.read_query_json(REDIS_SEARCH, &request.query).await?;

            let expected = *expected_total.get_or_insert(page.total);
            if page.total != expected
                || expected > REDIS_SEARCH_ITEM_LIMIT as u64
                || page.items.len() > REDIS_SEARCH_PAGE_SIZE
            {
                return Err(Error::UnexpectedResponse {
                    operation: REDIS_SEARCH.operation(),
                });
            }
            let expected = usize::try_from(expected).map_err(|_| Error::UnexpectedResponse {
                operation: REDIS_SEARCH.operation(),
            })?;
            let page_would_exceed_total = redis
                .len()
                .checked_add(page.items.len())
                .is_none_or(|count| count > expected);
            if page_would_exceed_total || (page.items.is_empty() && redis.len() < expected) {
                return Err(Error::UnexpectedResponse {
                    operation: REDIS_SEARCH.operation(),
                });
            }

            redis.extend(page.items);
            if redis.len() == expected {
                return Ok(RedisCollection { redis });
            }
        }
    }

    pub(crate) async fn domain_get(&self, domain_id: &str) -> Result<DomainDetails, Error> {
        let request = DomainOneRequest {
            query: DomainOneRequestQuery {
                domain_id: domain_id.to_owned(),
            },
        };
        validate_generated_request(DOMAIN_ONE, &request)?;

        self.read_query_json(DOMAIN_ONE, &request.query).await
    }

    pub(crate) async fn domain_create(&self, input: CreateDomain) -> Result<CreatedDomain, Error> {
        let request = DomainCreateRequest {
            body: DomainCreateRequestBody {
                host: input.host,
                application_id: Some(input.application_id.as_str().to_owned()),
                domain_type: Some("application".to_owned()),
                ..DomainCreateRequestBody::default()
            },
        };
        validate_generated_request(DOMAIN_CREATE, &request)?;
        let response: DomainCreateResponse =
            self.mutate_body_json(DOMAIN_CREATE, &request.body).await?;

        Ok(CreatedDomain::from_response(response))
    }

    pub(crate) async fn domain_update(&self, input: UpdateDomain) -> Result<(), Error> {
        if !input.is_valid() {
            return Err(invalid_request(
                DOMAIN_UPDATE.operation(),
                "Domain update fields cannot be empty",
            ));
        }

        self.mutate_body_ok(DOMAIN_UPDATE, &input).await
    }

    pub(crate) async fn domain_delete(&self, domain_id: DomainId) -> Result<(), Error> {
        let request = DomainDeleteRequest {
            body: DomainIdRequestBody {
                domain_id: domain_id.as_str().to_owned(),
            },
        };
        validate_generated_request(DOMAIN_DELETE, &request)?;

        self.mutate_body_ok(DOMAIN_DELETE, &request.body).await
    }

    pub(crate) async fn domains_by_application(
        &self,
        application_id: &str,
    ) -> Result<DomainCollection, Error> {
        let request = DomainByApplicationIdRequest {
            query: DomainByApplicationIdRequestQuery {
                application_id: application_id.to_owned(),
            },
        };
        validate_generated_request(DOMAIN_BY_APPLICATION_ID, &request)?;

        self.read_query_json(DOMAIN_BY_APPLICATION_ID, &request.query)
            .await
    }

    async fn read_json<T>(&self, endpoint: Endpoint) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        let response = self.send(endpoint, self.request(endpoint)).await?;

        decode_json_response(endpoint, response).await
    }

    async fn read_json_secret<T>(&self, endpoint: Endpoint) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        debug_assert_eq!(error_body_policy(endpoint), ErrorBodyPolicy::Sanitize);

        self.read_json(endpoint).await
    }

    async fn read_query_json<T, Q>(&self, endpoint: Endpoint, query: &Q) -> Result<T, Error>
    where
        T: DeserializeOwned,
        Q: Serialize + ?Sized,
    {
        let builder = self.request(endpoint).query(query);
        let response = self.send(endpoint, builder).await?;

        decode_json_response(endpoint, response).await
    }

    async fn read_query_json_secret<T, Q>(&self, endpoint: Endpoint, query: &Q) -> Result<T, Error>
    where
        T: DeserializeOwned,
        Q: Serialize + ?Sized,
    {
        debug_assert_eq!(error_body_policy(endpoint), ErrorBodyPolicy::Sanitize);

        self.read_query_json(endpoint, query).await
    }

    async fn mutate_body_json<T, B>(&self, endpoint: Endpoint, body: &B) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let response = self
            .send(endpoint, self.request(endpoint).json(body))
            .await?;

        decode_json_response(endpoint, response).await
    }

    async fn mutate_body_ok<B>(&self, endpoint: Endpoint, body: &B) -> Result<(), Error>
    where
        B: Serialize + ?Sized,
    {
        let response = self
            .send(endpoint, self.request(endpoint).json(body))
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let (_, bytes) = read_bounded_response_body(endpoint, response).await?;
        if error_body_policy(endpoint) == ErrorBodyPolicy::Sanitize {
            return Err(Error::Api(sanitized_dokploy_error(status)));
        }

        Err(Error::Api(decode_dokploy_error(status, &bytes)))
    }

    async fn mutate_body_json_secret<T, B>(&self, endpoint: Endpoint, body: &B) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        debug_assert_eq!(error_body_policy(endpoint), ErrorBodyPolicy::Sanitize);

        self.mutate_body_json(endpoint, body).await
    }

    async fn mutate_body_ok_secret<B>(&self, endpoint: Endpoint, body: &B) -> Result<(), Error>
    where
        B: Serialize + ?Sized,
    {
        debug_assert_eq!(error_body_policy(endpoint), ErrorBodyPolicy::Sanitize);

        self.mutate_body_ok(endpoint, body).await
    }

    pub(crate) async fn execute_imperative(
        &self,
        request: ImperativeRequest,
    ) -> Result<serde_json::Value, Error> {
        let endpoint = endpoint_by_operation(request.operation).ok_or_else(|| {
            invalid_request(
                request.operation,
                "operation is not declared by the pinned Dokploy contract",
            )
        })?;
        validate_imperative_method(&request, endpoint)?;
        let mut builder = self.request(endpoint);

        if !request.query.is_empty() {
            builder = builder.query(&request.query);
        }
        match request.body {
            ImperativeBody::None => {}
            ImperativeBody::Json(body) => {
                builder = builder.json(&body);
            }
            ImperativeBody::Multipart(fields) => {
                let mut form = reqwest::multipart::Form::new();
                for field in fields {
                    form = match field {
                        MultipartField::Text { name, value } => form.text(name, value),
                        MultipartField::File { name, path } => form
                            .file(name, path)
                            .await
                            .map_err(|source| Error::Request {
                                operation: request.operation,
                                source: source.into(),
                            })?,
                    };
                }
                builder = builder.multipart(form);
            }
        }

        let response = self.send(endpoint, builder).await?;

        decode_raw_response(endpoint, response).await
    }

    fn request(&self, endpoint: Endpoint) -> RequestBuilder {
        let url = operation_url(&self.inner.api.base_url, endpoint);

        self.inner
            .api
            .client
            .request(reqwest_method(endpoint.method()), url)
    }

    async fn send(&self, endpoint: Endpoint, builder: RequestBuilder) -> Result<Response, Error> {
        builder
            .send()
            .await
            .map_err(|source| transport_error(endpoint, source))
    }
}

fn validate_generated_request(
    endpoint: Endpoint,
    request: &impl dokploy_api::GeneratedRequest,
) -> Result<(), Error> {
    validate_request(request).map_err(|source| Error::InvalidRequest {
        operation: endpoint.operation(),
        source,
    })
}

fn validate_imperative_method(
    request: &ImperativeRequest,
    endpoint: Endpoint,
) -> Result<(), Error> {
    let matches_contract = matches!(
        (request.method, endpoint.method()),
        (ImperativeMethod::Get, EndpointMethod::Get)
            | (ImperativeMethod::Post, EndpointMethod::Post)
    );
    if matches_contract {
        return Ok(());
    }

    Err(invalid_request(
        request.operation,
        "HTTP method does not match the pinned Dokploy contract",
    ))
}

fn invalid_request(operation: &'static str, message: &'static str) -> Error {
    Error::InvalidRequest {
        operation,
        source: anyhow::anyhow!(message),
    }
}

fn post_mutation_proof_unknown(endpoint: Endpoint) -> Error {
    Error::OutcomeUnknown {
        operation: endpoint.operation(),
        source: anyhow::anyhow!("post-mutation identity proof failed"),
    }
}

fn operation_url(base_url: &Url, endpoint: Endpoint) -> Url {
    let mut url = base_url.clone();
    url.path_segments_mut()
        .expect("a validated base URL always supports path segments")
        .push(endpoint.operation());

    url
}

fn reqwest_method(method: EndpointMethod) -> reqwest::Method {
    match method {
        EndpointMethod::Delete => reqwest::Method::DELETE,
        EndpointMethod::Get => reqwest::Method::GET,
        EndpointMethod::Head => reqwest::Method::HEAD,
        EndpointMethod::Options => reqwest::Method::OPTIONS,
        EndpointMethod::Patch => reqwest::Method::PATCH,
        EndpointMethod::Post => reqwest::Method::POST,
        EndpointMethod::Put => reqwest::Method::PUT,
        EndpointMethod::Trace => reqwest::Method::TRACE,
    }
}

fn transport_error(endpoint: Endpoint, source: reqwest::Error) -> Error {
    match endpoint.method() {
        EndpointMethod::Delete
        | EndpointMethod::Patch
        | EndpointMethod::Post
        | EndpointMethod::Put => Error::OutcomeUnknown {
            operation: endpoint.operation(),
            source: source.into(),
        },
        EndpointMethod::Get
        | EndpointMethod::Head
        | EndpointMethod::Options
        | EndpointMethod::Trace => Error::Request {
            operation: endpoint.operation(),
            source: source.into(),
        },
    }
}

async fn decode_json_response<T>(endpoint: Endpoint, response: Response) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    let (status, bytes) = read_bounded_response_body(endpoint, response).await?;

    if status.is_success() {
        return decode_success_json(endpoint, &bytes);
    }
    if error_body_policy(endpoint) == ErrorBodyPolicy::Sanitize {
        return Err(Error::Api(sanitized_dokploy_error(status)));
    }

    Err(Error::Api(decode_dokploy_error(status, &bytes)))
}

async fn decode_raw_response(
    endpoint: Endpoint,
    response: Response,
) -> Result<serde_json::Value, Error> {
    let (status, bytes) = read_bounded_response_body(endpoint, response).await?;

    if !status.is_success() {
        if error_body_policy(endpoint) == ErrorBodyPolicy::Sanitize {
            return Err(Error::Api(sanitized_dokploy_error(status)));
        }
        return Err(Error::Api(decode_dokploy_error(status, &bytes)));
    }
    if bytes.is_empty() {
        return Ok(serde_json::Value::Null);
    }

    decode_success_json(endpoint, &bytes)
}

async fn read_bounded_response_body(
    endpoint: Endpoint,
    mut response: Response,
) -> Result<(StatusCode, Zeroizing<Vec<u8>>), Error> {
    let status = response.status();
    let declared_length = response.content_length();
    if declared_length.is_some_and(|length| length > MAX_JSON_RESPONSE_BYTES as u64) {
        return Err(response_body_too_large(endpoint, status));
    }
    let capacity = declared_length
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or_default()
        .min(MAX_JSON_RESPONSE_BYTES);
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));

    loop {
        let chunk = response.chunk().await.map_err(|source| {
            if status.is_success() {
                transport_error(endpoint, source)
            } else {
                Error::Api(sanitized_dokploy_error(status))
            }
        })?;
        let Some(chunk) = chunk else {
            break;
        };
        let Some(length) = bytes.len().checked_add(chunk.len()) else {
            return Err(response_body_too_large(endpoint, status));
        };
        if length > MAX_JSON_RESPONSE_BYTES {
            return Err(response_body_too_large(endpoint, status));
        }
        bytes.extend_from_slice(&chunk);
    }

    Ok((status, bytes))
}

fn decode_success_json<T>(endpoint: Endpoint, bytes: &[u8]) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    serde_json::from_slice(bytes).map_err(|source| match endpoint.method() {
        EndpointMethod::Delete
        | EndpointMethod::Patch
        | EndpointMethod::Post
        | EndpointMethod::Put => Error::OutcomeUnknown {
            operation: endpoint.operation(),
            source: source.into(),
        },
        EndpointMethod::Get
        | EndpointMethod::Head
        | EndpointMethod::Options
        | EndpointMethod::Trace => Error::Decode {
            operation: endpoint.operation(),
            source,
        },
    })
}

fn validate_external_selector_collection<T, I, V>(
    endpoint: Endpoint,
    items: &[T],
    identity: I,
    valid: V,
) -> Result<(), Error>
where
    I: Fn(&T) -> &str,
    V: Fn(&T) -> bool,
{
    if items.len() > EXTERNAL_SELECTOR_ITEM_LIMIT {
        return Err(Error::UnexpectedResponse {
            operation: endpoint.operation(),
        });
    }

    let mut identities = HashSet::with_capacity(items.len());
    for item in items {
        let item_id = identity(item);
        if item_id.is_empty() || !valid(item) || !identities.insert(item_id) {
            return Err(Error::UnexpectedResponse {
                operation: endpoint.operation(),
            });
        }
    }

    Ok(())
}

fn response_body_too_large(endpoint: Endpoint, status: StatusCode) -> Error {
    if !status.is_success() {
        return Error::Api(sanitized_dokploy_error(status));
    }

    match endpoint.method() {
        EndpointMethod::Delete
        | EndpointMethod::Patch
        | EndpointMethod::Post
        | EndpointMethod::Put => Error::OutcomeUnknown {
            operation: endpoint.operation(),
            source: anyhow::anyhow!("successful response body exceeded the SDK byte limit"),
        },
        EndpointMethod::Get
        | EndpointMethod::Head
        | EndpointMethod::Options
        | EndpointMethod::Trace => Error::UnexpectedResponse {
            operation: endpoint.operation(),
        },
    }
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    issues: Vec<ErrorIssue>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ErrorIssue {
    Detailed { message: String },
    Message(String),
}

fn decode_dokploy_error(status: StatusCode, bytes: &[u8]) -> DokployError {
    let body = serde_json::from_slice::<ErrorEnvelope>(bytes).ok();
    let code = body
        .as_ref()
        .and_then(|body| body.code.clone())
        .filter(|code| !code.trim().is_empty())
        .unwrap_or_else(|| fallback_error_code(status).to_owned());
    let message = body
        .as_ref()
        .and_then(|body| body.message.clone())
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Dokploy request failed")
                .to_owned()
        });
    let issues = body
        .map(|body| {
            body.issues
                .into_iter()
                .map(|issue| match issue {
                    ErrorIssue::Detailed { message } | ErrorIssue::Message(message) => message,
                })
                .collect()
        })
        .unwrap_or_default();

    DokployError::new(status.as_u16(), code, message, issues)
}

fn sanitized_dokploy_error(status: StatusCode) -> DokployError {
    let message = status
        .canonical_reason()
        .unwrap_or("Dokploy request failed")
        .to_owned();

    DokployError::new(
        status.as_u16(),
        fallback_error_code(status).to_owned(),
        message,
        Vec::new(),
    )
}

fn fallback_error_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "BAD_REQUEST",
        StatusCode::UNAUTHORIZED => "UNAUTHORIZED",
        StatusCode::FORBIDDEN => "FORBIDDEN",
        StatusCode::NOT_FOUND => "NOT_FOUND",
        StatusCode::CONFLICT => "CONFLICT",
        StatusCode::UNPROCESSABLE_ENTITY => "UNPROCESSABLE_ENTITY",
        StatusCode::TOO_MANY_REQUESTS => "TOO_MANY_REQUESTS",
        status if status.is_server_error() => "SERVER_ERROR",
        _ => "DOKPLOY_ERROR",
    }
}

/// Builder for a [`Dokploy`] client.
#[derive(Default)]
pub struct DokployBuilder {
    url: Option<String>,
    api_key: Option<Zeroizing<String>>,
}

impl DokployBuilder {
    /// Sets the Dokploy instance URL.
    #[must_use]
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Sets the Dokploy API key.
    #[must_use]
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(Zeroizing::new(api_key.into()));
        self
    }

    /// Validates the configuration and constructs the client.
    pub fn build(self) -> Result<Dokploy, BuildError> {
        let base_url = normalize_base_url(self.url.as_deref().ok_or(BuildError::MissingUrl)?)?;
        let api_key = self.api_key.ok_or(BuildError::MissingApiKey)?;

        if api_key.trim().is_empty() {
            return Err(BuildError::EmptyApiKey);
        }

        let mut header_value =
            HeaderValue::from_str(&api_key).map_err(BuildError::InvalidApiKey)?;
        header_value.set_sensitive(true);

        let mut headers = HeaderMap::new();
        headers.insert(API_KEY_HEADER, header_value);

        let retry_host = base_url
            .host_str()
            .expect("a validated base URL always has a host")
            .to_owned();
        let retry_policy = reqwest::retry::for_host(retry_host)
            .max_retries_per_request(2)
            .classify_fn(|request| {
                let is_safe_read = request.method() == reqwest::Method::GET;
                let is_transient_status = request.status().is_some_and(|status| {
                    matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
                });

                if is_safe_read && (request.error().is_some() || is_transient_status) {
                    request.retryable()
                } else {
                    request.success()
                }
            });

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(USER_AGENT)
            .timeout(DEFAULT_TIMEOUT)
            .retry(retry_policy)
            .build()
            .map_err(BuildError::HttpClient)?;
        let api = DokployApiClient::with_client(base_url.as_str(), client)
            .map_err(|_| BuildError::InvalidUrl)?;

        Ok(Dokploy {
            inner: Arc::new(ClientInner { api }),
        })
    }
}

fn normalize_base_url(value: &str) -> Result<Url, BuildError> {
    let mut url = Url::parse(value).map_err(|_| BuildError::InvalidUrl)?;

    if !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BuildError::InvalidUrl);
    }

    let trimmed_path = url.path().trim_end_matches('/');
    let api_path = if trimmed_path.ends_with("/api") {
        trimmed_path.to_owned()
    } else if trimmed_path.is_empty() {
        "/api".to_owned()
    } else {
        format!("{trimmed_path}/api")
    };
    url.set_path(&api_path);

    Ok(url)
}
