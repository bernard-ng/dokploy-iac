use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};
use std::fmt;
use zeroize::Zeroizing;

/// A mutation value that distinguishes omission from an explicit JSON null.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Nullable<T> {
    /// Explicitly clear the remote field.
    Null,
    /// Set the remote field to a concrete value.
    Value(T),
}

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates an identifier from the opaque value returned by Dokploy.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier!(ApplicationId);
identifier!(EnvironmentId);
identifier!(MariaDbId);
identifier!(MongoId);
identifier!(MySqlId);
identifier!(PostgresId);
identifier!(ProjectId);
identifier!(RedisId);
identifier!(DomainId);
identifier!(ServerId);

/// Inputs required to create one Dokploy project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateProject {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

impl CreateProject {
    /// Creates project input with no managed description.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
        }
    }

    /// Sets the initial project description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Physical identities returned by Dokploy when a project is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedProject {
    project_id: ProjectId,
    default_environment_id: EnvironmentId,
    default_environment_name: String,
}

impl CreatedProject {
    pub(crate) fn from_response(response: ProjectCreateResponse) -> Self {
        Self {
            project_id: response.project.project_id,
            default_environment_id: response.environment.environment_id,
            default_environment_name: response.environment.name,
        }
    }

    /// Returns the new project identity.
    #[must_use]
    pub const fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    /// Returns the identity of the default environment created with the project.
    #[must_use]
    pub const fn default_environment_id(&self) -> &EnvironmentId {
        &self.default_environment_id
    }

    /// Returns the name of the default environment created with the project.
    #[must_use]
    pub fn default_environment_name(&self) -> &str {
        &self.default_environment_name
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectCreateResponse {
    project: CreatedProjectResponse,
    environment: CreatedEnvironmentResponse,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedProjectResponse {
    project_id: ProjectId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedEnvironmentResponse {
    environment_id: EnvironmentId,
    name: String,
}

/// Inputs required to create one Dokploy environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateEnvironment {
    pub(crate) name: String,
    pub(crate) project_id: ProjectId,
    pub(crate) description: Option<String>,
}

impl CreateEnvironment {
    /// Creates environment input with no managed description.
    #[must_use]
    pub fn new(name: impl Into<String>, project_id: ProjectId) -> Self {
        Self {
            name: name.into(),
            project_id,
            description: None,
        }
    }

    /// Sets the initial environment description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Physical identity returned by Dokploy when an environment is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedEnvironment {
    environment_id: EnvironmentId,
}

impl CreatedEnvironment {
    pub(crate) fn from_response(response: EnvironmentCreateResponse) -> Self {
        Self {
            environment_id: response.environment_id,
        }
    }

    /// Returns the new environment identity.
    #[must_use]
    pub const fn environment_id(&self) -> &EnvironmentId {
        &self.environment_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnvironmentCreateResponse {
    environment_id: EnvironmentId,
}

/// The managed project description written by one update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProject {
    project_id: ProjectId,
    description: Nullable<String>,
}

impl UpdateProject {
    /// Selects the exact project description state.
    #[must_use]
    pub fn new(project_id: ProjectId, description: Nullable<String>) -> Self {
        Self {
            project_id,
            description,
        }
    }
}

/// The managed environment description written by one update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateEnvironment {
    environment_id: EnvironmentId,
    description: Nullable<String>,
}

impl UpdateEnvironment {
    /// Selects the exact environment description state.
    #[must_use]
    pub fn new(environment_id: EnvironmentId, description: Nullable<String>) -> Self {
        Self {
            environment_id,
            description,
        }
    }
}

/// Inputs required to create one Dokploy application record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateApplication {
    pub(crate) name: String,
    pub(crate) environment_id: EnvironmentId,
}

impl CreateApplication {
    /// Creates minimal application input for later explicit configuration.
    #[must_use]
    pub fn new(name: impl Into<String>, environment_id: EnvironmentId) -> Self {
        Self {
            name: name.into(),
            environment_id,
        }
    }
}

/// Physical identity returned by Dokploy when an application is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedApplication {
    application_id: ApplicationId,
}

impl CreatedApplication {
    pub(crate) fn from_response(response: ApplicationCreateResponse) -> Self {
        Self {
            application_id: response.application_id,
        }
    }

    /// Returns the new application identity.
    #[must_use]
    pub const fn application_id(&self) -> &ApplicationId {
        &self.application_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplicationCreateResponse {
    application_id: ApplicationId,
}

/// A transient application environment document used only to preserve unowned entries.
pub struct ApplicationEnvironmentDocument(Zeroizing<String>);

impl ApplicationEnvironmentDocument {
    pub(crate) fn from_response(response: ApplicationEnvironmentResponse) -> Self {
        Self(Zeroizing::new(response.environment.unwrap_or_default()))
    }

    /// Transfers the transient environment document to the mutation composer.
    #[must_use]
    pub fn into_document(self) -> Zeroizing<String> {
        self.0
    }
}

impl fmt::Debug for ApplicationEnvironmentDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApplicationEnvironmentDocument([REDACTED])")
    }
}

#[derive(Deserialize)]
pub(crate) struct ApplicationEnvironmentResponse {
    #[serde(default, rename = "env")]
    environment: Option<String>,
}

/// Owned application fields written by one `application.update` request.
pub struct UpdateApplication {
    application_id: ApplicationId,
    description: Option<Nullable<String>>,
    replicas: Option<Nullable<u32>>,
    source_type: Option<Nullable<String>>,
    repository: Option<Nullable<String>>,
    branch: Option<Nullable<String>>,
    environment: Option<Nullable<Zeroizing<String>>>,
    environment_id: Option<EnvironmentId>,
}

impl UpdateApplication {
    /// Starts an application update with no fields selected.
    #[must_use]
    pub fn new(application_id: ApplicationId) -> Self {
        Self {
            application_id,
            description: None,
            replicas: None,
            source_type: None,
            repository: None,
            branch: None,
            environment: None,
            environment_id: None,
        }
    }

