use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dokploy_cli::executor::{
    ApplyOptions, apply_workspace, apply_workspace_with_approval, destroy_workspace_with_approval,
};
use dokploy_cli::planning::plan_workspace;
use dokploy_sdk::Dokploy;
use dokploy_state::{InstanceIdentity, RecoveryStatus, ResourceAddress, StateStore};

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

struct ConcurrentDatabaseServer {
    url: String,
    result: Receiver<(Vec<String>, bool)>,
    thread: JoinHandle<()>,
}

#[tokio::test]
async fn destroy_deletes_every_tracked_resource_and_leaves_initialized_empty_state() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        ),
        ("200 OK", r#"{"ok":true}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(&config, "version: 1\nproject:\n  name: platform\n")
        .expect("configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial apply succeeds");

    let summary = destroy_workspace_with_approval(&client, &config, |plan| {
        assert_eq!(plan.changes().len(), 1);
        Ok(true)
    })
    .await
    .expect("destroy succeeds");

    assert_eq!(summary.applied(), 1);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    assert!(
        store
            .inspect()
            .expect("state is readable")
            .expect("state remains initialized")
            .resources()
            .is_empty()
    );
    assert_eq!(
        store.recovery_status().expect("journal scan succeeds"),
        RecoveryStatus::Clean
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].starts_with("POST /api/project.remove HTTP/1.1\r\n"));
    assert!(requests[3].contains(r#""projectId":"project-1""#));
}

#[tokio::test]
async fn destroy_without_state_is_a_noop_without_remote_reads_or_state_creation() {
    let server = TestServer::respond_in_sequence(Vec::new());
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    let summary = destroy_workspace_with_approval(&server.client(), &config, |plan| {
        assert!(plan.changes().is_empty());
        Ok(true)
    })
    .await
    .expect("empty destroy succeeds");

    assert_eq!(summary.applied(), 0);
    assert!(!directory.path().join(".dokploy/state.json").exists());
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn postgres_owned_fields_update_in_place() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/postgres-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"main","appName":"postgres-main","dockerImage":"postgres:16","databaseName":"app","databaseUser":"app"}"#,
        ),
        ("200 OK", r#"{"ok":true}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("postgres"), "postgres-password")
        .expect("Postgres secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    let postgres_config = |database: &str, username: &str| {
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    postgres:\n",
                "      main:\n",
                "        database: {}\n",
                "        username: {}\n",
                "        password:\n",
                "          file: .secrets/postgres\n",
            ),
            database, username,
        )
    };
    fs::write(&config, postgres_config("app", "app"))
        .expect("initial configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial Postgres apply succeeds");
    fs::write(&config, postgres_config("app_next", "app_next"))
        .expect("updated configuration fixture is writable");

    let summary = apply_workspace(&client, &config)
        .await
        .expect("Postgres update succeeds");

    assert_eq!(summary.applied(), 1);
    let requests = server.finish();
    assert_eq!(requests.len(), 9);
    assert!(requests[8].starts_with("POST /api/postgres.update HTTP/1.1\r\n"));
    assert!(requests[8].contains(r#""databaseName":"app_next""#));
    assert!(requests[8].contains(r#""databaseUser":"app_next""#));
    assert!(!requests[8].contains("databasePassword"));
}

#[tokio::test]
async fn compose_create_update_and_delete_preserve_volumes_without_persisting_the_document() {
    let project = r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#;
    let environments =
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
    let environment =
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
    let search = r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"web","appName":"web-app","description":"Initial stack","sourceType":"raw"}],"total":1}"#;
    let old = r#"{"composeId":"compose-1","environmentId":"environment-1","name":"web","appName":"web-app","description":"Initial stack","sourceType":"raw","composeType":"docker-compose","autoDeploy":false,"composePath":"./docker-compose.yml","composeStatus":"idle"}"#;
    let updated_search = r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"web","appName":"web-app","description":"Updated stack","sourceType":"raw"}],"total":1}"#;
    let updated = r#"{"composeId":"compose-1","environmentId":"environment-1","name":"web","appName":"web-app","description":"Updated stack","sourceType":"raw","composeType":"docker-compose","autoDeploy":false,"composePath":"./docker-compose.yml","composeStatus":"idle"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"{"composeId":"compose-1","environmentId":"environment-1","name":"web"}"#,
        ),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", search),
        ("200 OK", old),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/compose-update.owner.json"),
        ),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", updated_search),
        ("200 OK", updated),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/compose-delete.owner.json"),
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let compose_document = directory.path().join("compose.yaml");
    let config = directory.path().join("dokploy.yaml");
    let compose_config = |description: &str| {
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    compose:\n",
                "      web:\n",
                "        description: {:?}\n",
                "        document:\n",
                "          file: compose.yaml\n",
            ),
            description,
        )
    };
    fs::write(
        &compose_document,
        "services:\n  web:\n    image: initial-document-canary\n",
    )
    .expect("initial Compose document is writable");
    fs::write(&config, compose_config("Initial stack"))
        .expect("initial configuration fixture is writable");
    let client = server.client();

    let created = apply_workspace(&client, &config)
        .await
        .expect("initial Compose apply succeeds");
    assert_eq!(created.applied(), 3);
    fs::write(
        &compose_document,
        "services:\n  web:\n    image: updated-document-canary\n",
    )
    .expect("updated Compose document is writable");
    fs::write(&config, compose_config("Updated stack"))
        .expect("updated configuration fixture is writable");
    let updated_summary = apply_workspace(&client, &config)
        .await
        .expect("Compose update succeeds");
    assert_eq!(updated_summary.applied(), 1);
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    compose: {}\n",
            "removed:\n",
            "  - from: compose.web\n",
            "    destroy: true\n",
        ),
    )
    .expect("removal configuration fixture is writable");
    let deleted = apply_workspace(&client, &config)
        .await
        .expect("Compose delete succeeds");
    assert_eq!(deleted.applied(), 1);

    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    let state = store
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    assert!(state.resource(&"compose.web".parse().unwrap()).is_none());
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);

    let requests = server.finish();
    assert_eq!(requests.len(), 15);
    assert!(requests[2].starts_with("POST /api/compose.create HTTP/1.1\r\n"));
    assert!(requests[2].contains("initial-document-canary"));
    assert!(requests[2].contains(r#""sourceType":"raw""#));
    assert!(requests[8].starts_with("POST /api/compose.update HTTP/1.1\r\n"));
    assert!(requests[8].contains("updated-document-canary"));
    assert!(requests[8].contains(r#""description":"Updated stack""#));
    assert!(requests[14].starts_with("POST /api/compose.delete HTTP/1.1\r\n"));
    assert!(requests[14].contains(r#""deleteVolumes":false"#));
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("compose.deploy"))
    );

    let state_json = fs::read_to_string(directory.path().join(".dokploy/state.json"))
        .expect("state is readable as text");
    assert!(!state_json.contains("initial-document-canary"));
    assert!(!state_json.contains("updated-document-canary"));
    assert!(!format!("{created:?} {updated_summary:?} {deleted:?}").contains("document-canary"));
}

#[tokio::test]
async fn compose_unknown_create_outcome_keeps_the_document_step_recoverable() {
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    fs::write(
        directory.path().join("compose.yaml"),
        "services:\n  web:\n    image: unknown-compose-document-canary\n",
    )
    .expect("Compose document fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    compose:\n",
            "      web:\n",
            "        document:\n",
            "          file: compose.yaml\n",
        ),
    )
    .expect("configuration fixture is writable");

    let error = apply_workspace(&server.client(), &config)
        .await
        .expect_err("an interrupted Compose create has an unknown outcome");
    assert!(matches!(
        error,
        dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
            code: dokploy_state::FailureCode::TransportOutcomeUnknown
        }
    ));
    assert_recovery_step_in_progress(directory.path(), &server.url);
    let journal = fs::read_dir(directory.path().join(".dokploy/journal"))
        .unwrap()
        .find_map(|entry| {
            let path = entry.ok()?.path();
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                .then(|| fs::read_to_string(path).unwrap())
        })
        .expect("journal exists");
    assert!(!journal.contains("stepFailed"));
    assert!(!journal.contains("unknown-compose-document-canary"));
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn mysql_create_metadata_update_and_delete_are_checkpointed_without_secret_leaks() {
    let project = r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#;
    let environments =
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
    let environment =
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
    let search = r#"{"items":[{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main"}],"total":1}"#;
    let old = r#"{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main","appName":"mysql-main","dockerImage":"mysql:8","databaseName":"app","databaseUser":"app"}"#;
    let updated = r#"{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main","appName":"mysql-main","dockerImage":"mysql:8","databaseName":"app_next","databaseUser":"app_next"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", r#"{"mysqlId":"mysql-1"}"#),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", search),
        ("200 OK", old),
        ("200 OK", "true"),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", search),
        ("200 OK", updated),
        ("200 OK", "true"),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("mysql-user"), "mysql-user-password-canary")
        .expect("MySQL user secret fixture is writable");
    fs::write(secrets.join("mysql-root"), "mysql-root-password-canary")
        .expect("MySQL root secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    let mysql_config = |database: &str, username: &str| {
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    mysql:\n",
                "      main:\n",
                "        database: {}\n",
                "        username: {}\n",
                "        password:\n",
                "          file: .secrets/mysql-user\n",
                "        root_password:\n",
                "          file: .secrets/mysql-root\n",
            ),
            database, username,
        )
    };
    fs::write(&config, mysql_config("app", "app"))
        .expect("initial configuration fixture is writable");
    let client = server.client();

    let created = apply_workspace(&client, &config)
        .await
        .expect("initial MySQL apply succeeds");
    assert_eq!(created.applied(), 3);
    fs::write(&config, mysql_config("app_next", "app_next"))
        .expect("updated configuration fixture is writable");
    let updated = apply_workspace(&client, &config)
        .await
        .expect("MySQL metadata update succeeds");
    assert_eq!(updated.applied(), 1);
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    mysql: {}\n",
            "removed:\n",
            "  - from: mysql.main\n",
            "    destroy: true\n",
        ),
    )
    .expect("removal configuration fixture is writable");
    let deleted = apply_workspace(&client, &config)
        .await
        .expect("MySQL delete succeeds");
    assert_eq!(deleted.applied(), 1);

    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    let state = store
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    assert!(state.resource(&"mysql.main".parse().unwrap()).is_none());
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);

    let requests = server.finish();
    assert_eq!(requests.len(), 15);
    assert!(requests[2].starts_with("POST /api/mysql.create HTTP/1.1\r\n"));
    assert!(requests[2].contains(r#""databasePassword":"mysql-user-password-canary""#));
    assert!(requests[2].contains(r#""databaseRootPassword":"mysql-root-password-canary""#));
    assert!(requests[8].starts_with("POST /api/mysql.update HTTP/1.1\r\n"));
    assert!(requests[8].contains(r#""databaseName":"app_next""#));
    assert!(requests[8].contains(r#""databaseUser":"app_next""#));
    assert!(!requests[8].contains("databasePassword"));
    assert!(!requests[8].contains("databaseRootPassword"));
    assert!(requests[14].starts_with("POST /api/mysql.remove HTTP/1.1\r\n"));

    let state_json = fs::read_to_string(directory.path().join(".dokploy/state.json"))
        .expect("state is readable as text");
    assert!(!state_json.contains("mysql-user-password-canary"));
    assert!(!state_json.contains("mysql-root-password-canary"));
    assert!(!format!("{created:?} {updated:?} {deleted:?}").contains("password-canary"));
}

#[tokio::test]
async fn mariadb_create_metadata_update_and_delete_are_checkpointed_without_secret_leaks() {
    let project = r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#;
    let environments =
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
    let environment =
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
    let search = r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main"}],"total":1}"#;
    let old = r#"{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main","appName":"mariadb-main","dockerImage":"mariadb:11","databaseName":"app","databaseUser":"app"}"#;
    let updated = r#"{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main","appName":"mariadb-main","dockerImage":"mariadb:11","databaseName":"app_next","databaseUser":"app_next"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", r#"{"mariadbId":"mariadb-1"}"#),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", search),
        ("200 OK", old),
        ("200 OK", "true"),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", search),
        ("200 OK", updated),
        ("200 OK", "true"),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("mariadb-user"), "mariadb-user-password-canary")
        .expect("MariaDB user secret fixture is writable");
    fs::write(secrets.join("mariadb-root"), "mariadb-root-password-canary")
        .expect("MariaDB root secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    let mariadb_config = |database: &str, username: &str| {
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    mariadb:\n",
                "      main:\n",
                "        database: {}\n",
                "        username: {}\n",
                "        password:\n",
                "          file: .secrets/mariadb-user\n",
                "        root_password:\n",
                "          file: .secrets/mariadb-root\n",
            ),
            database, username,
        )
    };
    fs::write(&config, mariadb_config("app", "app"))
        .expect("initial configuration fixture is writable");
    let client = server.client();

    let created = apply_workspace(&client, &config)
        .await
        .expect("initial MariaDB apply succeeds");
    assert_eq!(created.applied(), 3);
    fs::write(&config, mariadb_config("app_next", "app_next"))
        .expect("updated configuration fixture is writable");
    let updated = apply_workspace(&client, &config)
        .await
        .expect("MariaDB metadata update succeeds");
    assert_eq!(updated.applied(), 1);
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    mariadb: {}\n",
            "removed:\n",
            "  - from: mariadb.main\n",
            "    destroy: true\n",
        ),
    )
    .expect("removal configuration fixture is writable");
    let deleted = apply_workspace(&client, &config)
        .await
        .expect("MariaDB delete succeeds");
    assert_eq!(deleted.applied(), 1);

    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    let state = store
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    assert!(state.resource(&"mariadb.main".parse().unwrap()).is_none());
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);

    let requests = server.finish();
    assert_eq!(requests.len(), 15);
    assert!(requests[2].starts_with("POST /api/mariadb.create HTTP/1.1\r\n"));
    assert!(requests[2].contains(r#""databasePassword":"mariadb-user-password-canary""#));
    assert!(requests[2].contains(r#""databaseRootPassword":"mariadb-root-password-canary""#));
    assert!(requests[8].starts_with("POST /api/mariadb.update HTTP/1.1\r\n"));
    assert!(requests[8].contains(r#""databaseName":"app_next""#));
    assert!(requests[8].contains(r#""databaseUser":"app_next""#));
    assert!(!requests[8].contains("databasePassword"));
    assert!(!requests[8].contains("databaseRootPassword"));
    assert!(requests[14].starts_with("POST /api/mariadb.remove HTTP/1.1\r\n"));

    let state_json = fs::read_to_string(directory.path().join(".dokploy/state.json"))
        .expect("state is readable as text");
    assert!(!state_json.contains("mariadb-user-password-canary"));
    assert!(!state_json.contains("mariadb-root-password-canary"));
    assert!(!format!("{created:?} {updated:?} {deleted:?}").contains("password-canary"));
}

#[tokio::test]
async fn mariadb_unknown_create_outcome_keeps_the_journal_step_recoverable() {
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("mariadb-user"), "unknown-mariadb-user-canary")
        .expect("MariaDB user secret fixture is writable");
    fs::write(secrets.join("mariadb-root"), "unknown-mariadb-root-canary")
        .expect("MariaDB root secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    mariadb:\n",
            "      main:\n",
            "        database: app\n",
            "        username: app\n",
            "        password:\n",
            "          file: .secrets/mariadb-user\n",
            "        root_password:\n",
            "          file: .secrets/mariadb-root\n",
        ),
    )
    .expect("configuration fixture is writable");

    let error = apply_workspace(&server.client(), &config)
        .await
        .expect_err("an interrupted create has an unknown outcome");
    assert!(matches!(
        error,
        dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
            code: dokploy_state::FailureCode::TransportOutcomeUnknown
        }
    ));
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    match store.recovery_status().expect("journal scan succeeds") {
        RecoveryStatus::RecoveryRequired(summary) => assert_eq!(
            summary.reason(),
            &dokploy_state::RecoveryReason::StepInProgress
        ),
        RecoveryStatus::Clean => panic!("unknown create outcome must require recovery"),
    }
    let journal = fs::read_dir(directory.path().join(".dokploy/journal"))
        .unwrap()
        .find_map(|entry| {
            let path = entry.ok()?.path();
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                .then(|| fs::read_to_string(path).unwrap())
        })
        .expect("journal exists");
    assert!(!journal.contains("stepFailed"));
    assert!(!journal.contains("unknown-mariadb-user-canary"));
    assert!(!journal.contains("unknown-mariadb-root-canary"));
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn mongo_create_metadata_update_and_delete_are_checkpointed_without_secret_leaks() {
    let project = r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#;
    let environments =
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
    let environment =
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
    let search = r#"{"items":[{"mongoId":"mongo-1","environmentId":"environment-1","name":"main"}],"total":1}"#;
    let old = r#"{"mongoId":"mongo-1","environmentId":"environment-1","name":"main","appName":"mongo-main","dockerImage":"mongo:8","databaseUser":"app","replicaSets":false}"#;
    let updated = r#"{"mongoId":"mongo-1","environmentId":"environment-1","name":"main","appName":"mongo-main","dockerImage":"mongo:8","databaseUser":"app_next","replicaSets":true}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", r#"{"mongoId":"mongo-1"}"#),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", search),
        ("200 OK", old),
        ("200 OK", "true"),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", search),
        ("200 OK", updated),
        ("200 OK", "true"),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("mongo"), "mongo-password-canary")
        .expect("MongoDB secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    let mongo_config = |username: &str, replica_sets: bool| {
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    mongo:\n",
                "      main:\n",
                "        username: {}\n",
                "        password:\n",
                "          file: .secrets/mongo\n",
                "        replica_sets: {}\n",
            ),
            username, replica_sets,
        )
    };
    fs::write(&config, mongo_config("app", false))
        .expect("initial configuration fixture is writable");
    let client = server.client();

    let created = apply_workspace(&client, &config)
        .await
        .expect("initial MongoDB apply succeeds");
    assert_eq!(created.applied(), 3);
    fs::write(&config, mongo_config("app_next", true))
        .expect("updated configuration fixture is writable");
    let updated = apply_workspace(&client, &config)
        .await
        .expect("MongoDB metadata update succeeds");
    assert_eq!(updated.applied(), 1);
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    mongo: {}\n",
            "removed:\n",
            "  - from: mongo.main\n",
            "    destroy: true\n",
        ),
    )
    .expect("removal configuration fixture is writable");
    let deleted = apply_workspace(&client, &config)
        .await
        .expect("MongoDB delete succeeds");
    assert_eq!(deleted.applied(), 1);

    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    let state = store
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    assert!(state.resource(&"mongo.main".parse().unwrap()).is_none());
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);

    let requests = server.finish();
    assert_eq!(requests.len(), 15);
    assert!(requests[2].starts_with("POST /api/mongo.create HTTP/1.1\r\n"));
    assert!(requests[2].contains(r#""databasePassword":"mongo-password-canary""#));
    assert!(requests[2].contains(r#""databaseUser":"app""#));
    assert!(requests[2].contains(r#""replicaSets":false"#));
    assert!(requests[8].starts_with("POST /api/mongo.update HTTP/1.1\r\n"));
    assert!(requests[8].contains(r#""databaseUser":"app_next""#));
    assert!(requests[8].contains(r#""replicaSets":true"#));
    assert!(!requests[8].contains("databasePassword"));
    assert!(requests[14].starts_with("POST /api/mongo.remove HTTP/1.1\r\n"));

    let state_json = fs::read_to_string(directory.path().join(".dokploy/state.json"))
        .expect("state is readable as text");
    assert!(!state_json.contains("mongo-password-canary"));
    assert!(!format!("{created:?} {updated:?} {deleted:?}").contains("password-canary"));
}

#[tokio::test]
async fn mongo_unknown_create_outcome_keeps_the_journal_step_recoverable() {
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("mongo"), "unknown-mongo-canary")
        .expect("MongoDB secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    mongo:\n",
            "      main:\n",
            "        username: app\n",
            "        password:\n",
            "          file: .secrets/mongo\n",
            "        replica_sets: false\n",
        ),
    )
    .expect("configuration fixture is writable");

    let error = apply_workspace(&server.client(), &config)
        .await
        .expect_err("an interrupted create has an unknown outcome");
    assert!(matches!(
        error,
        dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
            code: dokploy_state::FailureCode::TransportOutcomeUnknown
        }
    ));
    assert_recovery_step_in_progress(directory.path(), &server.url);
    let journal = fs::read_dir(directory.path().join(".dokploy/journal"))
        .unwrap()
        .find_map(|entry| {
            let path = entry.ok()?.path();
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                .then(|| fs::read_to_string(path).unwrap())
        })
        .expect("journal exists");
    assert!(!journal.contains("stepFailed"));
    assert!(!journal.contains("unknown-mongo-canary"));
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn libsql_updates_secrets_separately_and_replaces_nodes_delete_before_create() {
    let project = r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#;
    let environments =
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
    let environment =
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
    let empty = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[]}]}"#;
    let primary_collection = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[{"libsqlId":"libsql-1","name":"main","appName":"main","description":"old"}]}]}"#;
    let updated_collection = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[{"libsqlId":"libsql-1","name":"main","appName":"main","description":"next"}]}]}"#;
    let replica_collection = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[{"libsqlId":"libsql-2","name":"main","appName":"main","description":"next"}]}]}"#;
    let primary = r#"{"libsqlId":"libsql-1","environmentId":"environment-1","name":"main","appName":"main","dockerImage":"ghcr.io/tursodatabase/libsql-server:v0.24.32","description":"old","databaseUser":"app","sqldNode":"primary","sqldPrimaryUrl":null}"#;
    let updated = r#"{"libsqlId":"libsql-1","environmentId":"environment-1","name":"main","appName":"main","dockerImage":"ghcr.io/tursodatabase/libsql-server:v0.24.32","description":"next","databaseUser":"next","sqldNode":"primary","sqldPrimaryUrl":null}"#;
    let replica = r#"{"libsqlId":"libsql-2","environmentId":"environment-1","name":"main","appName":"main","dockerImage":"ghcr.io/tursodatabase/libsql-server:v0.24.32","description":"next","databaseUser":"next","sqldNode":"replica","sqldPrimaryUrl":"http://primary.internal"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", empty),
        ("200 OK", "true"),
        ("200 OK", primary_collection),
        ("200 OK", primary),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", primary_collection),
        ("200 OK", primary),
        ("200 OK", "true"),
        ("200 OK", "true"),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", updated_collection),
        ("200 OK", updated),
        ("200 OK", "true"),
        ("200 OK", empty),
        ("200 OK", "true"),
        ("200 OK", replica_collection),
        ("200 OK", replica),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", replica_collection),
        ("200 OK", replica),
        ("200 OK", "true"),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    let password = secrets.join("libsql");
    fs::write(&password, "libsql-old-password-canary").expect("LibSQL secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    let libsql_config = |description: &str, username: &str, node: &str| {
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    libsql:\n",
                "      main:\n",
                "        description: {}\n",
                "        username: {}\n",
                "        password:\n",
                "          file: .secrets/libsql\n",
                "        node:\n",
                "{}",
            ),
            description, username, node,
        )
    };
    fs::write(
        &config,
        libsql_config("old", "app", "          type: primary\n"),
    )
    .expect("initial configuration fixture is writable");
    let client = server.client();

    let created = apply_workspace(&client, &config)
        .await
        .expect("initial LibSQL apply succeeds");
    assert_eq!(created.applied(), 3);

    fs::write(&password, "libsql-next-password-canary")
        .expect("LibSQL password rotation is writable");
    fs::write(
        &config,
        libsql_config("next", "next", "          type: primary\n"),
    )
    .expect("updated configuration fixture is writable");
    let updated_summary = apply_workspace(&client, &config)
        .await
        .expect("LibSQL metadata and password update succeeds");
    assert_eq!(updated_summary.applied(), 1);

    fs::write(
        &config,
        libsql_config(
            "next",
            "next",
            "          type: replica\n          primary_url: http://primary.internal\n",
        ),
    )
    .expect("replacement configuration fixture is writable");
    let replaced = apply_workspace(&client, &config)
        .await
        .expect("LibSQL node replacement succeeds");
    assert_eq!(replaced.applied(), 1);

    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    libsql: {}\n",
            "removed:\n",
            "  - from: libsql.main\n",
            "    destroy: true\n",
        ),
    )
    .expect("removal configuration fixture is writable");
    let deleted = apply_workspace(&client, &config)
        .await
        .expect("LibSQL delete succeeds");
    assert_eq!(deleted.applied(), 1);

    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    let state = store.inspect().unwrap().unwrap();
    assert!(state.resource(&"libsql.main".parse().unwrap()).is_none());
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);

    let requests = server.finish();
    assert_eq!(requests.len(), 29);
    assert!(requests[3].starts_with("POST /api/libsql.create HTTP/1.1\r\n"));
    assert!(requests[3].contains(r#""databasePassword":"libsql-old-password-canary""#));
    assert!(requests[11].starts_with("POST /api/libsql.update HTTP/1.1\r\n"));
    assert!(requests[11].contains(r#""description":"next""#));
    assert!(requests[11].contains(r#""databaseUser":"next""#));
    assert!(!requests[11].contains("databasePassword"));
    assert!(requests[12].starts_with("POST /api/libsql.update HTTP/1.1\r\n"));
    assert!(requests[12].contains(r#""databasePassword":"libsql-next-password-canary""#));
    assert!(requests[18].starts_with("POST /api/libsql.remove HTTP/1.1\r\n"));
    assert!(requests[20].starts_with("POST /api/libsql.create HTTP/1.1\r\n"));
    assert!(requests[20].contains(r#""sqldNode":"replica""#));
    assert!(requests[20].contains(r#""sqldPrimaryUrl":"http://primary.internal""#));
    assert!(requests[28].starts_with("POST /api/libsql.remove HTTP/1.1\r\n"));

    let state_json = fs::read_to_string(directory.path().join(".dokploy/state.json"))
        .expect("state is readable as text");
    let journal_json = fs::read_dir(directory.path().join(".dokploy/journal"))
        .unwrap()
        .filter_map(|entry| fs::read_to_string(entry.ok()?.path()).ok())
        .collect::<String>();
    let debug = format!("{created:?} {updated_summary:?} {replaced:?} {deleted:?}");
    for secret in ["libsql-old-password-canary", "libsql-next-password-canary"] {
        assert!(!state_json.contains(secret));
        assert!(!journal_json.contains(secret));
        assert!(!debug.contains(secret));
    }
}

#[tokio::test]
async fn libsql_replacement_unknown_delete_keeps_the_old_identity_recoverable() {
    let project = r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#;
    let environments =
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
    let environment =
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
    let empty = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[]}]}"#;
    let collection = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[{"libsqlId":"libsql-1","name":"main","appName":"main"}]}]}"#;
    let details = r#"{"libsqlId":"libsql-1","environmentId":"environment-1","name":"main","appName":"main","dockerImage":"ghcr.io/tursodatabase/libsql-server:v0.24.32","databaseUser":"app","sqldNode":"primary","sqldPrimaryUrl":null}"#;
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", empty),
        ("200 OK", "true"),
        ("200 OK", collection),
        ("200 OK", details),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", collection),
        ("200 OK", details),
    ]);
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("password"), "libsql-delete-canary").unwrap();
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project: { name: platform }\n",
            "environments:\n",
            "  production:\n",
            "    libsql:\n",
            "      main:\n",
            "        username: app\n",
            "        password: { file: password }\n",
            "        node: { type: primary }\n",
        ),
    )
    .unwrap();
    let client = server.client();
    apply_workspace(&client, &config).await.unwrap();
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project: { name: platform }\n",
            "environments:\n",
            "  production:\n",
            "    libsql:\n",
            "      main:\n",
            "        username: app\n",
            "        password: { file: password }\n",
            "        node:\n",
            "          type: replica\n",
            "          primary_url: http://primary.internal\n",
        ),
    )
    .unwrap();

    assert_unknown_mutation(apply_workspace(&client, &config).await);
    assert_recovery_step_in_progress(directory.path(), &server.url);
    let state = StateStore::new(
        directory.path(),
        InstanceIdentity::parse(&server.url).unwrap(),
    )
    .unwrap()
    .inspect()
    .unwrap()
    .unwrap();
    assert_eq!(
        state
            .resource(&"libsql.main".parse().unwrap())
            .unwrap()
            .remote_id()
            .as_str(),
        "libsql-1"
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 12);
    assert!(requests[11].starts_with("POST /api/libsql.remove "));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /api/libsql.create "))
            .count(),
        1
    );
}

