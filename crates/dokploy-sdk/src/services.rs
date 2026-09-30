use crate::{
    ApplicationCollection, ApplicationDetails, ApplicationEnvironmentDocument, ApplicationId,
    ComposeCollection, ComposeDetails, ComposeId, ComposeVolumePolicy, Dokploy, DomainCollection,
    DomainDetails, DomainId, EnvironmentCollection, EnvironmentDetails, EnvironmentId, Error,
    LibSqlCollection, LibSqlDetails, LibSqlId, MariaDbCollection, MariaDbDetails, MariaDbId,
    MongoCollection, MongoDetails, MongoId, MountCollection, MountDetails, MountId,
    MySqlCollection, MySqlDetails, MySqlId, PortCollection, PortDetails, PortId,
    PostgresCollection, PostgresDetails, PostgresId, ProjectDetails, ProjectId, ProjectTopology,
    RedirectCollection, RedirectDetails, RedirectId, RedisCollection, RedisDetails, RedisId,
    ScheduleCollection, ScheduleDetails, ScheduleId, ScheduleTarget, SecurityCollection,
    SecurityDetails, SecurityId, ServiceTarget,
};
use crate::{
    ChangeLibSqlPassword, ChangeMariaDbPassword, ChangeMongoPassword, ChangeMySqlPassword,
    CreateApplication, CreateCompose, CreateDomain, CreateEnvironment, CreateLibSql, CreateMariaDb,
    CreateMongo, CreateMount, CreateMySql, CreatePort, CreatePostgres, CreateProject,
    CreateRedirect, CreateRedis, CreateSchedule, CreateSecurity, CreatedApplication,
    CreatedCompose, CreatedDomain, CreatedEnvironment, CreatedLibSql, CreatedMariaDb, CreatedMongo,
    CreatedMount, CreatedMySql, CreatedPort, CreatedPostgres, CreatedProject, CreatedRedirect,
    CreatedRedis, CreatedSchedule, CreatedSecurity, UpdateApplication, UpdateCompose, UpdateDomain,
    UpdateEnvironment, UpdateLibSql, UpdateMariaDb, UpdateMongo, UpdateMount, UpdateMySql,
    UpdatePort, UpdatePostgres, UpdateProject, UpdateRedirect, UpdateRedis, UpdateSchedule,
    UpdateSecurity,
};

/// Read operations for Dokploy projects.
pub struct Projects<'a> {
    client: &'a Dokploy,
}

impl<'a> Projects<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads all projects and their resource topology from fresh remote state.
    pub async fn all(&self) -> Result<ProjectTopology, Error> {
        self.client.project_all().await
    }

    /// Reads one project from fresh remote state.
    pub async fn get(&self, project_id: ProjectId) -> Result<ProjectDetails, Error> {
        self.client.project_get(project_id.as_str()).await
    }

    /// Creates a project and returns both identities created by Dokploy.
    pub async fn create(&self, input: CreateProject) -> Result<CreatedProject, Error> {
        self.client.project_create(input).await
    }

    /// Updates the owned project description.
    pub async fn update(&self, input: UpdateProject) -> Result<(), Error> {
        self.client.project_update(input).await
    }

    /// Permanently removes one project and its Dokploy-owned descendants.
    pub async fn delete(&self, project_id: ProjectId) -> Result<(), Error> {
        self.client.project_delete(project_id).await
    }
}

/// Read operations for Dokploy applications.
pub struct Applications<'a> {
    client: &'a Dokploy,
}

impl<'a> Applications<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one application from fresh remote state.
    pub async fn get(&self, application_id: ApplicationId) -> Result<ApplicationDetails, Error> {
        self.client.application_get(application_id.as_str()).await
    }

    /// Reads every application in one environment from fresh paginated state.
    pub async fn by_environment(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<ApplicationCollection, Error> {
        self.client
            .applications_by_environment(environment_id.as_str())
            .await
    }

    /// Creates a minimal application record for later explicit configuration.
    pub async fn create(&self, input: CreateApplication) -> Result<CreatedApplication, Error> {
        self.client.application_create(input).await
    }

    /// Writes an explicit subset of owned application configuration.
    pub async fn update(&self, input: UpdateApplication) -> Result<(), Error> {
        self.client.application_update(input).await
    }

    /// Requests deployment of the current application configuration.
    pub async fn deploy(&self, application_id: ApplicationId) -> Result<(), Error> {
        self.client.application_deploy(application_id).await
    }

    /// Permanently removes one application.
    pub async fn delete(&self, application_id: ApplicationId) -> Result<(), Error> {
        self.client.application_delete(application_id).await
    }

    /// Reads the transient raw environment document for a preservation merge.
    pub async fn environment(
        &self,
        application_id: ApplicationId,
    ) -> Result<ApplicationEnvironmentDocument, Error> {
        self.client.application_environment(application_id).await
    }
}