    /// Selects the application description.
    #[must_use]
    pub fn with_description(mut self, description: Nullable<String>) -> Self {
        self.description = Some(description);
        self
    }

    /// Selects the replica count.
    #[must_use]
    pub fn with_replicas(mut self, replicas: Nullable<u32>) -> Self {
        self.replicas = Some(replicas);
        self
    }

    /// Clears the complete source configuration.
    #[must_use]
    pub fn clearing_source(mut self) -> Self {
        self.source_type = Some(Nullable::Null);
        self.repository = Some(Nullable::Null);
        self.branch = Some(Nullable::Null);
        self
    }

    /// Selects a GitHub repository while leaving branch ownership unchanged.
    #[must_use]
    pub fn with_github_repository(mut self, repository: impl Into<String>) -> Self {
        self.source_type = Some(Nullable::Value("github".to_owned()));
        self.repository = Some(Nullable::Value(repository.into()));
        self
    }

    /// Selects the GitHub branch independently from the repository.
    #[must_use]
    pub fn with_branch(mut self, branch: Nullable<String>) -> Self {
        self.branch = Some(branch);
        self
    }

    /// Replaces or clears the complete Dokploy environment document.
    #[must_use]
    pub fn with_environment(mut self, environment: Nullable<Zeroizing<String>>) -> Self {
        self.environment = Some(environment);
        self
    }

    /// Moves the application to another physical environment.
    #[must_use]
    pub fn with_environment_id(mut self, environment_id: EnvironmentId) -> Self {
        self.environment_id = Some(environment_id);
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.application_id.as_str().is_empty()
            && (self.description.is_some()
                || self.replicas.is_some()
                || self.source_type.is_some()
                || self.repository.is_some()
                || self.branch.is_some()
                || self.environment.is_some()
                || self.environment_id.is_some())
    }
}

impl fmt::Debug for UpdateApplication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateApplication")
            .field("application_id", &self.application_id)
            .field("description", &self.description)
            .field("replicas", &self.replicas)
            .field("source_type", &self.source_type)
            .field("repository", &self.repository)
            .field("branch", &self.branch)
            .field(
                "environment",
                &self.environment.as_ref().map(|_| "[REDACTED]"),
            )
            .field("environment_id", &self.environment_id)
            .finish()
    }
}

impl Serialize for UpdateApplication {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut fields = 1;
        fields += usize::from(self.description.is_some());
        fields += usize::from(self.replicas.is_some());
        fields += usize::from(self.source_type.is_some());
        fields += usize::from(self.repository.is_some());
        fields += usize::from(self.branch.is_some());
        fields += usize::from(self.environment.is_some());
        fields += usize::from(self.environment_id.is_some());
        let mut body = serializer.serialize_struct("UpdateApplication", fields)?;
        body.serialize_field("applicationId", self.application_id.as_str())?;
        if let Some(description) = &self.description {
            body.serialize_field("description", description)?;
        }
        if let Some(replicas) = &self.replicas {
            body.serialize_field("replicas", replicas)?;
        }
        if let Some(source_type) = &self.source_type {
            body.serialize_field("sourceType", source_type)?;
        }
        if let Some(repository) = &self.repository {
            body.serialize_field("repository", repository)?;
        }
        if let Some(branch) = &self.branch {
            body.serialize_field("branch", branch)?;
        }
        if let Some(environment) = &self.environment {
            match environment {
                Nullable::Null => body.serialize_field("env", &Option::<&str>::None)?,
                Nullable::Value(value) => body.serialize_field("env", value.as_str())?,
            }
        }
        if let Some(environment_id) = &self.environment_id {
            body.serialize_field("environmentId", environment_id.as_str())?;
        }
        body.end()
    }
}

/// Inputs required to create one Dokploy Postgres database.
pub struct CreatePostgres {
    name: String,
    environment_id: EnvironmentId,
    database_name: String,
    database_user: String,
    database_password: Zeroizing<String>,
}

impl CreatePostgres {
    /// Creates Postgres input while retaining the password in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_name: impl Into<String>,
        database_user: impl Into<String>,
        database_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_name: database_name.into(),
            database_user: database_user.into(),
            database_password,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_name.is_empty()
            && !self.database_user.is_empty()
            && !self.database_password.is_empty()
    }
}

impl fmt::Debug for CreatePostgres {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreatePostgres")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .field("database_password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for CreatePostgres {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("CreatePostgres", 5)?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databaseName", &self.database_name)?;
        body.serialize_field("databaseUser", &self.database_user)?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        body.end()
    }
}

/// Physical identity returned by Dokploy when Postgres is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedPostgres {
    postgres_id: PostgresId,
}

impl CreatedPostgres {
    pub(crate) fn from_response(response: PostgresCreateResponse) -> Self {
        Self {
            postgres_id: response.postgres_id,
        }
    }