#[tokio::test]
async fn libsql_replacement_checkpoints_delete_before_an_uncertain_create_preflight() {
    let project = r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#;
    let environments =
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
    let environment =
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
    let empty = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[]}]}"#;
    let collection = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[{"libsqlId":"libsql-1","name":"main","appName":"main"}]}]}"#;
    let details = r#"{"libsqlId":"libsql-1","environmentId":"environment-1","name":"main","appName":"main","dockerImage":"ghcr.io/tursodatabase/libsql-server:v0.24.32","databaseUser":"app","sqldNode":"primary","sqldPrimaryUrl":null}"#;
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", empty),
        ("200 OK", "true"),
        ("200 OK", collection),
        ("200 OK", details),
        ("200 OK", project),
        ("200 OK", environments),
        ("200 OK", environment),
        ("200 OK", collection),
        ("200 OK", details),
        ("200 OK", "true"),
        ("200 OK", empty),
    ]);
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("password"), "libsql-create-canary").unwrap();
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project: { name: platform }\n",
            "environments:\n",
            "  production:\n",
            "    libsql:\n",
            "      main:\n",
            "        username: app\n",
            "        password: { file: password }\n",
            "        node: { type: primary }\n",
        ),
    )
    .unwrap();
    let client = server.client();
    apply_workspace(&client, &config).await.unwrap();
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project: { name: platform }\n",
            "environments:\n",
            "  production:\n",
            "    libsql:\n",
            "      main:\n",
            "        username: app\n",
            "        password: { file: password }\n",
            "        node:\n",
            "          type: replica\n",
            "          primary_url: http://primary.internal\n",
        ),
    )
    .unwrap();

    assert_unknown_mutation(apply_workspace(&client, &config).await);
    assert_recovery_step_in_progress(directory.path(), &server.url);
    let state = StateStore::new(
        directory.path(),
        InstanceIdentity::parse(&server.url).unwrap(),
    )
    .unwrap()
    .inspect()
    .unwrap()
    .unwrap();
    assert!(state.resource(&"libsql.main".parse().unwrap()).is_none());
    let requests = server.finish();
    assert_eq!(requests.len(), 14);
    assert!(requests[11].starts_with("POST /api/libsql.remove "));
    assert!(requests[12].starts_with("GET /api/project.one?"));
    assert!(requests[13].starts_with("POST /api/libsql.create "));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /api/libsql.create "))
            .count(),
        2
    );
}