/// Read operations for Dokploy environments.
pub struct Environments<'a> {
    client: &'a Dokploy,
}

impl<'a> Environments<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one environment from fresh remote state.
    pub async fn get(&self, environment_id: EnvironmentId) -> Result<EnvironmentDetails, Error> {
        self.client.environment_get(environment_id.as_str()).await
    }

    /// Reads the fresh environment collection scoped to one project.
    pub async fn by_project(&self, project_id: ProjectId) -> Result<EnvironmentCollection, Error> {
        self.client
            .environments_by_project(project_id.as_str())
            .await
    }

    /// Creates an environment under one project.
    pub async fn create(&self, input: CreateEnvironment) -> Result<CreatedEnvironment, Error> {
        self.client.environment_create(input).await
    }

    /// Updates the owned environment description.
    pub async fn update(&self, input: UpdateEnvironment) -> Result<(), Error> {
        self.client.environment_update(input).await
    }

    /// Permanently removes one environment.
    pub async fn delete(&self, environment_id: EnvironmentId) -> Result<(), Error> {
        self.client.environment_delete(environment_id).await
    }
}

/// Read and mutation operations for Dokploy Compose records.
pub struct Composes<'a> {
    client: &'a Dokploy,
}

impl<'a> Composes<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one Compose record from fresh remote state.
    pub async fn get(&self, compose_id: ComposeId) -> Result<ComposeDetails, Error> {
        self.client.compose_get(compose_id.as_str()).await
    }

    /// Reads every Compose record in one environment from fresh paginated state.
    pub async fn by_environment(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<ComposeCollection, Error> {
        self.client
            .composes_by_environment(environment_id.as_str())
            .await
    }

    /// Creates an undeployed raw Compose record and validates its returned identity.
    pub async fn create(&self, input: CreateCompose) -> Result<CreatedCompose, Error> {
        self.client.compose_create(input).await
    }

    /// Writes the selected owned Compose fields without deploying the record.
    pub async fn update(&self, input: UpdateCompose) -> Result<(), Error> {
        self.client.compose_update(input).await
    }

    /// Permanently removes one Compose record with an explicit volume policy.
    pub async fn delete(
        &self,
        compose_id: ComposeId,
        volume_policy: ComposeVolumePolicy,
    ) -> Result<(), Error> {
        self.client.compose_delete(compose_id, volume_policy).await
    }
}

/// Read and mutation operations for Dokploy mounts.
pub struct Mounts<'a> {
    client: &'a Dokploy,
}

impl<'a> Mounts<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one Mount from fresh remote state.
    pub async fn get(&self, mount_id: MountId) -> Result<MountDetails, Error> {
        self.client.mount_get(mount_id.as_str()).await
    }

    /// Reads the complete bounded Mount collection for one exact target.
    pub async fn by_target(&self, target: ServiceTarget) -> Result<MountCollection, Error> {
        self.client.mounts_by_target(&target).await
    }

    /// Creates a Mount and validates the complete identity returned by Dokploy.
    pub async fn create(&self, input: CreateMount) -> Result<CreatedMount, Error> {
        self.client.mount_create(input).await
    }

    /// Writes an explicit subset of owned Mount fields.
    pub async fn update(&self, input: UpdateMount) -> Result<(), Error> {
        self.client.mount_update(input).await
    }

    /// Permanently removes one Mount by physical identity.
    pub async fn delete(&self, mount_id: MountId) -> Result<(), Error> {
        self.client.mount_delete(mount_id).await
    }
}

/// Read and mutation operations for application Ports.
pub struct Ports<'a> {
    client: &'a Dokploy,
}