    /// Returns the new Postgres identity.
    #[must_use]
    pub const fn postgres_id(&self) -> &PostgresId {
        &self.postgres_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PostgresCreateResponse {
    postgres_id: PostgresId,
}

/// Owned Postgres fields written by one update.
pub struct UpdatePostgres {
    postgres_id: PostgresId,
    database_name: Option<String>,
    database_user: Option<String>,
    database_password: Option<Zeroizing<String>>,
}

impl UpdatePostgres {
    /// Starts a Postgres update with no fields selected.
    #[must_use]
    pub fn new(postgres_id: PostgresId) -> Self {
        Self {
            postgres_id,
            database_name: None,
            database_user: None,
            database_password: None,
        }
    }

    /// Selects the database name.
    #[must_use]
    pub fn with_database(mut self, database_name: impl Into<String>) -> Self {
        self.database_name = Some(database_name.into());
        self
    }

    /// Selects the database user.
    #[must_use]
    pub fn with_username(mut self, database_user: impl Into<String>) -> Self {
        self.database_user = Some(database_user.into());
        self
    }

    /// Selects the write-only database password.
    #[must_use]
    pub fn with_password(mut self, database_password: Zeroizing<String>) -> Self {
        self.database_password = Some(database_password);
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.postgres_id.as_str().is_empty()
            && (self.database_name.is_some()
                || self.database_user.is_some()
                || self.database_password.is_some())
    }
}

impl fmt::Debug for UpdatePostgres {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdatePostgres")
            .field("postgres_id", &self.postgres_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .field(
                "database_password",
                &self.database_password.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl Serialize for UpdatePostgres {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdatePostgres",
            1 + usize::from(self.database_name.is_some())
                + usize::from(self.database_user.is_some())
                + usize::from(self.database_password.is_some()),
        )?;
        body.serialize_field("postgresId", self.postgres_id.as_str())?;
        if let Some(database_name) = &self.database_name {
            body.serialize_field("databaseName", database_name)?;
        }
        if let Some(database_user) = &self.database_user {
            body.serialize_field("databaseUser", database_user)?;
        }
        if let Some(database_password) = &self.database_password {
            body.serialize_field("databasePassword", database_password.as_str())?;
        }
        body.end()
    }
}

/// Inputs required to create one Dokploy MongoDB database.
pub struct CreateMongo {
    name: String,
    environment_id: EnvironmentId,
    database_user: String,
    database_password: Zeroizing<String>,
    replica_sets: Option<bool>,
}

impl CreateMongo {
    /// Creates MongoDB input while retaining the required password in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_user: impl Into<String>,
        database_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_user: database_user.into(),
            database_password,
            replica_sets: None,
        }
    }

    /// Selects whether Dokploy configures MongoDB replica sets.
    #[must_use]
    pub fn with_replica_sets(mut self, replica_sets: bool) -> Self {
        self.replica_sets = Some(replica_sets);
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_user.is_empty()
            && !self.database_password.is_empty()
            && self
                .database_password
                .chars()
                .all(valid_database_password_character)
    }
}

impl fmt::Debug for CreateMongo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateMongo")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_user", &self.database_user)
            .field("database_password", &"[REDACTED]")
            .field("replica_sets", &self.replica_sets)
            .finish()
    }
}

impl Serialize for CreateMongo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer
            .serialize_struct("CreateMongo", 4 + usize::from(self.replica_sets.is_some()))?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databaseUser", &self.database_user)?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        if let Some(replica_sets) = self.replica_sets {
            body.serialize_field("replicaSets", &replica_sets)?;
        }
        body.end()
    }
}

/// Physical identity returned by Dokploy when MongoDB is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedMongo {
    mongo_id: MongoId,
}

impl CreatedMongo {
    pub(crate) fn from_response(response: MongoCreateResponse) -> Self {
        Self {
            mongo_id: response.mongo_id,
        }
    }

    /// Returns the new MongoDB identity.
    #[must_use]
    pub const fn mongo_id(&self) -> &MongoId {
        &self.mongo_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MongoCreateResponse {
    mongo_id: MongoId,
}

/// Owned non-secret MongoDB fields written by one update.
pub struct UpdateMongo {
    mongo_id: MongoId,
    database_user: Option<String>,
    replica_sets: Option<bool>,
}

impl UpdateMongo {
    /// Starts a MongoDB update with no fields selected.
    #[must_use]
    pub fn new(mongo_id: MongoId) -> Self {
        Self {
            mongo_id,
            database_user: None,
            replica_sets: None,
        }
    }

    /// Selects the database user.
    #[must_use]
    pub fn with_username(mut self, database_user: impl Into<String>) -> Self {
        self.database_user = Some(database_user.into());
        self
    }

    /// Selects whether Dokploy configures MongoDB replica sets.
    #[must_use]
    pub fn with_replica_sets(mut self, replica_sets: bool) -> Self {
        self.replica_sets = Some(replica_sets);
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mongo_id.as_str().is_empty()
            && (self
                .database_user
                .as_ref()
                .is_some_and(|value| !value.is_empty())
                || self.replica_sets.is_some())
    }
}

impl fmt::Debug for UpdateMongo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateMongo")
            .field("mongo_id", &self.mongo_id)
            .field("database_user", &self.database_user)
            .field("replica_sets", &self.replica_sets)
            .finish()
    }
}

impl Serialize for UpdateMongo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdateMongo",
            1 + usize::from(self.database_user.is_some())
                + usize::from(self.replica_sets.is_some()),
        )?;
        body.serialize_field("mongoId", self.mongo_id.as_str())?;
        if let Some(database_user) = &self.database_user {
            body.serialize_field("databaseUser", database_user)?;
        }
        if let Some(replica_sets) = self.replica_sets {
            body.serialize_field("replicaSets", &replica_sets)?;
        }
        body.end()
    }
}

/// A write-only MongoDB password rotation.
pub struct ChangeMongoPassword {
    mongo_id: MongoId,
    password: Zeroizing<String>,
}

impl ChangeMongoPassword {
    /// Creates an explicit MongoDB password rotation.
    #[must_use]
    pub fn new(mongo_id: MongoId, password: Zeroizing<String>) -> Self {
        Self { mongo_id, password }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mongo_id.as_str().is_empty()
            && !self.password.is_empty()
            && self.password.chars().all(valid_database_password_character)
    }
}

impl fmt::Debug for ChangeMongoPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChangeMongoPassword")
            .field("mongo_id", &self.mongo_id)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for ChangeMongoPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("ChangeMongoPassword", 2)?;
        body.serialize_field("mongoId", self.mongo_id.as_str())?;
        body.serialize_field("password", self.password.as_str())?;
        body.end()
    }
}

/// Inputs required to create one Dokploy MariaDB database.
pub struct CreateMariaDb {
    name: String,
    environment_id: EnvironmentId,
    database_name: String,
    database_user: String,
    database_password: Zeroizing<String>,
    database_root_password: Option<Zeroizing<String>>,
}