#[tokio::test]
async fn libsql_post_create_topology_transport_failure_stays_recoverable() {
    let empty = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[]}]}"#;
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", empty),
        ("200 OK", "true"),
    ]);
    let directory = tempfile::tempdir().unwrap();
    let config = write_libsql_primary_config(directory.path(), "post-topology-canary");

    let result = apply_workspace(&server.client(), &config).await;
    assert!(matches!(
        result,
        Err(dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
            code: dokploy_state::FailureCode::TransportOutcomeUnknown
        })
    ));
    assert_recovery_step_in_progress(directory.path(), &server.url);
    let journal = operation_journal(directory.path());
    assert!(!journal.contains("stepFailed"));
    assert!(!journal.contains("post-topology-canary"));
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /api/libsql.create "))
            .count(),
        1
    );
}

#[tokio::test]
async fn libsql_post_create_direct_proof_transport_failure_stays_recoverable() {
    let empty = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[]}]}"#;
    let populated = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[{"libsqlId":"libsql-1","name":"main","appName":"main"}]}]}"#;
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", empty),
        ("200 OK", "true"),
        ("200 OK", populated),
    ]);
    let directory = tempfile::tempdir().unwrap();
    let config = write_libsql_primary_config(directory.path(), "direct-proof-canary");

    let result = apply_workspace(&server.client(), &config).await;
    assert!(matches!(
        result,
        Err(dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
            code: dokploy_state::FailureCode::Internal
        })
    ));
    assert_recovery_step_in_progress(directory.path(), &server.url);
    let journal = operation_journal(directory.path());
    assert!(!journal.contains("stepFailed"));
    assert!(!journal.contains("direct-proof-canary"));
    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert!(requests[5].starts_with("GET /api/libsql.one?"));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /api/libsql.create "))
            .count(),
        1
    );
}

