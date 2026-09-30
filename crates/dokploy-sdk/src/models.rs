use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
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
identifier!(PostgresId);
identifier!(ProjectId);
identifier!(RedisId);
identifier!(ServerId);

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