impl CreateMariaDb {
    /// Creates MariaDB input while retaining the required user password in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_name: impl Into<String>,
        database_user: impl Into<String>,
        database_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_name: database_name.into(),
            database_user: database_user.into(),
            database_password,
            database_root_password: None,
        }
    }

    /// Supplies an explicit root password instead of allowing Dokploy to generate one.
    #[must_use]
    pub fn with_root_password(mut self, database_root_password: Zeroizing<String>) -> Self {
        self.database_root_password = Some(database_root_password);
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_name.is_empty()
            && !self.database_user.is_empty()
            && !self.database_password.is_empty()
            && self
                .database_password
                .chars()
                .all(valid_database_password_character)
            && self.database_root_password.as_ref().is_none_or(|password| {
                !password.is_empty() && password.chars().all(valid_database_password_character)
            })
    }
}

impl fmt::Debug for CreateMariaDb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateMariaDb")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .field("database_password", &"[REDACTED]")
            .field(
                "database_root_password",
                &self.database_root_password.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl Serialize for CreateMariaDb {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "CreateMariaDb",
            5 + usize::from(self.database_root_password.is_some()),
        )?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databaseName", &self.database_name)?;
        body.serialize_field("databaseUser", &self.database_user)?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        if let Some(database_root_password) = &self.database_root_password {
            body.serialize_field("databaseRootPassword", database_root_password.as_str())?;
        }
        body.end()
    }
}

/// Physical identity returned by Dokploy when MariaDB is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedMariaDb {
    mariadb_id: MariaDbId,
}

impl CreatedMariaDb {
    pub(crate) fn from_response(response: MariaDbCreateResponse) -> Self {
        Self {
            mariadb_id: response.mariadb_id,
        }
    }

    /// Returns the new MariaDB identity.
    #[must_use]
    pub const fn mariadb_id(&self) -> &MariaDbId {
        &self.mariadb_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MariaDbCreateResponse {
    mariadb_id: MariaDbId,
}

/// Owned non-secret MariaDB fields written by one update.
pub struct UpdateMariaDb {
    mariadb_id: MariaDbId,
    database_name: Option<String>,
    database_user: Option<String>,
}

impl UpdateMariaDb {
    /// Starts a MariaDB update with no fields selected.
    #[must_use]
    pub fn new(mariadb_id: MariaDbId) -> Self {
        Self {
            mariadb_id,
            database_name: None,
            database_user: None,
        }
    }

    /// Selects the database name.
    #[must_use]
    pub fn with_database(mut self, database_name: impl Into<String>) -> Self {
        self.database_name = Some(database_name.into());
        self
    }

    /// Selects the database user.
    #[must_use]
    pub fn with_username(mut self, database_user: impl Into<String>) -> Self {
        self.database_user = Some(database_user.into());
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mariadb_id.as_str().is_empty()
            && (self
                .database_name
                .as_ref()
                .is_some_and(|value| !value.is_empty())
                || self
                    .database_user
                    .as_ref()
                    .is_some_and(|value| !value.is_empty()))
    }
}

impl fmt::Debug for UpdateMariaDb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateMariaDb")
            .field("mariadb_id", &self.mariadb_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .finish()
    }
}

impl Serialize for UpdateMariaDb {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdateMariaDb",
            1 + usize::from(self.database_name.is_some())
                + usize::from(self.database_user.is_some()),
        )?;
        body.serialize_field("mariadbId", self.mariadb_id.as_str())?;
        if let Some(database_name) = &self.database_name {
            body.serialize_field("databaseName", database_name)?;
        }
        if let Some(database_user) = &self.database_user {
            body.serialize_field("databaseUser", database_user)?;
        }
        body.end()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MariaDbPasswordTarget {
    User,
    Root,
}

impl MariaDbPasswordTarget {
    const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Root => "root",
        }
    }
}

/// A write-only MariaDB user or root password rotation.
pub struct ChangeMariaDbPassword {
    mariadb_id: MariaDbId,
    password: Zeroizing<String>,
    target: MariaDbPasswordTarget,
}

impl ChangeMariaDbPassword {
    /// Rotates the configured database user's password.
    #[must_use]
    pub fn user(mariadb_id: MariaDbId, password: Zeroizing<String>) -> Self {
        Self {
            mariadb_id,
            password,
            target: MariaDbPasswordTarget::User,
        }
    }

    /// Rotates the MariaDB root password.
    #[must_use]
    pub fn root(mariadb_id: MariaDbId, password: Zeroizing<String>) -> Self {
        Self {
            mariadb_id,
            password,
            target: MariaDbPasswordTarget::Root,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mariadb_id.as_str().is_empty()
            && !self.password.is_empty()
            && self.password.chars().all(valid_database_password_character)
    }
}

impl fmt::Debug for ChangeMariaDbPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChangeMariaDbPassword")
            .field("mariadb_id", &self.mariadb_id)
            .field("password", &"[REDACTED]")
            .field("target", &self.target)
            .finish()
    }
}

impl Serialize for ChangeMariaDbPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("ChangeMariaDbPassword", 3)?;
        body.serialize_field("mariadbId", self.mariadb_id.as_str())?;
        body.serialize_field("password", self.password.as_str())?;
        body.serialize_field("type", self.target.as_str())?;
        body.end()
    }
}

/// Inputs required to create one Dokploy MySQL database.
pub struct CreateMySql {
    name: String,
    environment_id: EnvironmentId,
    database_name: String,
    database_user: String,
    database_password: Zeroizing<String>,
    database_root_password: Zeroizing<String>,
}

impl CreateMySql {
    /// Creates MySQL input while retaining both required passwords in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_name: impl Into<String>,
        database_user: impl Into<String>,
        database_password: Zeroizing<String>,
        database_root_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_name: database_name.into(),
            database_user: database_user.into(),
            database_password,
            database_root_password,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_name.is_empty()
            && !self.database_user.is_empty()
            && !self.database_password.is_empty()
            && !self.database_root_password.is_empty()
    }
}