#[tokio::test]
async fn libsql_definite_create_rejection_closes_the_journal_step() {
    let empty = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"projectId":"project-1","libsql":[]}]}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", empty),
        ("400 Bad Request", r#"{"message":"rejected"}"#),
    ]);
    let directory = tempfile::tempdir().unwrap();
    let config = write_libsql_primary_config(directory.path(), "rejected-create-canary");

    let result = apply_workspace(&server.client(), &config).await;
    assert!(matches!(
        result,
        Err(dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
            code: dokploy_state::FailureCode::Validation
        })
    ));
    let store = StateStore::new(
        directory.path(),
        InstanceIdentity::parse(&server.url).unwrap(),
    )
    .unwrap();
    match store.recovery_status().unwrap() {
        RecoveryStatus::RecoveryRequired(summary) => assert_eq!(
            summary.reason(),
            &dokploy_state::RecoveryReason::Failed(dokploy_state::FailureCode::Validation)
        ),
        RecoveryStatus::Clean => panic!("a rejected create must leave failed recovery evidence"),
    }
    let journal = operation_journal(directory.path());
    assert!(journal.contains("stepFailed"));
    assert!(!journal.contains("rejected-create-canary"));
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn mysql_unknown_create_outcome_keeps_the_journal_step_recoverable() {
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("mysql-user"), "unknown-user-password-canary")
        .expect("MySQL user secret fixture is writable");
    fs::write(secrets.join("mysql-root"), "unknown-root-password-canary")
        .expect("MySQL root secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    mysql:\n",
            "      main:\n",
            "        database: app\n",
            "        username: app\n",
            "        password:\n",
            "          file: .secrets/mysql-user\n",
            "        root_password:\n",
            "          file: .secrets/mysql-root\n",
        ),
    )
    .expect("configuration fixture is writable");

    let error = apply_workspace(&server.client(), &config)
        .await
        .expect_err("an interrupted create has an unknown outcome");
    assert!(matches!(
        error,
        dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
            code: dokploy_state::FailureCode::TransportOutcomeUnknown
        }
    ));
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    match store.recovery_status().expect("journal scan succeeds") {
        RecoveryStatus::RecoveryRequired(summary) => assert_eq!(
            summary.reason(),
            &dokploy_state::RecoveryReason::StepInProgress
        ),
        RecoveryStatus::Clean => panic!("unknown create outcome must require recovery"),
    }
    assert!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&"mysql.main".parse().unwrap())
            .is_none()
    );
    let journal = fs::read_dir(directory.path().join(".dokploy/journal"))
        .unwrap()
        .find_map(|entry| {
            let path = entry.ok()?.path();
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                .then(|| fs::read_to_string(path).unwrap())
        })
        .expect("journal exists");
    assert!(!journal.contains("stepFailed"));
    assert!(!journal.contains("unknown-user-password-canary"));
    assert!(!journal.contains("unknown-root-password-canary"));
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn mysql_invalid_create_identity_keeps_the_journal_step_recoverable() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", r#"{"mysqlId":""}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(
        secrets.join("mysql-user"),
        "invalid-id-user-password-canary",
    )
    .expect("MySQL user secret fixture is writable");
    fs::write(
        secrets.join("mysql-root"),
        "invalid-id-root-password-canary",
    )
    .expect("MySQL root secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    mysql:\n",
            "      main:\n",
            "        database: app\n",
            "        username: app\n",
            "        password:\n",
            "          file: .secrets/mysql-user\n",
            "        root_password:\n",
            "          file: .secrets/mysql-root\n",
        ),
    )
    .expect("configuration fixture is writable");

    let error = apply_workspace(&server.client(), &config)
        .await
        .expect_err("an invalid identity cannot prove whether create succeeded");
    assert!(matches!(
        error,
        dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
            code: dokploy_state::FailureCode::Internal
        }
    ));
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    match store.recovery_status().expect("journal scan succeeds") {
        RecoveryStatus::RecoveryRequired(summary) => assert_eq!(
            summary.reason(),
            &dokploy_state::RecoveryReason::StepInProgress
        ),
        RecoveryStatus::Clean => panic!("unusable create identity must require recovery"),
    }
    let journal = fs::read_dir(directory.path().join(".dokploy/journal"))
        .unwrap()
        .find_map(|entry| {
            let path = entry.ok()?.path();
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                .then(|| fs::read_to_string(path).unwrap())
        })
        .expect("journal exists");
    assert!(!journal.contains("stepFailed"));
    assert!(!journal.contains("invalid-id-user-password-canary"));
    assert!(!journal.contains("invalid-id-root-password-canary"));
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn serial_create_unknown_outcome_keeps_the_journal_step_recoverable() {
    let server = TestServer::respond_then_drop(vec![("200 OK", "[]")]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(&config, "version: 1\nproject:\n  name: platform\n")
        .expect("configuration fixture is writable");

    assert_unknown_mutation(apply_workspace(&server.client(), &config).await);
    assert_recovery_step_in_progress(directory.path(), &server.url);
    assert_eq!(server.finish().len(), 2);
}

#[tokio::test]
async fn serial_update_unknown_outcome_keeps_the_journal_step_recoverable() {
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","description":"before","environments":[]}]"#,
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\n  description: before\n",
    )
    .expect("configuration fixture is writable");
    apply_workspace(&server.client(), &config)
        .await
        .expect("initial apply succeeds");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\n  description: after\n",
    )
    .expect("updated configuration fixture is writable");

    assert_unknown_mutation(apply_workspace(&server.client(), &config).await);
    assert_recovery_step_in_progress(directory.path(), &server.url);
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn serial_delete_unknown_outcome_keeps_the_journal_step_recoverable() {
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(&config, "version: 1\nproject:\n  name: platform\n")
        .expect("configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial apply succeeds");

    let result = destroy_workspace_with_approval(&client, &config, |_| Ok(true)).await;
    assert!(
        matches!(
            result,
            Err(dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
                code: dokploy_state::FailureCode::TransportOutcomeUnknown
            })
        ),
        "unexpected destroy result: {result:?}"
    );
    assert_recovery_step_in_progress(directory.path(), &server.url);
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn deploy_unknown_outcome_keeps_the_journal_step_recoverable() {
    let server = TestServer::respond_then_drop(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/application-create.owner.json"),
        ),
        ("200 OK", r#"{"ok":true}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    applications:\n",
            "      api:\n",
            "        replicas: 1\n",
        ),
    )
    .expect("configuration fixture is writable");

    assert_unknown_mutation(apply_workspace(&server.client(), &config).await);
    assert_recovery_step_in_progress(directory.path(), &server.url);
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn redis_password_rotation_uses_the_new_one_shot_secret_value() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/redis-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"redisId":"redis-1","environmentId":"environment-1","name":"cache"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"redisId":"redis-1","environmentId":"environment-1","name":"cache","appName":"redis-cache","dockerImage":"redis:8"}"#,
        ),
        ("200 OK", r#"{"ok":true}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    let password = secrets.join("redis");
    fs::write(&password, "old-password-canary").expect("Redis secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    redis:\n",
            "      cache:\n",
            "        password:\n",
            "          file: .secrets/redis\n",
        ),
    )
    .expect("configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial Redis apply succeeds");
    fs::write(&password, "new-password-canary").expect("Redis secret rotation is writable");

    let summary = apply_workspace(&client, &config)
        .await
        .expect("Redis password rotation succeeds");

    assert_eq!(summary.applied(), 1);
    let requests = server.finish();
    assert_eq!(requests.len(), 9);
    assert!(requests[8].starts_with("POST /api/redis.update HTTP/1.1\r\n"));
    assert!(requests[8].contains(r#""databasePassword":"new-password-canary""#));
    assert!(!format!("{summary:?}").contains("new-password-canary"));
}

#[tokio::test]
async fn application_environment_rotation_preserves_unowned_remote_entries() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/application-create.owner.json"),
        ),
        ("200 OK", r#"{"ok":true}"#),
        ("200 OK", r#"{"ok":true}"#),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api","env":"TOKEN=old-password-canary"}"#,
        ),
        (
            "200 OK",
            "{\"applicationId\":\"application-1\",\"env\":\"UNMANAGED=keep\\nTOKEN=old-password-canary\"}",
        ),
        ("200 OK", r#"{"ok":true}"#),
        ("200 OK", r#"{"ok":true}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    let token = secrets.join("token");
    fs::write(&token, "old-password-canary").expect("application secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    applications:\n",
            "      api:\n",
            "        environment:\n",
            "          TOKEN:\n",
            "            secret:\n",
            "              file: .secrets/token\n",
        ),
    )
    .expect("configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial application environment apply succeeds");
    fs::write(&token, "new-password-canary").expect("application secret rotation is writable");

    let summary = apply_workspace(&client, &config)
        .await
        .expect("application environment rotation succeeds");

    assert_eq!(summary.applied(), 1);
    let requests = server.finish();
    assert_eq!(requests.len(), 13);
    assert!(requests[10].starts_with("GET /api/application.one?applicationId=application-1"));
    assert!(requests[11].starts_with("POST /api/application.update HTTP/1.1\r\n"));
    let body: serde_json::Value = serde_json::from_str(
        requests[11]
            .split_once("\r\n\r\n")
            .expect("request contains a body")
            .1,
    )
    .expect("request body is JSON");
    assert_eq!(body["env"], "UNMANAGED=keep\nTOKEN=new-password-canary");
    assert!(requests[12].starts_with("POST /api/application.deploy HTTP/1.1\r\n"));
    assert!(!format!("{summary:?}").contains("new-password-canary"));
}

impl TestServer {
    fn respond_in_sequence(responses: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received = Vec::new();

            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                received.push(read_request(&mut stream));
                write_response(&mut stream, status, body);
            }

            sender.send(received).expect("test receives requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn respond_then_drop(responses: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                received.push(read_request(&mut stream));
                write_response(&mut stream, status, body);
            }
            let (mut stream, _) = listener
                .accept()
                .expect("test server accepts the interrupted mutation");
            received.push(read_request(&mut stream));
            drop(stream);
            sender.send(received).expect("test receives requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn client(&self) -> Dokploy {
        Dokploy::builder()
            .url(&self.url)
            .api_key("test-api-key")
            .build()
            .expect("client configuration is valid")
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("test server exits cleanly");
        requests
    }
}

fn assert_unknown_mutation<T: std::fmt::Debug>(
    result: Result<T, dokploy_cli::executor::ApplyWorkspaceError>,
) {
    assert!(
        matches!(
            result,
            Err(dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
                code: dokploy_state::FailureCode::TransportOutcomeUnknown
            })
        ),
        "unexpected mutation result: {result:?}"
    );
}

