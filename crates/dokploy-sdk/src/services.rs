use crate::{
    ApplicationCollection, ApplicationDetails, ApplicationEnvironmentDocument, ApplicationId,
    Dokploy, DomainCollection, DomainDetails, DomainId, EnvironmentCollection, EnvironmentDetails,
    EnvironmentId, Error, PostgresCollection, PostgresDetails, PostgresId, ProjectDetails,
    ProjectId, ProjectTopology, RedisCollection, RedisDetails, RedisId,
};
use crate::{
    CreateApplication, CreateDomain, CreateEnvironment, CreatePostgres, CreateProject, CreateRedis,
    CreatedApplication, CreatedDomain, CreatedEnvironment, CreatedPostgres, CreatedProject,
    CreatedRedis, UpdateApplication, UpdateDomain, UpdateEnvironment, UpdatePostgres,
    UpdateProject, UpdateRedis,
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
}