impl fmt::Debug for CreateMySql {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateMySql")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .field("database_password", &"[REDACTED]")
            .field("database_root_password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for CreateMySql {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("CreateMySql", 6)?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databaseName", &self.database_name)?;
        body.serialize_field("databaseUser", &self.database_user)?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        body.serialize_field("databaseRootPassword", self.database_root_password.as_str())?;
        body.end()
    }
}

/// Physical identity returned by Dokploy when MySQL is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedMySql {
    mysql_id: MySqlId,
}

impl CreatedMySql {
    pub(crate) fn from_response(response: MySqlCreateResponse) -> Self {
        Self {
            mysql_id: response.mysql_id,
        }
    }

    /// Returns the new MySQL identity.
    #[must_use]
    pub const fn mysql_id(&self) -> &MySqlId {
        &self.mysql_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MySqlCreateResponse {
    mysql_id: MySqlId,
}

/// Owned non-secret MySQL fields written by one update.
pub struct UpdateMySql {
    mysql_id: MySqlId,
    database_name: Option<String>,
    database_user: Option<String>,
}

impl UpdateMySql {
    /// Starts a MySQL update with no fields selected.
    #[must_use]
    pub fn new(mysql_id: MySqlId) -> Self {
        Self {
            mysql_id,
            database_name: None,
            database_user: None,
        }
    }

    /// Selects the database name.
    #[must_use]
    pub fn with_database(mut self, database_name: impl Into<String>) -> Self {
        self.database_name = Some(database_name.into());
        self
    }

    /// Selects the database user.
    #[must_use]
    pub fn with_username(mut self, database_user: impl Into<String>) -> Self {
        self.database_user = Some(database_user.into());
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mysql_id.as_str().is_empty()
            && (self
                .database_name
                .as_ref()
                .is_some_and(|value| !value.is_empty())
                || self
                    .database_user
                    .as_ref()
                    .is_some_and(|value| !value.is_empty()))
    }
}

impl fmt::Debug for UpdateMySql {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateMySql")
            .field("mysql_id", &self.mysql_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .finish()
    }
}

impl Serialize for UpdateMySql {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdateMySql",
            1 + usize::from(self.database_name.is_some())
                + usize::from(self.database_user.is_some()),
        )?;
        body.serialize_field("mysqlId", self.mysql_id.as_str())?;
        if let Some(database_name) = &self.database_name {
            body.serialize_field("databaseName", database_name)?;
        }
        if let Some(database_user) = &self.database_user {
            body.serialize_field("databaseUser", database_user)?;
        }
        body.end()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MySqlPasswordTarget {
    User,
    Root,
}

impl MySqlPasswordTarget {
    const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Root => "root",
        }
    }
}

/// A write-only MySQL user or root password rotation.
pub struct ChangeMySqlPassword {
    mysql_id: MySqlId,
    password: Zeroizing<String>,
    target: MySqlPasswordTarget,
}

impl ChangeMySqlPassword {
    /// Rotates the configured database user's password.
    #[must_use]
    pub fn user(mysql_id: MySqlId, password: Zeroizing<String>) -> Self {
        Self {
            mysql_id,
            password,
            target: MySqlPasswordTarget::User,
        }
    }

    /// Rotates the MySQL root password.
    #[must_use]
    pub fn root(mysql_id: MySqlId, password: Zeroizing<String>) -> Self {
        Self {
            mysql_id,
            password,
            target: MySqlPasswordTarget::Root,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mysql_id.as_str().is_empty()
            && !self.password.is_empty()
            && self.password.chars().all(valid_database_password_character)
    }
}

impl fmt::Debug for ChangeMySqlPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChangeMySqlPassword")
            .field("mysql_id", &self.mysql_id)
            .field("password", &"[REDACTED]")
            .field("target", &self.target)
            .finish()
    }
}

impl Serialize for ChangeMySqlPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("ChangeMySqlPassword", 3)?;
        body.serialize_field("mysqlId", self.mysql_id.as_str())?;
        body.serialize_field("password", self.password.as_str())?;
        body.serialize_field("type", self.target.as_str())?;
        body.end()
    }
}

fn valid_database_password_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '@' | '#'
                | '%'
                | '^'
                | '&'
                | '*'
                | '('
                | ')'
                | '_'
                | '+'
                | '-'
                | '='
                | '['
                | ']'
                | '{'
                | '}'
                | '|'
                | ';'
                | ':'
                | ','
                | '.'
                | '<'
                | '>'
                | '?'
                | '~'
                | '`'
        )
}

/// Inputs required to create one Dokploy Redis database.
pub struct CreateRedis {
    name: String,
    environment_id: EnvironmentId,
    database_password: Zeroizing<String>,
}

impl CreateRedis {
    /// Creates Redis input while retaining the password in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_password,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_password.is_empty()
    }
}

impl fmt::Debug for CreateRedis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateRedis")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for CreateRedis {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("CreateRedis", 3)?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        body.end()
    }
}

/// Physical identity returned by Dokploy when Redis is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedRedis {
    redis_id: RedisId,
}

impl CreatedRedis {
    pub(crate) fn from_response(response: RedisCreateResponse) -> Self {
        Self {
            redis_id: response.redis_id,
        }
    }

    /// Returns the new Redis identity.
    #[must_use]
    pub const fn redis_id(&self) -> &RedisId {
        &self.redis_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RedisCreateResponse {
    redis_id: RedisId,
}

/// A write-only Redis password update.
pub struct UpdateRedis {
    redis_id: RedisId,
    database_password: Zeroizing<String>,
}

impl UpdateRedis {
    /// Selects the exact Redis password intent.
    #[must_use]
    pub fn new(redis_id: RedisId, database_password: Zeroizing<String>) -> Self {
        Self {
            redis_id,
            database_password,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.redis_id.as_str().is_empty() && !self.database_password.is_empty()
    }
}

impl fmt::Debug for UpdateRedis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateRedis")
            .field("redis_id", &self.redis_id)
            .field("database_password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for UpdateRedis {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("UpdateRedis", 2)?;
        body.serialize_field("redisId", self.redis_id.as_str())?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        body.end()
    }
}

/// Inputs required to attach one domain to a Dokploy application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateDomain {
    pub(crate) host: String,
    pub(crate) application_id: ApplicationId,
}

impl CreateDomain {
    /// Creates an application domain with Dokploy's transport defaults.
    #[must_use]
    pub fn new(host: impl Into<String>, application_id: ApplicationId) -> Self {
        Self {
            host: host.into(),
            application_id,
        }
    }
}

/// Physical identity returned by Dokploy when a domain is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedDomain {
    domain_id: DomainId,
}