impl<'a> Ports<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one Port from fresh remote state and validates its parent identity.
    pub async fn get(&self, port_id: PortId) -> Result<PortDetails, Error> {
        self.client.port_get(port_id.as_str()).await
    }

    /// Reads the authoritative bounded Port collection for one application.
    pub async fn by_application(
        &self,
        application_id: ApplicationId,
    ) -> Result<PortCollection, Error> {
        self.client.ports_by_application(&application_id).await
    }

    /// Creates a Port and validates the complete identity returned by Dokploy.
    pub async fn create(&self, input: CreatePort) -> Result<CreatedPort, Error> {
        self.client.port_create(input).await
    }

    /// Replaces every mutable field of one Port without deploying its application.
    pub async fn update(&self, input: UpdatePort) -> Result<(), Error> {
        self.client.port_update(input).await
    }

    /// Permanently removes one Port by physical identity.
    pub async fn delete(&self, port_id: PortId) -> Result<(), Error> {
        self.client.port_delete(port_id).await
    }
}

/// Read and mutation operations for application Redirects.
pub struct Redirects<'a> {
    client: &'a Dokploy,
}

impl<'a> Redirects<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one Redirect and requires agreement with its application collection.
    pub async fn get(&self, redirect_id: RedirectId) -> Result<RedirectDetails, Error> {
        self.client.redirect_get(redirect_id.as_str()).await
    }

    /// Reads the authoritative bounded Redirect collection for one application.
    pub async fn by_application(
        &self,
        application_id: ApplicationId,
    ) -> Result<RedirectCollection, Error> {
        self.client.redirects_by_application(&application_id).await
    }

    /// Creates a Redirect and discovers exactly one new matching identity.
    pub async fn create(&self, input: CreateRedirect) -> Result<CreatedRedirect, Error> {
        self.client.redirect_create(input).await
    }

    /// Replaces every mutable field without changing the Redirect parent.
    pub async fn update(&self, input: UpdateRedirect) -> Result<(), Error> {
        self.client.redirect_update(input).await
    }

    /// Permanently removes one Redirect by physical identity.
    pub async fn delete(&self, redirect_id: RedirectId) -> Result<(), Error> {
        self.client.redirect_delete(redirect_id).await
    }
}

/// Read and mutation operations for application basic-auth Security entries.
pub struct Security<'a> {
    client: &'a Dokploy,
}

impl<'a> Security<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one Security entry and requires agreement with its application collection.
    pub async fn get(&self, security_id: SecurityId) -> Result<SecurityDetails, Error> {
        self.client.security_get(security_id.as_str()).await
    }

    /// Reads the authoritative bounded Security collection for one application.
    pub async fn by_application(
        &self,
        application_id: ApplicationId,
    ) -> Result<SecurityCollection, Error> {
        self.client.security_by_application(&application_id).await
    }

    /// Creates a Security entry and discovers exactly one new matching identity.
    pub async fn create(&self, input: CreateSecurity) -> Result<CreatedSecurity, Error> {
        self.client.security_create(input).await
    }

    /// Replaces the complete username and password without changing the parent.
    pub async fn update(&self, input: UpdateSecurity) -> Result<(), Error> {
        self.client.security_update(input).await
    }

    /// Permanently removes one Security entry by physical identity.
    pub async fn delete(&self, security_id: SecurityId) -> Result<(), Error> {
        self.client.security_delete(security_id).await
    }
}

/// Read and mutation operations for application and Compose Schedules.
pub struct Schedules<'a> {
    client: &'a Dokploy,
}

impl<'a> Schedules<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one Schedule and requires agreement with its target collection.
    pub async fn get(&self, schedule_id: ScheduleId) -> Result<ScheduleDetails, Error> {
        self.client.schedule_get(schedule_id.as_str()).await
    }

    /// Reads the authoritative bounded Schedule collection for one exact target.
    pub async fn by_target(&self, target: ScheduleTarget) -> Result<ScheduleCollection, Error> {
        self.client.schedules_by_target(&target).await
    }

    /// Creates a Schedule and validates its returned and collected identity.
    pub async fn create(&self, input: CreateSchedule) -> Result<CreatedSchedule, Error> {
        self.client.schedule_create(input).await
    }

    /// Replaces every safe mutable field without changing target or service.
    pub async fn update(&self, input: UpdateSchedule) -> Result<(), Error> {
        self.client.schedule_update(input).await
    }

    /// Permanently removes one Schedule by physical identity.
    pub async fn delete(&self, schedule_id: ScheduleId) -> Result<(), Error> {
        self.client.schedule_delete(schedule_id).await
    }
}

/// Read and mutation operations for Dokploy LibSQL databases.
pub struct LibSql<'a> {
    client: &'a Dokploy,
}