fn assert_recovery_step_in_progress(workspace: &std::path::Path, server_url: &str) {
    let instance = InstanceIdentity::parse(server_url).expect("instance is valid");
    let store = StateStore::new(workspace, instance).expect("state store is valid");
    match store.recovery_status().expect("journal scan succeeds") {
        RecoveryStatus::RecoveryRequired(summary) => assert_eq!(
            summary.reason(),
            &dokploy_state::RecoveryReason::StepInProgress
        ),
        RecoveryStatus::Clean => panic!("unknown mutation outcome must require recovery"),
    }
}

fn write_libsql_primary_config(workspace: &std::path::Path, secret: &str) -> std::path::PathBuf {
    fs::write(workspace.join("password"), secret).unwrap();
    let config = workspace.join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project: { name: platform }\n",
            "environments:\n",
            "  production:\n",
            "    libsql:\n",
            "      main:\n",
            "        username: app\n",
            "        password: { file: password }\n",
            "        node: { type: primary }\n",
        ),
    )
    .unwrap();

    config
}

fn operation_journal(workspace: &std::path::Path) -> String {
    fs::read_dir(workspace.join(".dokploy/journal"))
        .unwrap()
        .find_map(|entry| {
            let path = entry.ok()?.path();
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                .then(|| fs::read_to_string(path).unwrap())
        })
        .expect("journal exists")
}