impl CreatedDomain {
    pub(crate) fn from_response(response: DomainCreateResponse) -> Self {
        Self {
            domain_id: response.domain_id,
        }
    }

    /// Returns the new domain identity.
    #[must_use]
    pub const fn domain_id(&self) -> &DomainId {
        &self.domain_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DomainCreateResponse {
    domain_id: DomainId,
}

/// The required host pair written by one Domain update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDomain {
    domain_id: DomainId,
    host: String,
}

impl UpdateDomain {
    /// Selects the exact Domain host.
    #[must_use]
    pub fn new(domain_id: DomainId, host: impl Into<String>) -> Self {
        Self {
            domain_id,
            host: host.into(),
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.domain_id.as_str().is_empty() && !self.host.is_empty()
    }
}

/// Presence-aware value returned by a tolerant Dokploy response model.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ResponseField<T> {
    /// Dokploy omitted the field, so its current value is unknown.
    #[default]
    NotReturned,
    /// Dokploy returned an explicit JSON null.
    Null,
    /// Dokploy returned a concrete value.
    Value(T),
}

impl<'de, T> Deserialize<'de> for ResponseField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Option::<T>::deserialize(deserializer)?.map_or(Self::Null, Self::Value))
    }
}

/// A stable subset of the response returned by `application.one`.
///
/// Unknown response fields are intentionally ignored. Dokploy's OpenAPI
/// document currently declares an empty response object, while the runtime
/// handler returns the application and its relations.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationDetails {
    pub application_id: ApplicationId,
    pub name: String,
    pub app_name: String,
    pub environment_id: EnvironmentId,
    #[serde(default)]
    pub source_type: ResponseField<String>,
    #[serde(default)]
    pub description: ResponseField<String>,
    #[serde(default)]
    pub replicas: ResponseField<u32>,
    #[serde(default)]
    pub repository: ResponseField<String>,
    #[serde(default)]
    pub branch: ResponseField<String>,
    #[serde(default)]
    pub build_path: ResponseField<String>,
    #[serde(default, rename = "env")]
    pub environment: ResponseField<ApplicationEnvironmentShape>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub has_git_provider_access: Option<bool>,
    #[serde(default)]
    pub server_id: Option<ServerId>,
    #[serde(default)]
    pub unauthorized_provider: Option<String>,
}

/// Value-free shape of the secret-bearing application `env` field.
///
/// The custom deserializer deliberately consumes strings without retaining
/// their bytes, so runtime environment values cannot enter snapshots, logs,
/// diagnostics, or debug output through the SDK model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationEnvironmentShape {
    /// Dokploy returned an empty environment document.
    Empty,
    /// Dokploy returned a non-empty environment document whose bytes are opaque.
    Opaque,
}

impl<'de> Deserialize<'de> for ApplicationEnvironmentShape {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ShapeVisitor;

        impl Visitor<'_> for ShapeVisitor {
            type Value = ApplicationEnvironmentShape;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an application environment string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(if value.is_empty() {
                    ApplicationEnvironmentShape::Empty
                } else {
                    ApplicationEnvironmentShape::Opaque
                })
            }
        }

        deserializer.deserialize_str(ShapeVisitor)
    }
}

/// One safe application entry returned by `application.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSearchItem {
    pub application_id: ApplicationId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected application search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationCollection {
    pub(crate) applications: Vec<ApplicationSearchItem>,
}

impl ApplicationCollection {
    /// Returns all applications discovered in the parent environment.
    #[must_use]
    pub fn applications(&self) -> &[ApplicationSearchItem] {
        &self.applications
    }
}

/// One page returned by the runtime `application.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplicationSearchPage {
    pub(crate) items: Vec<ApplicationSearchItem>,
    pub(crate) total: u64,
}

/// A safe subset of the response returned by `postgres.one`.
///
/// Credentials, environment variables, and other secret-bearing runtime
/// fields are deliberately absent, so they cannot enter plans, state, logs,
/// diagnostics, or snapshots through this model. Unknown response fields are
/// intentionally ignored because Dokploy's OpenAPI response schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostgresDetails {
    pub postgres_id: PostgresId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub database_name: ResponseField<String>,
    #[serde(default)]
    pub database_user: ResponseField<String>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: Option<ServerId>,
}

/// A safe subset of the response returned by `mariadb.one`.
///
/// User and root passwords, environment variables, and other secret-bearing
/// runtime fields are deliberately absent. Unknown response fields are ignored
/// because Dokploy's OpenAPI success schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MariaDbDetails {
    pub mariadb_id: MariaDbId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub database_name: ResponseField<String>,
    #[serde(default)]
    pub database_user: ResponseField<String>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: Option<ServerId>,
}

/// A safe subset of the response returned by `mongo.one`.
///
/// Passwords, environment variables, mounts, and other secret-bearing runtime
/// fields are deliberately absent. Unknown response fields are ignored because
/// Dokploy's OpenAPI success schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MongoDetails {
    pub mongo_id: MongoId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub database_user: ResponseField<String>,
    #[serde(default)]
    pub replica_sets: ResponseField<bool>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: Option<ServerId>,
}

/// One safe MongoDB entry returned by `mongo.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MongoSearchItem {
    pub mongo_id: MongoId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected MongoDB search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct MongoCollection {
    pub(crate) mongo: Vec<MongoSearchItem>,
}