impl<'a> LibSql<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one LibSQL database from fresh remote state.
    pub async fn get(&self, libsql_id: LibSqlId) -> Result<LibSqlDetails, Error> {
        self.client.libsql_get(libsql_id.as_str()).await
    }

    /// Reads LibSQL databases from one exact project/environment topology.
    pub async fn by_environment(
        &self,
        project_id: ProjectId,
        environment_id: EnvironmentId,
    ) -> Result<LibSqlCollection, Error> {
        self.client
            .libsql_by_environment(project_id.as_str(), environment_id.as_str())
            .await
    }

    /// Creates a LibSQL database and authoritatively discovers its identity.
    pub async fn create(&self, input: CreateLibSql) -> Result<CreatedLibSql, Error> {
        self.client.libsql_create(input).await
    }

    /// Writes an explicit subset of owned non-secret LibSQL fields.
    pub async fn update(&self, input: UpdateLibSql) -> Result<(), Error> {
        self.client.libsql_update(input).await
    }

    /// Rotates the LibSQL database password/authentication token.
    pub async fn change_password(&self, input: ChangeLibSqlPassword) -> Result<(), Error> {
        self.client.libsql_change_password(input).await
    }

    /// Removes one LibSQL database by physical identity.
    pub async fn delete(&self, libsql_id: LibSqlId) -> Result<(), Error> {
        self.client.libsql_delete(libsql_id).await
    }
}

/// Read and mutation operations for Dokploy MongoDB databases.
pub struct Mongo<'a> {
    client: &'a Dokploy,
}

impl<'a> Mongo<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one MongoDB database from fresh remote state.
    pub async fn get(&self, mongo_id: MongoId) -> Result<MongoDetails, Error> {
        self.client.mongo_get(mongo_id.as_str()).await
    }

    /// Reads every MongoDB database in one environment from fresh paginated state.
    pub async fn by_environment(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<MongoCollection, Error> {
        self.client
            .mongo_by_environment(environment_id.as_str())
            .await
    }

    /// Creates a MongoDB database with write-only credentials.
    pub async fn create(&self, input: CreateMongo) -> Result<CreatedMongo, Error> {
        self.client.mongo_create(input).await
    }

    /// Writes an explicit subset of owned non-secret MongoDB fields.
    pub async fn update(&self, input: UpdateMongo) -> Result<(), Error> {
        self.client.mongo_update(input).await
    }

    /// Rotates the MongoDB database password.
    pub async fn change_password(&self, input: ChangeMongoPassword) -> Result<(), Error> {
        self.client.mongo_change_password(input).await
    }

    /// Removes one MongoDB database by physical identity.
    pub async fn delete(&self, mongo_id: MongoId) -> Result<(), Error> {
        self.client.mongo_delete(mongo_id).await
    }
}

/// Read and mutation operations for Dokploy MariaDB databases.
pub struct MariaDb<'a> {
    client: &'a Dokploy,
}

impl<'a> MariaDb<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one MariaDB database from fresh remote state.
    pub async fn get(&self, mariadb_id: MariaDbId) -> Result<MariaDbDetails, Error> {
        self.client.mariadb_get(mariadb_id.as_str()).await
    }

    /// Reads every MariaDB database in one environment from fresh paginated state.
    pub async fn by_environment(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<MariaDbCollection, Error> {
        self.client
            .mariadb_by_environment(environment_id.as_str())
            .await
    }

    /// Creates a MariaDB database with a required user password and optional root password.
    pub async fn create(&self, input: CreateMariaDb) -> Result<CreatedMariaDb, Error> {
        self.client.mariadb_create(input).await
    }

    /// Writes an explicit subset of owned non-secret MariaDB fields.
    pub async fn update(&self, input: UpdateMariaDb) -> Result<(), Error> {
        self.client.mariadb_update(input).await
    }

    /// Rotates either the configured user or root password explicitly.
    pub async fn change_password(&self, input: ChangeMariaDbPassword) -> Result<(), Error> {
        self.client.mariadb_change_password(input).await
    }

    /// Removes one MariaDB database by physical identity.
    pub async fn delete(&self, mariadb_id: MariaDbId) -> Result<(), Error> {
        self.client.mariadb_delete(mariadb_id).await
    }
}

/// Read and mutation operations for Dokploy MySQL databases.
pub struct MySql<'a> {
    client: &'a Dokploy,
}