impl ConcurrentDatabaseServer {
    fn with_failing_postgres() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, result) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received = Vec::new();
            for (status, body) in [
                ("200 OK", "[]"),
                (
                    "200 OK",
                    include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
                ),
            ] {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                received.push(read_request(&mut stream));
                write_response(&mut stream, status, body);
            }

            listener
                .set_nonblocking(true)
                .expect("listener becomes nonblocking");
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut database_requests = Vec::new();
            while database_requests.len() < 4 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_request(&mut stream);
                        database_requests.push((stream, request));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("test server accepts database request: {error}"),
                }
            }
            let overlapped = database_requests.len() == 4;
            database_requests
                .sort_by_key(|(_, request)| request.starts_with("POST /api/postgres.create "));
            for (mut stream, request) in database_requests {
                if request.starts_with("POST /api/postgres.create ") {
                    write_response(
                        &mut stream,
                        "500 Internal Server Error",
                        r#"{"error":"failed"}"#,
                    );
                } else if request.starts_with("POST /api/redis.create ") {
                    write_response(
                        &mut stream,
                        "200 OK",
                        include_str!("../../../fixtures/api/live/v0.30.6/redis-create.owner.json"),
                    );
                } else if request.starts_with("POST /api/mariadb.create ") {
                    write_response(
                        &mut stream,
                        "200 OK",
                        include_str!(
                            "../../../fixtures/api/live/v0.30.6/mariadb-create.owner.json"
                        ),
                    );
                } else if request.starts_with("POST /api/mongo.create ") {
                    write_response(
                        &mut stream,
                        "200 OK",
                        include_str!("../../../fixtures/api/live/v0.30.6/mongo-create.owner.json"),
                    );
                } else {
                    write_response(&mut stream, "404 Not Found", r#"{"error":"unexpected"}"#);
                }
                received.push(request);
            }

            sender
                .send((received, overlapped))
                .expect("test receives result");
        });

        Self {
            url: format!("http://{address}"),
            result,
            thread,
        }
    }

    fn client(&self) -> Dokploy {
        Dokploy::builder()
            .url(&self.url)
            .api_key("test-api-key")
            .build()
            .expect("client configuration is valid")
    }

    fn finish(self) -> (Vec<String>, bool) {
        let result = self.result.recv().expect("test receives result");
        self.thread.join().expect("test server exits cleanly");
        result
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];

    loop {
        let count = stream.read(&mut buffer).expect("request is readable");
        bytes.extend_from_slice(&buffer[..count]);
        if count == 0 || request_is_complete(&bytes) {
            break;
        }
    }

    String::from_utf8(bytes).expect("request is UTF-8")
}

fn write_response(stream: &mut TcpStream, status: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .expect("response is writable");
}

fn request_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();

    bytes.len() >= header_end + 4 + content_length
}

#[tokio::test]
async fn project_apply_checkpoints_and_the_next_plan_converges() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","description":"Managed","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\n  description: Managed\nenvironments: {}\n",
    )
    .expect("configuration fixture is writable");
    let client = server.client();

    let summary = apply_workspace(&client, &config)
        .await
        .expect("project apply succeeds");
    let converged = plan_workspace(&client, &config)
        .await
        .expect("post-apply plan succeeds");

    assert_eq!(summary.applied(), 1);
    assert!(converged.changes().is_empty());
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    let state = store
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    let project: ResourceAddress = "project.platform".parse().expect("address is valid");
    assert_eq!(
        state
            .resource(&project)
            .expect("project is checkpointed")
            .remote_id()
            .as_str(),
        "project-1"
    );
    assert_eq!(state.serial(), 1);
    assert_eq!(
        store.recovery_status().expect("journal scan succeeds"),
        RecoveryStatus::Clean
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /api/project.all HTTP/1.1\r\n"));
    assert!(requests[1].starts_with("POST /api/project.create HTTP/1.1\r\n"));
    assert!(requests[2].starts_with("GET /api/project.all HTTP/1.1\r\n"));
}

#[tokio::test]
async fn project_default_environment_is_checkpointed_without_a_duplicate_create() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production: {}\n",
    )
    .expect("configuration fixture is writable");
    let client = server.client();

    let summary = apply_workspace(&client, &config)
        .await
        .expect("project and default environment apply succeeds");

    assert_eq!(summary.applied(), 2);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let state = StateStore::new(directory.path(), instance)
        .expect("state store is valid")
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    let environment: ResourceAddress = "environment.production".parse().expect("address is valid");
    assert_eq!(
        state
            .resource(&environment)
            .expect("default environment is checkpointed")
            .remote_id()
            .as_str(),
        "environment-1"
    );
    assert_eq!(state.serial(), 2);

    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/project.all HTTP/1.1\r\n"));
    assert!(requests[1].starts_with("POST /api/project.create HTTP/1.1\r\n"));
}

