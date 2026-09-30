use std::env;

use dokploy_sdk::{ApplicationId, Dokploy, EnvironmentId, PostgresId, ProjectId, ResponseField};

const LIVE_TEST_FLAG: &str = "DOKPLOY_SDK_LIVE_TEST";

fn live_client() -> Dokploy {
    assert_eq!(
        env::var(LIVE_TEST_FLAG).as_deref(),
        Ok("1"),
        "live SDK tests must be invoked through scripts/integration/test-sdk.sh"
    );

    let url = env::var("DOKPLOY_URL").expect("the live Dokploy URL must be provided");
    let api_key = env::var("DOKPLOY_API_KEY").expect("the live Dokploy API key must be provided");

    Dokploy::builder()
        .url(url)
        .api_key(api_key)
        .build()
        .expect("the live SDK client configuration must be valid")
}

#[tokio::test]
#[ignore = "requires an explicit local Dokploy integration run"]
async fn projects_all_reads_the_live_remote_topology() {
    let client = live_client();

    let first_read = client
        .projects()
        .all()
        .await
        .expect("the first live project read must succeed");
    let second_read = client
        .projects()
        .all()
        .await
        .expect("the second live project read must succeed");

    assert!(
        !first_read.projects().is_empty(),
        "the live fixture must contain a project"
    );
    assert_eq!(first_read, second_read);
}

#[tokio::test]
#[ignore = "requires an explicit local Dokploy integration run"]
async fn projects_get_reads_the_selected_live_project() {
    let client = live_client();
    let topology = client
        .projects()
        .all()
        .await
        .expect("the live project topology must be readable");
    let expected = topology
        .projects()
        .first()
        .expect("the live fixture must contain a project");

    let project = client
        .projects()
        .get(ProjectId::new(expected.project_id.as_str()))
        .await
        .expect("the selected live project must be readable");

    assert_eq!(project.project_id, expected.project_id);
    assert_eq!(project.name, expected.name);
    assert_eq!(
        project
            .environments
            .first()
            .map(|item| &item.environment_id),
        expected
            .environments
            .first()
            .map(|item| &item.environment_id)
    );
}

#[tokio::test]
#[ignore = "requires an explicit local Dokploy integration run"]
async fn environments_read_the_live_parent_collection_and_selected_environment() {
    let client = live_client();
    let topology = client
        .projects()
        .all()
        .await
        .expect("the live project topology must be readable");
    let project = topology
        .projects()
        .iter()
        .find(|project| !project.environments.is_empty())
        .expect("the live fixture must contain an environment");
    let expected = project
        .environments
        .first()
        .expect("the selected project must contain an environment");

    let collection = client
        .environments()
        .by_project(ProjectId::new(project.project_id.as_str()))
        .await
        .expect("the live environment collection must be readable");
    let environment = client
        .environments()
        .get(EnvironmentId::new(expected.environment_id.as_str()))
        .await
        .expect("the selected live environment must be readable");

    assert!(
        collection
            .environments()
            .iter()
            .any(|item| item.environment_id == expected.environment_id)
    );
    assert_eq!(environment.environment_id, expected.environment_id);
    assert_eq!(environment.project_id, project.project_id);
}

#[tokio::test]
#[ignore = "requires an explicit local Dokploy integration run"]
async fn applications_get_reads_the_selected_live_application() {
    let client = live_client();
    let topology = client
        .projects()
        .all()
        .await
        .expect("the live project topology must be readable");
    let environment = topology
        .projects()
        .iter()
        .flat_map(|project| &project.environments)
        .find(|environment| !environment.applications.is_empty())
        .expect("the live fixture must contain an environment with an application");
    let expected = environment
        .applications
        .first()
        .expect("the selected environment must contain an application");

    let application = client
        .applications()
        .get(ApplicationId::new(expected.application_id.as_str()))
        .await
        .expect("the selected live application must be readable");
    let collection = client
        .applications()
        .by_environment(EnvironmentId::new(environment.environment_id.as_str()))
        .await
        .expect("the live parent-scoped application collection must be readable");

    assert_eq!(application.application_id, expected.application_id);
    assert_eq!(application.environment_id, environment.environment_id);
    assert_eq!(application.name, expected.name);
    assert!(
        collection
            .applications()
            .iter()
            .any(|item| item.application_id == expected.application_id)
    );
}

#[tokio::test]
#[ignore = "requires an explicit local Dokploy integration run"]
async fn postgres_reads_the_selected_live_database_and_parent_collection() {
    let client = live_client();
    let topology = client
        .projects()
        .all()
        .await
        .expect("the live project topology must be readable");
    let environment = topology
        .projects()
        .iter()
        .flat_map(|project| &project.environments)
        .find(|environment| !environment.postgres.is_empty())
        .expect("the live fixture must contain an environment with a Postgres database");
    let expected = environment
        .postgres
        .first()
        .expect("the selected environment must contain a Postgres database");

    let postgres = client
        .postgres()
        .get(PostgresId::new(expected.postgres_id.as_str()))
        .await
        .expect("the selected live Postgres database must be readable");
    let collection = client
        .postgres()
        .by_environment(EnvironmentId::new(environment.environment_id.as_str()))
        .await
        .expect("the live parent-scoped Postgres collection must be readable");

    assert_eq!(postgres.postgres_id, expected.postgres_id);
    assert_eq!(postgres.environment_id, environment.environment_id);
    assert!(
        collection
            .postgres()
            .iter()
            .any(|item| item.postgres_id == expected.postgres_id)
    );
    assert!(!postgres.name.trim().is_empty());
    assert!(matches!(
        postgres.database_name,
        ResponseField::Value(value) if !value.trim().is_empty()
    ));
    assert!(matches!(
        postgres.database_user,
        ResponseField::Value(value) if !value.trim().is_empty()
    ));
}

#[tokio::test]
#[ignore = "requires an explicit local Dokploy integration run"]
async fn redis_reads_the_live_empty_parent_collection() {
    let client = live_client();
    let topology = client
        .projects()
        .all()
        .await
        .expect("the live project topology must be readable");
    let environment = topology
        .projects()
        .iter()
        .flat_map(|project| &project.environments)
        .find(|environment| environment.name == "production")
        .expect("the live fixture must contain its production environment");

    let collection = client
        .redis()
        .by_environment(EnvironmentId::new(environment.environment_id.as_str()))
        .await
        .expect("the live parent-scoped Redis collection must be readable");

    assert!(
        collection.redis().is_empty(),
        "the disposable Redis contract record must have been removed"
    );
}

#[tokio::test]
#[ignore = "requires an explicit local Dokploy integration run"]
async fn invalid_authentication_returns_a_structured_unauthorized_error() {
    assert_eq!(
        env::var(LIVE_TEST_FLAG).as_deref(),
        Ok("1"),
        "live SDK tests must be invoked through scripts/integration/test-sdk.sh"
    );

    let url = env::var("DOKPLOY_URL").expect("the live Dokploy URL must be provided");
    let client = Dokploy::builder()
        .url(url)
        .api_key("deliberately-invalid-integration-key")
        .build()
        .expect("the live SDK client configuration must be valid");

    let error = client
        .projects()
        .all()
        .await
        .expect_err("an invalid API key must be rejected");
    let details = error
        .dokploy()
        .expect("an authentication failure must preserve structured error details");

    assert_eq!(details.status(), 401);
    assert_eq!(details.code(), "UNAUTHORIZED");
    assert_eq!(details.message(), "Unauthorized");
}