impl MongoCollection {
    /// Returns all MongoDB databases discovered in the parent environment.
    #[must_use]
    pub fn mongo(&self) -> &[MongoSearchItem] {
        &self.mongo
    }
}

/// One page returned by the runtime `mongo.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MongoSearchPage {
    pub(crate) items: Vec<MongoSearchItem>,
    pub(crate) total: u64,
}

/// One safe MariaDB entry returned by `mariadb.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MariaDbSearchItem {
    pub mariadb_id: MariaDbId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected MariaDB search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct MariaDbCollection {
    pub(crate) mariadb: Vec<MariaDbSearchItem>,
}

impl MariaDbCollection {
    /// Returns all MariaDB databases discovered in the parent environment.
    #[must_use]
    pub fn mariadb(&self) -> &[MariaDbSearchItem] {
        &self.mariadb
    }
}

/// One page returned by the runtime `mariadb.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MariaDbSearchPage {
    pub(crate) items: Vec<MariaDbSearchItem>,
    pub(crate) total: u64,
}

/// A safe subset of the response returned by `mysql.one`.
///
/// User and root passwords, environment variables, mounts, backups, and other
/// secret-bearing runtime fields are deliberately absent. Unknown response
/// fields are ignored because Dokploy's OpenAPI success schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MySqlDetails {
    pub mysql_id: MySqlId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub database_name: ResponseField<String>,
    #[serde(default)]
    pub database_user: ResponseField<String>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: Option<ServerId>,
}

/// One safe MySQL entry returned by `mysql.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MySqlSearchItem {
    pub mysql_id: MySqlId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected MySQL search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct MySqlCollection {
    pub(crate) mysql: Vec<MySqlSearchItem>,
}

impl MySqlCollection {
    /// Returns all MySQL databases discovered in the parent environment.
    #[must_use]
    pub fn mysql(&self) -> &[MySqlSearchItem] {
        &self.mysql
    }
}

/// One page returned by the runtime `mysql.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MySqlSearchPage {
    pub(crate) items: Vec<MySqlSearchItem>,
    pub(crate) total: u64,
}

/// One safe Postgres entry returned by `postgres.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostgresSearchItem {
    pub postgres_id: PostgresId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected Postgres search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct PostgresCollection {
    pub(crate) postgres: Vec<PostgresSearchItem>,
}

impl PostgresCollection {
    /// Returns all Postgres databases discovered in the parent environment.
    #[must_use]
    pub fn postgres(&self) -> &[PostgresSearchItem] {
        &self.postgres
    }
}

/// One page returned by the runtime `postgres.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PostgresSearchPage {
    pub(crate) items: Vec<PostgresSearchItem>,
    pub(crate) total: u64,
}

/// A safe subset of the response returned by `redis.one`.
///
/// Passwords, environment variables, and nested runtime data are deliberately
/// absent so secret bytes cannot cross the SDK read seam.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedisDetails {
    pub redis_id: RedisId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: Option<ServerId>,
}

/// One safe Redis entry returned by `redis.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedisSearchItem {
    pub redis_id: RedisId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected Redis search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct RedisCollection {
    pub(crate) redis: Vec<RedisSearchItem>,
}

impl RedisCollection {
    /// Returns all Redis databases discovered in the parent environment.
    #[must_use]
    pub fn redis(&self) -> &[RedisSearchItem] {
        &self.redis
    }
}

/// One page returned by the runtime `redis.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RedisSearchPage {
    pub(crate) items: Vec<RedisSearchItem>,
    pub(crate) total: u64,
}

/// A safe subset of the response returned by `domain.one`.
///
/// Routing middleware and application runtime details are deliberately absent;
/// reconciliation owns only the domain identity, host, and application link.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DomainDetails {
    pub domain_id: DomainId,
    pub host: String,
    #[serde(default)]
    pub application_id: Option<ApplicationId>,
}

/// The complete domain collection returned by `domain.byApplicationId`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct DomainCollection {
    domains: Vec<DomainDetails>,
}

impl DomainCollection {
    /// Returns all domains discovered for the selected application.
    #[must_use]
    pub fn domains(&self) -> &[DomainDetails] {
        &self.domains
    }
}

/// The project collection returned by `project.all`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct ProjectTopology {
    projects: Vec<ProjectDetails>,
}

impl ProjectTopology {
    #[must_use]
    pub fn projects(&self) -> &[ProjectDetails] {
        &self.projects
    }
}

/// A project and the environment topology needed for resource discovery.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetails {
    pub project_id: ProjectId,
    pub name: String,
    #[serde(default)]
    pub description: ResponseField<String>,
    #[serde(default)]
    pub environments: Vec<EnvironmentTopology>,
}

/// An environment and the initial MVP resource summaries nested below it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTopology {
    pub environment_id: EnvironmentId,
    pub name: String,
    pub is_default: bool,
    #[serde(default)]
    pub applications: Vec<ApplicationSummary>,
    #[serde(default)]
    pub postgres: Vec<PostgresSummary>,
    #[serde(default)]
    pub redis: Vec<RedisSummary>,
}

/// A safe subset of the response returned by `environment.one`.
///
/// Environment variables and nested resources are deliberately omitted so
/// secret-bearing runtime fields cannot cross into discovery snapshots.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDetails {
    pub environment_id: EnvironmentId,
    pub name: String,
    #[serde(default)]
    pub description: ResponseField<String>,
    pub project_id: ProjectId,
}

/// One safe environment entry returned by `environment.byProjectId`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSummary {
    pub environment_id: EnvironmentId,
    pub name: String,
    #[serde(default)]
    pub description: ResponseField<String>,
}

/// The parent-scoped environment collection returned by `environment.byProjectId`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct EnvironmentCollection {
    environments: Vec<EnvironmentSummary>,
}

impl EnvironmentCollection {
    #[must_use]
    pub fn environments(&self) -> &[EnvironmentSummary] {
        &self.environments
    }
}