#[tokio::test]
async fn project_default_environment_is_configured_after_adoption() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        ("200 OK", r#"{"ok":true}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    description: Managed\n",
        ),
    )
    .expect("configuration fixture is writable");

    let summary = apply_workspace(&server.client(), &config)
        .await
        .expect("default environment configuration succeeds");

    assert_eq!(summary.applied(), 2);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let state = StateStore::new(directory.path(), instance)
        .expect("state store is valid")
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    assert_eq!(state.serial(), 3);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].starts_with("POST /api/environment.update HTTP/1.1\r\n"));
    assert!(requests[2].contains(r#""description":"Managed""#));
}

#[tokio::test]
async fn non_default_environment_is_created_under_the_checkpointed_project() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-2","projectId":"project-1","name":"staging"}"#,
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\nenvironments:\n  staging: {}\n",
    )
    .expect("configuration fixture is writable");

    let summary = apply_workspace(&server.client(), &config)
        .await
        .expect("project and non-default environment apply succeeds");

    assert_eq!(summary.applied(), 2);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let state = StateStore::new(directory.path(), instance)
        .expect("state store is valid")
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    let environment: ResourceAddress = "environment.staging".parse().expect("address is valid");
    assert_eq!(
        state
            .resource(&environment)
            .expect("environment is checkpointed")
            .remote_id()
            .as_str(),
        "environment-2"
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].starts_with("POST /api/environment.create HTTP/1.1\r\n"));
    assert!(requests[2].contains(r#""projectId":"project-1""#));
}

#[tokio::test]
async fn application_is_created_under_the_checkpointed_environment() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/application-create.owner.json"),
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    applications:\n",
            "      api: {}\n",
        ),
    )
    .expect("configuration fixture is writable");

    let summary = apply_workspace(&server.client(), &config)
        .await
        .expect("application apply succeeds");

    assert_eq!(summary.applied(), 3);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let state = StateStore::new(directory.path(), instance)
        .expect("state store is valid")
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    let application: ResourceAddress = "application.api".parse().expect("address is valid");
    assert_eq!(
        state
            .resource(&application)
            .expect("application is checkpointed")
            .remote_id()
            .as_str(),
        "application-1"
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].starts_with("POST /api/application.create HTTP/1.1\r\n"));
    assert!(requests[2].contains(r#""environmentId":"environment-1""#));
}

#[tokio::test]
async fn all_mvp_resources_are_created_and_checkpointed_in_dependency_order() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/application-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/domain-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/postgres-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/redis-create.owner.json"),
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("postgres"), "postgres-password-canary")
        .expect("Postgres secret fixture is writable");
    fs::write(secrets.join("redis"), "redis-password-canary")
        .expect("Redis secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    applications:\n",
            "      api: {}\n",
            "    postgres:\n",
            "      main:\n",
            "        database: app\n",
            "        username: app\n",
            "        password:\n",
            "          file: .secrets/postgres\n",
            "    redis:\n",
            "      cache:\n",
            "        password:\n",
            "          file: .secrets/redis\n",
            "    domains:\n",
            "      public:\n",
            "        host: api.example.test\n",
            "        application: application.api\n",
        ),
    )
    .expect("configuration fixture is writable");

    let summary = apply_workspace(&server.client(), &config)
        .await
        .expect("the complete MVP create graph applies");

    assert_eq!(summary.applied(), 6);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let state = StateStore::new(directory.path(), instance)
        .expect("state store is valid")
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    for (address, remote_id) in [
        ("project.platform", "project-1"),
        ("environment.production", "environment-1"),
        ("application.api", "application-1"),
        ("postgres.main", "postgres-1"),
        ("redis.cache", "redis-1"),
        ("domain.public", "domain-1"),
    ] {
        let address: ResourceAddress = address.parse().expect("address is valid");
        assert_eq!(
            state
                .resource(&address)
                .expect("resource is checkpointed")
                .remote_id()
                .as_str(),
            remote_id
        );
    }
    assert_eq!(state.serial(), 6);

    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert!(requests[2].starts_with("POST /api/application.create HTTP/1.1\r\n"));
    assert!(requests[3].starts_with("POST /api/domain.create HTTP/1.1\r\n"));
    assert!(requests[3].contains(r#""applicationId":"application-1""#));
    assert!(requests[4].starts_with("POST /api/postgres.create HTTP/1.1\r\n"));
    assert!(requests[4].contains(r#""databasePassword":"postgres-password-canary""#));
    assert!(requests[5].starts_with("POST /api/redis.create HTTP/1.1\r\n"));
    assert!(requests[5].contains(r#""databasePassword":"redis-password-canary""#));
}

#[tokio::test]
async fn independent_database_mutations_overlap_and_checkpoint_a_successful_sibling() {
    let server = ConcurrentDatabaseServer::with_failing_postgres();
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("postgres"), "postgres-password")
        .expect("Postgres secret fixture is writable");
    fs::write(secrets.join("redis"), "redis-password").expect("Redis secret fixture is writable");
    fs::write(secrets.join("mariadb"), "mariadb-password")
        .expect("MariaDB secret fixture is writable");
    fs::write(secrets.join("mongo"), "mongo-password").expect("MongoDB secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    postgres:\n",
            "      main:\n",
            "        database: app\n",
            "        username: app\n",
            "        password:\n",
            "          file: .secrets/postgres\n",
            "    redis:\n",
            "      cache:\n",
            "        password:\n",
            "          file: .secrets/redis\n",
            "    mariadb:\n",
            "      records:\n",
            "        database: records\n",
            "        username: records\n",
            "        password:\n",
            "          file: .secrets/mariadb\n",
            "    mongo:\n",
            "      documents:\n",
            "        username: documents\n",
            "        password:\n",
            "          file: .secrets/mongo\n",
            "        replica_sets: false\n",
        ),
    )
    .expect("configuration fixture is writable");

    let error = apply_workspace_with_approval(
        &server.client(),
        &config,
        ApplyOptions::new(4).expect("parallelism is valid"),
        |_| Ok(true),
    )
    .await
    .expect_err("the Postgres failure must fail the operation");

    assert!(matches!(
        error,
        dokploy_cli::executor::ApplyWorkspaceError::RemoteMutation {
            code: dokploy_state::FailureCode::RemoteRejected
        }
    ));
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    let state = store
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    let postgres: ResourceAddress = "postgres.main".parse().expect("address is valid");
    let redis: ResourceAddress = "redis.cache".parse().expect("address is valid");
    let mariadb: ResourceAddress = "mariadb.records".parse().expect("address is valid");
    let mongo: ResourceAddress = "mongo.documents".parse().expect("address is valid");
    assert!(state.resource(&postgres).is_none());
    assert_eq!(
        state
            .resource(&redis)
            .expect("successful sibling is checkpointed")
            .remote_id()
            .as_str(),
        "redis-1"
    );
    assert_eq!(
        state
            .resource(&mariadb)
            .expect("successful MariaDB sibling is checkpointed")
            .remote_id()
            .as_str(),
        "mariadb-1"
    );
    assert_eq!(
        state
            .resource(&mongo)
            .expect("successful MongoDB sibling is checkpointed")
            .remote_id()
            .as_str(),
        "mongo-1"
    );
    match store.recovery_status().expect("journal scan succeeds") {
        RecoveryStatus::RecoveryRequired(summary) => assert_eq!(
            summary.reason(),
            &dokploy_state::RecoveryReason::Failed(dokploy_state::FailureCode::RemoteRejected)
        ),
        RecoveryStatus::Clean => panic!("failed apply must require recovery"),
    }

    let (requests, overlapped) = server.finish();
    assert!(
        overlapped,
        "all database requests must be in flight together"
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /api/postgres.create "))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /api/redis.create "))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /api/mariadb.create "))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /api/mongo.create "))
    );
}

#[tokio::test]
async fn database_create_without_required_inputs_is_blocked_before_mutation() {
    let server = TestServer::respond_in_sequence(vec![("200 OK", "[]")]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    postgres:\n",
            "      main: {}\n",
        ),
    )
    .expect("configuration fixture is writable");

    let error = apply_workspace(&server.client(), &config)
        .await
        .expect_err("an incomplete database create must be rejected");

    assert!(matches!(
        error,
        dokploy_cli::executor::ApplyWorkspaceError::PlanBlocked
    ));
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /api/project.all HTTP/1.1\r\n"));
}

