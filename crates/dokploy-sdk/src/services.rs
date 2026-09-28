use crate::{
    ApplicationDetails, ApplicationId, Dokploy, Error, PostgresDetails, PostgresId, ProjectDetails,
    ProjectId, ProjectTopology,
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
}