impl<'a> MySql<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one MySQL database from fresh remote state.
    pub async fn get(&self, mysql_id: MySqlId) -> Result<MySqlDetails, Error> {
        self.client.mysql_get(mysql_id.as_str()).await
    }

    /// Reads every MySQL database in one environment from fresh paginated state.
    pub async fn by_environment(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<MySqlCollection, Error> {
        self.client
            .mysql_by_environment(environment_id.as_str())
            .await
    }

    /// Creates a MySQL database with separate write-only user and root credentials.
    pub async fn create(&self, input: CreateMySql) -> Result<CreatedMySql, Error> {
        self.client.mysql_create(input).await
    }

    /// Writes an explicit subset of owned non-secret MySQL fields.
    pub async fn update(&self, input: UpdateMySql) -> Result<(), Error> {
        self.client.mysql_update(input).await
    }

    /// Rotates either the configured user or root password explicitly.
    pub async fn change_password(&self, input: ChangeMySqlPassword) -> Result<(), Error> {
        self.client.mysql_change_password(input).await
    }

    /// Removes one MySQL database by physical identity.
    pub async fn delete(&self, mysql_id: MySqlId) -> Result<(), Error> {
        self.client.mysql_delete(mysql_id).await
    }
}

/// Read operations for Dokploy Postgres databases.
pub struct Postgres<'a> {
    client: &'a Dokploy,
}

impl<'a> Postgres<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one Postgres database from fresh remote state.
    pub async fn get(&self, postgres_id: PostgresId) -> Result<PostgresDetails, Error> {
        self.client.postgres_get(postgres_id.as_str()).await
    }

    /// Reads every Postgres database in one environment from fresh paginated state.
    pub async fn by_environment(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<PostgresCollection, Error> {
        self.client
            .postgres_by_environment(environment_id.as_str())
            .await
    }

    /// Creates a Postgres database with write-only credentials.
    pub async fn create(&self, input: CreatePostgres) -> Result<CreatedPostgres, Error> {
        self.client.postgres_create(input).await
    }

    /// Updates an explicit subset of owned Postgres fields.
    pub async fn update(&self, input: UpdatePostgres) -> Result<(), Error> {
        self.client.postgres_update(input).await
    }

    /// Permanently removes one Postgres database.
    pub async fn delete(&self, postgres_id: PostgresId) -> Result<(), Error> {
        self.client.postgres_delete(postgres_id).await
    }
}

/// Read operations for Dokploy Redis databases.
pub struct Redis<'a> {
    client: &'a Dokploy,
}

impl<'a> Redis<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one Redis database from fresh remote state.
    pub async fn get(&self, redis_id: RedisId) -> Result<RedisDetails, Error> {
        self.client.redis_get(redis_id.as_str()).await
    }

    /// Reads every Redis database in one environment from fresh paginated state.
    pub async fn by_environment(
        &self,
        environment_id: EnvironmentId,
    ) -> Result<RedisCollection, Error> {
        self.client
            .redis_by_environment(environment_id.as_str())
            .await
    }

    /// Creates a Redis database with a write-only password.
    pub async fn create(&self, input: CreateRedis) -> Result<CreatedRedis, Error> {
        self.client.redis_create(input).await
    }

    /// Rotates the write-only Redis password.
    pub async fn update(&self, input: UpdateRedis) -> Result<(), Error> {
        self.client.redis_update(input).await
    }

    /// Permanently removes one Redis database.
    pub async fn delete(&self, redis_id: RedisId) -> Result<(), Error> {
        self.client.redis_delete(redis_id).await
    }
}

/// Read operations for Dokploy application domains.
pub struct Domains<'a> {
    client: &'a Dokploy,
}

impl<'a> Domains<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Reads one domain from fresh remote state.
    pub async fn get(&self, domain_id: DomainId) -> Result<DomainDetails, Error> {
        self.client.domain_get(domain_id.as_str()).await
    }

    /// Reads every domain attached to one application from fresh remote state.
    pub async fn by_application(
        &self,
        application_id: ApplicationId,
    ) -> Result<DomainCollection, Error> {
        self.client
            .domains_by_application(application_id.as_str())
            .await
    }

    /// Attaches a domain to an application.
    pub async fn create(&self, input: CreateDomain) -> Result<CreatedDomain, Error> {
        self.client.domain_create(input).await
    }

    /// Updates the Domain host while preserving its application attachment.
    pub async fn update(&self, input: UpdateDomain) -> Result<(), Error> {
        self.client.domain_update(input).await
    }

    /// Permanently removes one domain attachment.
    pub async fn delete(&self, domain_id: DomainId) -> Result<(), Error> {
        self.client.domain_delete(domain_id).await
    }
}