#[tokio::test]
async fn configured_application_is_checkpointed_then_configured_and_deployed() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/application-create.owner.json"),
        ),
        ("200 OK", r#"{"ok":true}"#),
        ("200 OK", r#"{"ok":true}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let secrets = directory.path().join(".secrets");
    fs::create_dir(&secrets).expect("secret fixture directory is writable");
    fs::write(secrets.join("token"), "secret-canary")
        .expect("application secret fixture is writable");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    applications:\n",
            "      api:\n",
            "        description: API\n",
            "        replicas: 2\n",
            "        source:\n",
            "          type: github\n",
            "          repository: legalterlaw/platform\n",
            "          branch: main\n",
            "        environment:\n",
            "          TOKEN:\n",
            "            secret:\n",
            "              file: .secrets/token\n",
        ),
    )
    .expect("configuration fixture is writable");

    let summary = apply_workspace(&server.client(), &config)
        .await
        .expect("configured application apply succeeds");

    assert_eq!(summary.applied(), 3);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let state = StateStore::new(directory.path(), instance)
        .expect("state store is valid")
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    assert_eq!(state.serial(), 5);
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert!(requests[2].starts_with("POST /api/application.create HTTP/1.1\r\n"));
    assert!(requests[3].starts_with("POST /api/application.update HTTP/1.1\r\n"));
    assert!(requests[3].contains(r#""description":"API""#));
    assert!(requests[3].contains(r#""replicas":2"#));
    assert!(requests[3].contains(r#""repository":"legalterlaw/platform""#));
    assert!(requests[3].contains(r#""branch":"main""#));
    assert!(requests[3].contains(r#""env":"TOKEN=secret-canary""#));
    assert!(requests[4].starts_with("POST /api/application.deploy HTTP/1.1\r\n"));
}

#[tokio::test]
async fn changed_application_configuration_updates_then_deploys_without_recreating() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/application-create.owner.json"),
        ),
        ("200 OK", r#"{"ok":true}"#),
        ("200 OK", r#"{"ok":true}"#),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api","replicas":1,"sourceType":"github","repository":"legalterlaw/platform","branch":"main"}"#,
        ),
        ("200 OK", r#"{"ok":true}"#),
        ("200 OK", r#"{"ok":true}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    let application_config = |replicas: u32, branch: &str| {
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    applications:\n",
                "      api:\n",
                "        replicas: {}\n",
                "        source:\n",
                "          type: github\n",
                "          repository: legalterlaw/platform\n",
                "          branch: {}\n",
            ),
            replicas, branch,
        )
    };
    fs::write(&config, application_config(1, "main"))
        .expect("initial configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial application apply succeeds");
    fs::write(&config, application_config(2, "next"))
        .expect("updated configuration fixture is writable");

    let summary = apply_workspace(&client, &config)
        .await
        .expect("application update succeeds");

    assert_eq!(summary.applied(), 1);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let state = StateStore::new(directory.path(), instance)
        .expect("state store is valid")
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    assert_eq!(state.serial(), 7);
    let requests = server.finish();
    assert_eq!(requests.len(), 12);
    assert!(requests[10].starts_with("POST /api/application.update HTTP/1.1\r\n"));
    assert!(requests[10].contains(r#""replicas":2"#));
    assert!(requests[10].contains(r#""branch":"next""#));
    assert!(!requests[10].contains("application.create"));
    assert!(requests[11].starts_with("POST /api/application.deploy HTTP/1.1\r\n"));
}

#[tokio::test]
async fn project_update_checkpoints_and_the_next_plan_converges() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","description":"Old","environments":[]}]"#,
        ),
        ("200 OK", r#"{"ok":true}"#),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","description":"New","environments":[]}]"#,
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\n  description: Old\nenvironments: {}\n",
    )
    .expect("initial configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial project apply succeeds");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\n  description: New\nenvironments: {}\n",
    )
    .expect("updated configuration fixture is writable");

    let summary = apply_workspace(&client, &config)
        .await
        .expect("project update succeeds");
    let converged = plan_workspace(&client, &config)
        .await
        .expect("post-update plan succeeds");

    assert_eq!(summary.applied(), 1);
    assert!(converged.changes().is_empty());
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert!(requests[3].starts_with("POST /api/project.update HTTP/1.1\r\n"));
    assert!(requests[3].contains(r#""description":"New""#));
}

#[tokio::test]
async fn state_only_project_move_preserves_identity_and_protection_without_remote_mutation() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "  lifecycle:\n    protect: true\n",
            "environments: {}\n",
        ),
    )
    .unwrap();
    let client = server.client();
    apply_workspace(&client, &config).await.unwrap();
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: renamed\n",
            "  lifecycle:\n    protect: true\n",
            "environments: {}\n",
            "moves:\n",
            "  - from: project.platform\n",
            "    to: project.renamed\n",
        ),
    )
    .unwrap();

    let summary = apply_workspace(&client, &config).await.unwrap();
    let converged = plan_workspace(&client, &config).await.unwrap();

    assert_eq!(summary.applied(), 1);
    assert!(converged.changes().is_empty());
    let state = StateStore::new(
        directory.path(),
        InstanceIdentity::parse(&server.url).unwrap(),
    )
    .unwrap()
    .inspect()
    .unwrap()
    .unwrap();
    assert!(
        state
            .resource(&"project.platform".parse().unwrap())
            .is_none()
    );
    let moved = state.resource(&"project.renamed".parse().unwrap()).unwrap();
    assert_eq!(moved.remote_id().as_str(), "project-1");
    assert!(moved.is_protected());
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /api/project.create "))
            .count(),
        1
    );
    assert!(requests.iter().all(|request| {
        !request.starts_with("POST /api/project.remove ")
            && !request.starts_with("POST /api/project.update ")
    }));
}

#[tokio::test]
async fn project_move_with_remote_update_updates_once_then_converges() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","description":"Old","environments":[]}]"#,
        ),
        ("200 OK", r#"{"ok":true}"#),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","description":"New","environments":[]}]"#,
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\n  description: Old\nenvironments: {}\n",
    )
    .unwrap();
    let client = server.client();
    apply_workspace(&client, &config).await.unwrap();
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n  name: renamed\n  description: New\n",
            "environments: {}\n",
            "moves:\n",
            "  - from: project.platform\n",
            "    to: project.renamed\n",
        ),
    )
    .unwrap();

    let summary = apply_workspace(&client, &config).await.unwrap();
    let converged = plan_workspace(&client, &config).await.unwrap();

    assert_eq!(summary.applied(), 1);
    assert!(converged.changes().is_empty());
    let state = StateStore::new(
        directory.path(),
        InstanceIdentity::parse(&server.url).unwrap(),
    )
    .unwrap()
    .inspect()
    .unwrap()
    .unwrap();
    let moved = state.resource(&"project.renamed".parse().unwrap()).unwrap();
    assert_eq!(moved.remote_id().as_str(), "project-1");
    assert_eq!(moved.last_applied().as_json()["description"], "New");
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /api/project.update "))
            .count(),
        1
    );
    assert!(requests[3].contains(r#""projectId":"project-1""#));
    assert!(requests[3].contains(r#""description":"New""#));
    assert!(
        requests
            .iter()
            .all(|request| !request.starts_with("POST /api/project.remove "))
    );
}

#[tokio::test]
async fn domain_host_update_uses_the_typed_in_place_mutation() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/application-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/domain-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            include_str!(
                "../../../fixtures/api/live/v0.30.6/application-one.domain-created.owner.json"
            ),
        ),
        (
            "200 OK",
            include_str!(
                "../../../fixtures/api/live/v0.30.6/domain-by-application.created.owner.json"
            ),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/domain-one.created.owner.json"),
        ),
        ("200 OK", r#"{"ok":true}"#),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    let domain_config = |host: &str| {
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    applications:\n",
                "      api: {}\n",
                "    domains:\n",
                "      public:\n",
                "        host: {}\n",
                "        application: application.api\n",
            ),
            "{}", host,
        )
    };
    fs::write(&config, domain_config("api.example.test"))
        .expect("initial configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial domain apply succeeds");
    fs::write(&config, domain_config("next.example.test"))
        .expect("updated configuration fixture is writable");

    let summary = apply_workspace(&client, &config)
        .await
        .expect("domain update succeeds");

    assert_eq!(summary.applied(), 1);
    let requests = server.finish();
    assert_eq!(requests.len(), 12);
    assert!(requests[11].starts_with("POST /api/domain.update HTTP/1.1\r\n"));
    assert!(requests[11].contains(r#""host":"next.example.test""#));
}

#[tokio::test]
async fn protection_change_is_a_state_only_checkpoint_without_remote_mutation() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
    ]);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\nenvironments: {}\n",
    )
    .expect("initial configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial project apply succeeds");
    fs::write(
        &config,
        concat!(
            "version: 1\n",
            "project:\n",
            "  name: platform\n",
            "  lifecycle:\n",
            "    protect: true\n",
            "environments: {}\n",
        ),
    )
    .expect("protected configuration fixture is writable");

    let summary = apply_workspace(&client, &config)
        .await
        .expect("protection checkpoint succeeds");

    assert_eq!(summary.applied(), 1);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let state = StateStore::new(directory.path(), instance)
        .expect("state store is valid")
        .inspect()
        .expect("state is readable")
        .expect("state was initialized");
    let project: ResourceAddress = "project.platform".parse().expect("address is valid");
    assert!(
        state
            .resource(&project)
            .expect("project is checkpointed")
            .is_protected()
    );
    assert_eq!(state.serial(), 2);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("project.update"))
    );
}

#[tokio::test]
async fn removed_domain_is_deleted_and_forgotten_durably() {
    let server = TestServer::respond_in_sequence(domain_removal_responses(true));
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(&config, domain_configuration(None))
        .expect("initial configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial domain apply succeeds");
    fs::write(&config, domain_configuration(Some(true)))
        .expect("removal configuration fixture is writable");

    let summary = apply_workspace(&client, &config)
        .await
        .expect("domain deletion succeeds");

    assert_eq!(summary.applied(), 1);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    let state = store
        .inspect()
        .expect("state is readable")
        .expect("state remains initialized");
    let domain: ResourceAddress = "domain.public".parse().expect("address is valid");
    assert!(state.resource(&domain).is_none());
    assert_eq!(
        store.recovery_status().expect("journal scan succeeds"),
        RecoveryStatus::Clean
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 12);
    assert!(requests[11].starts_with("POST /api/domain.delete HTTP/1.1\r\n"));
    assert!(requests[11].contains(r#""domainId":"domain-1""#));
}

#[tokio::test]
async fn retained_domain_is_forgotten_without_a_remote_mutation() {
    let server = TestServer::respond_in_sequence(domain_removal_responses(false));
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(&config, domain_configuration(None))
        .expect("initial configuration fixture is writable");
    let client = server.client();
    apply_workspace(&client, &config)
        .await
        .expect("initial domain apply succeeds");
    fs::write(&config, domain_configuration(Some(false)))
        .expect("retain configuration fixture is writable");

    let summary = apply_workspace(&client, &config)
        .await
        .expect("state-only forget succeeds");

    assert_eq!(summary.applied(), 1);
    let instance = InstanceIdentity::parse(&server.url).expect("instance is valid");
    let store = StateStore::new(directory.path(), instance).expect("state store is valid");
    let state = store
        .inspect()
        .expect("state is readable")
        .expect("state remains initialized");
    let domain: ResourceAddress = "domain.public".parse().expect("address is valid");
    assert!(state.resource(&domain).is_none());
    assert_eq!(
        store.recovery_status().expect("journal scan succeeds"),
        RecoveryStatus::Clean
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 11);
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("domain.delete"))
    );
}

fn domain_removal_responses(delete: bool) -> Vec<(&'static str, &'static str)> {
    let mut responses = vec![
        ("200 OK", "[]"),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/project-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/application-create.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/domain-create.owner.json"),
        ),
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}]}]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api"}"#,
        ),
        (
            "200 OK",
            r#"[{"domainId":"domain-1","host":"api.example.test","applicationId":"application-1"}]"#,
        ),
        (
            "200 OK",
            r#"{"domainId":"domain-1","host":"api.example.test","applicationId":"application-1"}"#,
        ),
    ];
    if delete {
        responses.push(("200 OK", r#"{"ok":true}"#));
    }

    responses
}

fn domain_configuration(destroy: Option<bool>) -> String {
    let mut configuration = concat!(
        "version: 1\n",
        "project:\n  name: platform\n",
        "environments:\n",
        "  production:\n",
        "    applications:\n",
        "      api: {}\n",
    )
    .to_owned();
    match destroy {
        None => configuration.push_str(concat!(
            "    domains:\n",
            "      public:\n",
            "        host: api.example.test\n",
            "        application: application.api\n",
        )),
        Some(destroy) => configuration.push_str(&format!(
            "    domains: {{}}\nremoved:\n  - from: domain.public\n    destroy: {destroy}\n"
        )),
    }

    configuration
}