/// The role-dependent application projection embedded in `project.all`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSummary {
    pub application_id: ApplicationId,
    pub name: String,
    #[serde(default)]
    pub application_status: Option<String>,
}

/// The role-dependent Postgres projection embedded in `project.all`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostgresSummary {
    pub postgres_id: PostgresId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub application_status: Option<String>,
}

/// The role-dependent Redis projection embedded in `project.all`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedisSummary {
    pub redis_id: RedisId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub application_status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        ApplicationDetails, ApplicationEnvironmentShape, PostgresDetails, ProjectTopology,
        ResponseField,
    };

    #[test]
    fn application_details_tolerate_unknown_runtime_fields() {
        let application: ApplicationDetails = serde_json::from_str(include_str!(
            "../../../fixtures/api/live/v0.30.6/application-one.owner.json"
        ))
        .expect("fixture must deserialize");

        assert_eq!(application.application_id.as_str(), "application-1");
        assert_eq!(application.environment_id.as_str(), "environment-1");
        assert_eq!(application.server_id, None);
        assert_eq!(application.description, ResponseField::Null);
        assert_eq!(application.replicas, ResponseField::Value(1));
        assert_eq!(application.build_path, ResponseField::Value("/".to_owned()));
        assert_eq!(application.environment, ResponseField::Null);
    }

    #[test]
    fn application_environment_shape_never_retains_runtime_bytes() {
        let application: ApplicationDetails = serde_json::from_str(
            r#"{
                "applicationId":"application-1",
                "environmentId":"environment-1",
                "name":"API",
                "appName":"api",
                "env":"SECRET=environment-value-canary"
            }"#,
        )
        .expect("application response is valid");

        assert_eq!(
            application.environment,
            ResponseField::Value(ApplicationEnvironmentShape::Opaque)
        );
        assert!(!format!("{application:?}").contains("environment-value-canary"));
    }

    #[test]
    fn application_fields_preserve_omitted_null_and_value() {
        let application: ApplicationDetails = serde_json::from_str(
            r#"{
                "applicationId":"application-1",
                "environmentId":"environment-1",
                "name":"API",
                "appName":"api",
                "description":null,
                "replicas":2,
                "repository":"owner/repository",
                "branch":"main",
                "sourceType":"github",
                "buildPath":"apps/api",
                "env":""
            }"#,
        )
        .expect("application response is valid");

        assert_eq!(application.description, ResponseField::Null);
        assert_eq!(application.replicas, ResponseField::Value(2));
        assert_eq!(
            application.repository,
            ResponseField::Value("owner/repository".to_owned())
        );
        assert_eq!(application.branch, ResponseField::Value("main".to_owned()));
        assert_eq!(
            application.source_type,
            ResponseField::Value("github".to_owned())
        );
        assert_eq!(
            application.environment,
            ResponseField::Value(ApplicationEnvironmentShape::Empty)
        );
    }

    #[test]
    fn project_topology_accepts_role_dependent_service_summaries() {
        let topology: ProjectTopology = serde_json::from_str(include_str!(
            "../../../fixtures/api/live/v0.30.6/project-all.populated.owner.json"
        ))
        .expect("fixture must deserialize");

        let project = &topology.projects()[0];
        let environment = &project.environments[0];

        assert_eq!(project.project_id.as_str(), "project-1");
        assert_eq!(
            project.description,
            ResponseField::Value("Disposable integration fixture".to_owned())
        );
        assert_eq!(environment.applications[0].name, "API");
        assert_eq!(environment.postgres[0].name, None);
    }

    #[test]
    fn project_description_preserves_omitted_null_and_string_responses() {
        let cases = [
            (
                r#"[{"projectId":"project-1","name":"omitted"}]"#,
                ResponseField::NotReturned,
            ),
            (
                r#"[{"projectId":"project-1","name":"null","description":null}]"#,
                ResponseField::Null,
            ),
            (
                r#"[{"projectId":"project-1","name":"value","description":"known"}]"#,
                ResponseField::Value("known".to_owned()),
            ),
        ];

        for (json, expected) in cases {
            let topology: ProjectTopology =
                serde_json::from_str(json).expect("project topology is valid");

            assert_eq!(topology.projects()[0].description, expected);
        }
    }

    #[test]
    fn postgres_details_ignore_secret_bearing_runtime_fields() {
        let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/api/live/v0.30.6/postgres-one.owner.json"
        ))
        .expect("fixture must be valid JSON");
        fixture["databasePassword"] = "password-canary".into();
        fixture["env"] = "environment-secret-canary".into();
        fixture["buildSecrets"] = "build-secret-canary".into();

        let postgres: PostgresDetails =
            serde_json::from_value(fixture).expect("fixture must deserialize");
        let debug = format!("{postgres:?}");

        assert_eq!(postgres.postgres_id.as_str(), "postgres-1");
        assert_eq!(postgres.environment_id.as_str(), "environment-1");
        assert!(!debug.contains("password-canary"));
        assert!(!debug.contains("environment-secret-canary"));
        assert!(!debug.contains("build-secret-canary"));
    }

    #[test]
    fn postgres_owned_fields_preserve_omitted_null_and_value() {
        let cases = [
            (
                r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"omitted","appName":"omitted","dockerImage":"postgres:16"}"#,
                ResponseField::NotReturned,
                ResponseField::NotReturned,
            ),
            (
                r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"null","appName":"null","dockerImage":"postgres:16","databaseName":null,"databaseUser":null}"#,
                ResponseField::Null,
                ResponseField::Null,
            ),
            (
                r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"value","appName":"value","dockerImage":"postgres:16","databaseName":"app","databaseUser":"owner"}"#,
                ResponseField::Value("app".to_owned()),
                ResponseField::Value("owner".to_owned()),
            ),
        ];

        for (json, expected_database, expected_user) in cases {
            let postgres: PostgresDetails =
                serde_json::from_str(json).expect("Postgres details are valid");

            assert_eq!(postgres.database_name, expected_database);
            assert_eq!(postgres.database_user, expected_user);
        }
    }
}
