use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dokploy_cli::recovery::{RecoveryAction, recover_workspace_with_approval};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    ExpectedCheckpoint, ExpectedState, FingerprintKeyId, InstanceIdentity, JournalAction,
    ManagedInputs, OperationJournal, PlanDigest, RecoveryStatus, RemoteId, ResourceAddress,
    ResourceKind, ResourceState, SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath,
    StateFile, StateStore,
};
use semver::Version;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

struct CrashServer {
    url: String,
    create_observed: Receiver<()>,
    release_create: mpsc::Sender<()>,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn project_topology(body: &'static str) -> Self {
        Self::respond_in_sequence(vec![body])
    }

    fn respond_in_sequence(bodies: Vec<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut captured = Vec::new();
            for body in bodies {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                captured.push(read_request(&mut stream));
                write_response(&mut stream, body);
            }
            sender.send(captured).expect("test receives requests");
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

#[tokio::test]
async fn uncertain_port_update_is_recovered_from_authoritative_complete_state() {
    let server = TestServer::respond_in_sequence(vec![
        r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"postgres":[],"redis":[]}]}]"#,
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api"}"#,
        r#"{"applicationId":"application-1","ports":[{"portId":"port-1","applicationId":"application-1","publishedPort":8080,"targetPort":81,"publishMode":"ingress","protocol":"tcp"}]}"#,
        r#"{"portId":"port-1","applicationId":"application-1","publishedPort":8080,"targetPort":81,"publishMode":"ingress","protocol":"tcp"}"#,
    ]);
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    applications:\n",
            "      api:\n",
            "        ports:\n",
            "          http:\n",
            "            published_port: 8080\n",
            "            target_port: 81\n",
            "            publish_mode: ingress\n",
            "            protocol: tcp\n",
        ),
    )
    .expect("configuration fixture is writable");
    let instance = InstanceIdentity::parse(&server.url).expect("server URL is valid");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store is valid");
    let mut state = seed_parent_state(&store, instance);
    let before_parents = state.clone();
    state
        .upsert_resource(
            address("application.api"),
            ResourceState::new(
                ResourceKind::Application,
                RemoteId::new("application-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                Some(address("environment.production")),
                Vec::new(),
            ),
        )
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&before_parents), &state)
        .unwrap();
    let before_inputs = serde_json::json!({
        "published_port": 8080,
        "target_port": 80,
        "publish_mode": "ingress",
        "protocol": "tcp"
    });
    let after_inputs = serde_json::json!({
        "published_port": 8080,
        "target_port": 81,
        "publish_mode": "ingress",
        "protocol": "tcp"
    });
    let port_address = address("port.http");
    let before_port = ResourceState::new(
        ResourceKind::Port,
        RemoteId::new("port-1").unwrap(),
        false,
        ManagedInputs::try_from_json(before_inputs).unwrap(),
        Some(address("application.api")),
        Vec::new(),
    );
    let before_application = state.clone();
    state
        .upsert_resource(port_address.clone(), before_port.clone())
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&before_application), &state)
        .unwrap();
    let after_port = ResourceState::new(
        ResourceKind::Port,
        RemoteId::new("port-1").unwrap(),
        false,
        ManagedInputs::try_from_json(after_inputs.clone()).unwrap(),
        Some(address("application.api")),
        Vec::new(),
    );
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("d".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            port_address.clone(),
            JournalAction::Update,
            ExpectedCheckpoint::update(before_port, after_port).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&port_address));
        assert_eq!(preview.action(), RecoveryAction::CheckpointConfirmedSuccess);
        Ok(true)
    })
    .await
    .expect("complete authoritative Port evidence confirms the update");

    assert_eq!(result.recovered_steps(), 1);
    assert_eq!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&port_address)
            .unwrap()
            .last_applied()
            .as_json(),
        &after_inputs
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    assert_eq!(server.finish().len(), 7);
}

#[tokio::test]
async fn uncertain_port_create_adopts_one_exact_collision_key_without_retrying() {
    let server = TestServer::respond_in_sequence(vec![
        r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"postgres":[],"redis":[]}]}]"#,
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api"}"#,
        r#"{"applicationId":"application-1","ports":[{"portId":"port-1","applicationId":"application-1","publishedPort":8080,"targetPort":80,"publishMode":"ingress","protocol":"tcp"}]}"#,
    ]);
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        concat!(
            "version: 1\n",
            "project:\n  name: platform\n",
            "environments:\n",
            "  production:\n",
            "    applications:\n",
            "      api:\n",
            "        ports:\n",
            "          http:\n",
            "            published_port: 8080\n",
            "            target_port: 80\n",
            "            publish_mode: ingress\n",
            "            protocol: tcp\n",
        ),
    )
    .expect("configuration fixture is writable");
    let instance = InstanceIdentity::parse(&server.url).expect("server URL is valid");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store is valid");
    let mut state = seed_parent_state(&store, instance);
    let before_application = state.clone();
    state
        .upsert_resource(
            address("application.api"),
            ResourceState::new(
                ResourceKind::Application,
                RemoteId::new("application-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                Some(address("environment.production")),
                Vec::new(),
            ),
        )
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&before_application), &state)
        .unwrap();
    let inputs = serde_json::json!({
        "published_port": 8080,
        "target_port": 80,
        "publish_mode": "ingress",
        "protocol": "tcp"
    });
    let port_address = address("port.http");
    let target = ResourceState::new(
        ResourceKind::Port,
        RemoteId::new("recovery-pending").unwrap(),
        false,
        ManagedInputs::try_from_json(inputs.clone()).unwrap(),
        Some(address("application.api")),
        Vec::new(),
    );
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("e".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            port_address.clone(),
            JournalAction::Create,
            ExpectedCheckpoint::create(target).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&port_address));
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .expect("one exact authoritative Port adopts the uncertain create");

    assert_eq!(result.recovered_steps(), 1);
    let recovered = store.inspect().unwrap().unwrap();
    let port = recovered.resource(&port_address).unwrap();
    assert_eq!(port.remote_id().as_str(), "port-1");
    assert_eq!(port.last_applied().as_json(), &inputs);
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("port.create"))
    );
}

#[tokio::test]
async fn uncertain_mysql_metadata_update_is_recovered_from_readable_fresh_state() {
    exercise_uncertain_database_update_recovery(
        ResourceKind::MySql,
        "mysql.main",
        "mysql-1",
        "mysql",
        r#"{"items":[{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        r#"{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main","appName":"mysql-main","dockerImage":"mysql:8","databaseName":"next","databaseUser":"next"}"#,
        1,
        true,
    )
    .await;
}

#[tokio::test]
async fn uncertain_mysql_secret_rotation_requires_manual_intervention() {
    exercise_uncertain_database_update_recovery(
        ResourceKind::MySql,
        "mysql.main",
        "mysql-1",
        "mysql",
        r#"{"items":[{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        r#"{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main","appName":"mysql-main","dockerImage":"mysql:8","databaseName":"next","databaseUser":"next"}"#,
        9,
        false,
    )
    .await;
}

#[tokio::test]
async fn uncertain_mariadb_metadata_update_is_recovered_from_readable_fresh_state() {
    exercise_uncertain_database_update_recovery(
        ResourceKind::MariaDb,
        "mariadb.main",
        "mariadb-1",
        "mariadb",
        r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        r#"{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main","appName":"mariadb-main","dockerImage":"mariadb:11","databaseName":"next","databaseUser":"next"}"#,
        1,
        true,
    )
    .await;
}

#[tokio::test]
async fn uncertain_mongo_metadata_update_is_recovered_from_readable_fresh_state() {
    exercise_uncertain_database_update_recovery(
        ResourceKind::Mongo,
        "mongo.main",
        "mongo-1",
        "mongo",
        r#"{"items":[{"mongoId":"mongo-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        r#"{"mongoId":"mongo-1","environmentId":"environment-1","name":"main","appName":"mongo-main","dockerImage":"mongo:8","databaseUser":"next","replicaSets":true}"#,
        1,
        true,
    )
    .await;
}

#[tokio::test]
async fn uncertain_libsql_metadata_update_is_recovered_when_password_is_unchanged() {
    exercise_uncertain_database_update_recovery(
        ResourceKind::LibSql,
        "libsql.main",
        "libsql-1",
        "libsql",
        r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"libsql":[{"libsqlId":"libsql-1","name":"main","appName":"main","description":"next"}]}]}"#,
        r#"{"libsqlId":"libsql-1","environmentId":"environment-1","name":"main","appName":"main","dockerImage":"ghcr.io/tursodatabase/libsql-server:v0.24.32","description":"next","databaseUser":"next","sqldNode":"primary","sqldPrimaryUrl":null}"#,
        1,
        true,
    )
    .await;
}

#[tokio::test]
async fn uncertain_libsql_password_rotation_requires_manual_intervention() {
    exercise_uncertain_database_update_recovery(
        ResourceKind::LibSql,
        "libsql.main",
        "libsql-1",
        "libsql",
        r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"libsql":[{"libsqlId":"libsql-1","name":"main","appName":"main","description":"next"}]}]}"#,
        r#"{"libsqlId":"libsql-1","environmentId":"environment-1","name":"main","appName":"main","dockerImage":"ghcr.io/tursodatabase/libsql-server:v0.24.32","description":"next","databaseUser":"next","sqldNode":"primary","sqldPrimaryUrl":null}"#,
        9,
        false,
    )
    .await;
}

#[tokio::test]
async fn uncertain_compose_metadata_update_is_recovered_when_document_is_unchanged() {
    exercise_uncertain_database_update_recovery(
        ResourceKind::Compose,
        "compose.main",
        "compose-1",
        "compose",
        r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"main","appName":"main-app","description":"next","sourceType":"raw"}],"total":1}"#,
        r#"{"composeId":"compose-1","environmentId":"environment-1","name":"main","appName":"main-app","description":"next","sourceType":"raw","composeType":"docker-compose","autoDeploy":false,"composePath":"./docker-compose.yml","composeStatus":"idle"}"#,
        1,
        true,
    )
    .await;
}

#[tokio::test]
async fn uncertain_compose_document_update_requires_manual_intervention() {
    exercise_uncertain_database_update_recovery(
        ResourceKind::Compose,
        "compose.main",
        "compose-1",
        "compose",
        r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"main","appName":"main-app","description":"next","sourceType":"raw"}],"total":1}"#,
        r#"{"composeId":"compose-1","environmentId":"environment-1","name":"main","appName":"main-app","description":"next","sourceType":"raw","composeType":"docker-compose","autoDeploy":false,"composePath":"./docker-compose.yml","composeStatus":"idle"}"#,
        9,
        false,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn exercise_uncertain_database_update_recovery(
    kind: ResourceKind,
    address_value: &str,
    remote_id: &str,
    collection_name: &str,
    collection: &'static str,
    details: &'static str,
    proposed_password_fingerprint: u8,
    expect_recovery: bool,
) {
    let server = TestServer::respond_in_sequence(vec![
        r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        collection,
        details,
    ]);
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    let (configured_fields, before_inputs, after_inputs, before_sensitive, after_sensitive) =
        if kind == ResourceKind::Compose {
            (
                concat!(
                    "        description: next\n",
                    "        document:\n",
                    "          file: compose.yaml\n",
                )
                .to_owned(),
                serde_json::json!({"description":"old"}),
                serde_json::json!({"description":"next"}),
                compose_sensitive_inputs(1),
                compose_sensitive_inputs(proposed_password_fingerprint),
            )
        } else if kind == ResourceKind::Mongo {
            (
                "        username: next\n        password: null\n        replica_sets: true\n"
                    .to_owned(),
                serde_json::json!({"username":"old","replica_sets":false}),
                serde_json::json!({"username":"next","replica_sets":true}),
                mongo_sensitive_inputs(1),
                mongo_sensitive_inputs(proposed_password_fingerprint),
            )
        } else if kind == ResourceKind::LibSql {
            (
                concat!(
                    "        description: next\n",
                    "        username: next\n",
                    "        password: null\n",
                    "        node: { type: primary }\n",
                )
                .to_owned(),
                serde_json::json!({"description":"old","username":"old","node":{"type":"primary"}}),
                serde_json::json!({"description":"next","username":"next","node":{"type":"primary"}}),
                mongo_sensitive_inputs(1),
                mongo_sensitive_inputs(proposed_password_fingerprint),
            )
        } else {
            (
                "        database: next\n        username: next\n        password: null\n        root_password: null\n"
                    .to_owned(),
                serde_json::json!({"database":"old","username":"old"}),
                serde_json::json!({"database":"next","username":"next"}),
                database_sensitive_inputs(1, 2),
                database_sensitive_inputs(proposed_password_fingerprint, 2),
            )
        };
    fs::write(
        &config_file,
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    {}:\n",
                "      main:\n",
                "{}",
            ),
            collection_name, configured_fields,
        ),
    )
    .expect("configuration fixture is writable");
    if kind == ResourceKind::Compose {
        fs::write(
            workspace.path().join("compose.yaml"),
            "services:\n  web:\n    image: recovery-canary\n",
        )
        .expect("Compose document fixture is writable");
    }
    let instance = InstanceIdentity::parse(&server.url).expect("server URL is valid");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store is valid");
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::absent(), &state)
        .unwrap();
    let before_project = state.clone();
    state
        .upsert_resource(
            address("project.platform"),
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new("project-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                None,
                Vec::new(),
            ),
        )
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&before_project), &state)
        .unwrap();
    let before_environment = state.clone();
    state
        .upsert_resource(
            address("environment.production"),
            ResourceState::new(
                ResourceKind::Environment,
                RemoteId::new("environment-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                Some(address("project.platform")),
                Vec::new(),
            ),
        )
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&before_environment), &state)
        .unwrap();
    let before_database = state.clone();
    state
        .upsert_resource(
            address(address_value),
            ResourceState::try_new(
                kind,
                RemoteId::new(remote_id).unwrap(),
                false,
                ManagedInputs::try_from_json(before_inputs).unwrap(),
                before_sensitive,
                Some(address("environment.production")),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&before_database), &state)
        .unwrap();
    let before = state.resource(&address(address_value)).unwrap().clone();
    let after = ResourceState::try_new(
        kind,
        RemoteId::new(remote_id).unwrap(),
        false,
        ManagedInputs::try_from_json(after_inputs.clone()).unwrap(),
        after_sensitive,
        Some(address("environment.production")),
        Vec::new(),
    )
    .unwrap();
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("c".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            address(address_value),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, after).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&address(address_value)));
        assert_eq!(preview.action(), RecoveryAction::CheckpointConfirmedSuccess);
        Ok(true)
    })
    .await;

    if expect_recovery {
        let result = result.expect("readable database update recovery succeeds");
        assert_eq!(result.recovered_steps(), 1);
        let recovered = store.inspect().unwrap().unwrap();
        assert_eq!(
            recovered
                .resource(&address(address_value))
                .unwrap()
                .last_applied()
                .as_json(),
            &after_inputs
        );
        assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    } else {
        assert!(matches!(
            result,
            Err(dokploy_cli::recovery::RecoverWorkspaceError::ManualIntervention)
        ));
        assert!(matches!(
            store.recovery_status().unwrap(),
            RecoveryStatus::RecoveryRequired(_)
        ));
    }
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn uncertain_mysql_create_adopts_one_matching_resource_without_retrying_secrets() {
    exercise_uncertain_database_create_recovery(
        ResourceKind::MySql,
        "mysql.main",
        "mysql-1",
        "mysql",
        r#"{"items":[{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        r#"{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main","appName":"mysql-main","dockerImage":"mysql:8","databaseName":"app","databaseUser":"app","databasePassword":"never-crosses-sdk","databaseRootPassword":"never-crosses-sdk"}"#,
    )
    .await;
}

#[tokio::test]
async fn uncertain_mariadb_create_adopts_one_matching_resource_without_retrying_secrets() {
    exercise_uncertain_database_create_recovery(
        ResourceKind::MariaDb,
        "mariadb.main",
        "mariadb-1",
        "mariadb",
        r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        r#"{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main","appName":"mariadb-main","dockerImage":"mariadb:11","databaseName":"app","databaseUser":"app","databasePassword":"never-crosses-sdk","databaseRootPassword":"never-crosses-sdk"}"#,
    )
    .await;
}

#[tokio::test]
async fn uncertain_mongo_create_adopts_one_matching_resource_without_retrying_secrets() {
    exercise_uncertain_database_create_recovery(
        ResourceKind::Mongo,
        "mongo.main",
        "mongo-1",
        "mongo",
        r#"{"items":[{"mongoId":"mongo-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        r#"{"mongoId":"mongo-1","environmentId":"environment-1","name":"main","appName":"mongo-main","dockerImage":"mongo:8","databaseUser":"app","replicaSets":false,"databasePassword":"never-crosses-sdk"}"#,
    )
    .await;
}

#[tokio::test]
async fn uncertain_libsql_create_adopts_topology_identity_without_retrying_secrets() {
    exercise_uncertain_database_create_recovery(
        ResourceKind::LibSql,
        "libsql.main",
        "libsql-1",
        "libsql",
        r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"libsql":[{"libsqlId":"libsql-1","name":"main","appName":"main","description":"edge"}]}]}"#,
        r#"{"libsqlId":"libsql-1","environmentId":"environment-1","name":"main","appName":"main","dockerImage":"ghcr.io/tursodatabase/libsql-server:v0.24.32","description":"edge","databaseUser":"app","sqldNode":"primary","sqldPrimaryUrl":null,"databasePassword":"never-crosses-sdk"}"#,
    )
    .await;
}

#[tokio::test]
async fn uncertain_compose_create_adopts_one_matching_resource_without_retrying_the_document() {
    exercise_uncertain_database_create_recovery(
        ResourceKind::Compose,
        "compose.main",
        "compose-1",
        "compose",
        r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"main","appName":"main-app","description":"edge","sourceType":"raw"}],"total":1}"#,
        r#"{"composeId":"compose-1","environmentId":"environment-1","name":"main","appName":"main-app","description":"edge","sourceType":"raw","composeType":"docker-compose","autoDeploy":false,"composePath":"./docker-compose.yml","composeStatus":"idle"}"#,
    )
    .await;
}

#[tokio::test]
async fn interrupted_libsql_create_with_authoritative_absence_confirms_no_change() {
    let server = TestServer::respond_in_sequence(vec![
        r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"libsql":[]}]}"#,
    ]);
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        concat!(
            "version: 1\n",
            "project: { name: platform }\n",
            "environments:\n",
            "  production:\n",
            "    libsql:\n",
            "      main:\n",
            "        description: edge\n",
            "        username: app\n",
            "        password: null\n",
            "        node: { type: primary }\n",
        ),
    )
    .unwrap();
    let instance = InstanceIdentity::parse(&server.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    seed_parent_state(&store, instance);
    let target = ResourceState::try_new(
        ResourceKind::LibSql,
        RemoteId::new("recovery-pending").unwrap(),
        false,
        ManagedInputs::try_from_json(serde_json::json!({
            "description":"edge",
            "username":"app",
            "node":{"type":"primary"}
        }))
        .unwrap(),
        mongo_sensitive_inputs(1),
        Some(address("environment.production")),
        Vec::new(),
    )
    .unwrap();
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("e".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            address("libsql.main"),
            JournalAction::Create,
            ExpectedCheckpoint::create(target).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&address("libsql.main")));
        assert_eq!(preview.action(), RecoveryAction::ConfirmNoChange);
        Ok(true)
    })
    .await
    .expect("authoritative absence closes the interrupted create without retrying");

    assert_eq!(result.recovered_steps(), 1);
    assert!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&address("libsql.main"))
            .is_none()
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[tokio::test]
async fn replacement_crash_after_delete_checkpoint_resolves_without_remote_retry() {
    let server = TestServer::respond_in_sequence(Vec::new());
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(&config_file, "version: 1\nproject: { name: platform }\n").unwrap();
    let instance = InstanceIdentity::parse(&server.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    let mut state = seed_parent_state(&store, instance);
    let before_state = state.clone();
    let database = ResourceState::try_new(
        ResourceKind::LibSql,
        RemoteId::new("libsql-1").unwrap(),
        false,
        ManagedInputs::try_from_json(serde_json::json!({
            "username":"app",
            "node":{"type":"primary"}
        }))
        .unwrap(),
        mongo_sensitive_inputs(1),
        Some(address("environment.production")),
        Vec::new(),
    )
    .unwrap();
    state
        .upsert_resource(address("libsql.main"), database.clone())
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&before_state), &state)
        .unwrap();
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("f".repeat(64)).unwrap()).unwrap();
    let token = journal
        .start_recoverable_step(
            address("libsql.main"),
            JournalAction::Delete,
            ExpectedCheckpoint::remove(database),
        )
        .unwrap();
    state.remove_resource(&address("libsql.main")).unwrap();
    journal.succeed(token, None, &state).unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.address(), None);
        assert_eq!(preview.action(), RecoveryAction::ResolveOperation);
        Ok(true)
    })
    .await
    .expect("a crash between replacement steps resolves from the durable delete checkpoint");

    assert_eq!(result.recovered_steps(), 0);
    assert!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&address("libsql.main"))
            .is_none()
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    assert!(server.finish().is_empty());
}

async fn exercise_uncertain_database_create_recovery(
    kind: ResourceKind,
    address_value: &str,
    remote_id: &str,
    collection_name: &str,
    collection: &'static str,
    details: &'static str,
) {
    let server = TestServer::respond_in_sequence(vec![
        r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true}]}]"#,
        r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#,
        r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        collection,
        details,
    ]);
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    let (configured_fields, managed_inputs, sensitive) = if kind == ResourceKind::Compose {
        (
            concat!(
                "        description: edge\n",
                "        document:\n",
                "          file: compose.yaml\n",
            )
            .to_owned(),
            serde_json::json!({"description":"edge"}),
            compose_sensitive_inputs(1),
        )
    } else if kind == ResourceKind::Mongo {
        (
            "        username: app\n        password: null\n        replica_sets: false\n"
                .to_owned(),
            serde_json::json!({"username":"app","replica_sets":false}),
            mongo_sensitive_inputs(1),
        )
    } else if kind == ResourceKind::LibSql {
        (
            concat!(
                "        description: edge\n",
                "        username: app\n",
                "        password: null\n",
                "        node: { type: primary }\n",
            )
            .to_owned(),
            serde_json::json!({"description":"edge","username":"app","node":{"type":"primary"}}),
            mongo_sensitive_inputs(1),
        )
    } else {
        (
            "        database: app\n        username: app\n        password: null\n        root_password: null\n"
                .to_owned(),
            serde_json::json!({"database":"app","username":"app"}),
            database_sensitive_inputs(1, 2),
        )
    };
    fs::write(
        &config_file,
        format!(
            concat!(
                "version: 1\n",
                "project:\n  name: platform\n",
                "environments:\n",
                "  production:\n",
                "    {}:\n",
                "      main:\n",
                "{}",
            ),
            collection_name, configured_fields,
        ),
    )
    .expect("configuration fixture is writable");
    if kind == ResourceKind::Compose {
        fs::write(
            workspace.path().join("compose.yaml"),
            "services:\n  web:\n    image: recovery-create-canary\n",
        )
        .expect("Compose document fixture is writable");
    }
    let instance = InstanceIdentity::parse(&server.url).expect("server URL is valid");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store is valid");
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::absent(), &state)
        .unwrap();
    for (address_value, kind, remote_id, containment) in [
        ("project.platform", ResourceKind::Project, "project-1", None),
        (
            "environment.production",
            ResourceKind::Environment,
            "environment-1",
            Some("project.platform"),
        ),
    ] {
        let before = state.clone();
        state
            .upsert_resource(
                address(address_value),
                ResourceState::new(
                    kind,
                    RemoteId::new(remote_id).unwrap(),
                    false,
                    ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                    containment.map(address),
                    Vec::new(),
                ),
            )
            .unwrap();
        store
            .begin_write()
            .unwrap()
            .checkpoint(ExpectedState::from_state(&before), &state)
            .unwrap();
    }
    let target = ResourceState::try_new(
        kind,
        RemoteId::new("recovery-pending").unwrap(),
        false,
        ManagedInputs::try_from_json(managed_inputs).unwrap(),
        sensitive,
        Some(address("environment.production")),
        Vec::new(),
    )
    .unwrap();
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("d".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            address(address_value),
            JournalAction::Create,
            ExpectedCheckpoint::create(target).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&address(address_value)));
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .expect("matching database create recovery succeeds");

    assert_eq!(result.recovered_steps(), 1);
    let recovered = store.inspect().unwrap().unwrap();
    assert_eq!(
        recovered
            .resource(&address(address_value))
            .unwrap()
            .remote_id()
            .as_str(),
        remote_id
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

impl CrashServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("crash server binds");
        let address = listener.local_addr().expect("crash server has an address");
        let (create_sender, create_observed) = mpsc::channel();
        let (release_create, release_receiver) = mpsc::channel();
        let (request_sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut captured = Vec::new();

            let (mut plan_stream, _) = listener.accept().expect("plan request is accepted");
            captured.push(read_request(&mut plan_stream));
            write_response(&mut plan_stream, "[]");

            let (mut create_stream, _) = listener.accept().expect("create request is accepted");
            captured.push(read_request(&mut create_stream));
            create_sender
                .send(())
                .expect("test observes the create request");
            release_receiver
                .recv()
                .expect("test releases the interrupted response");
            drop(create_stream);

            let (mut recovery_stream, _) = listener.accept().expect("recovery read is accepted");
            captured.push(read_request(&mut recovery_stream));
            write_response(
                &mut recovery_stream,
                r#"[{"projectId":"project-1","name":"platform","description":"Stable","environments":[]}]"#,
            );

            request_sender
                .send(captured)
                .expect("test receives captured requests");
        });

        Self {
            url: format!("http://{address}"),
            create_observed,
            release_create,
            requests,
            thread,
        }
    }

    fn wait_for_create(&self) {
        self.create_observed
            .recv_timeout(Duration::from_secs(15))
            .expect("apply reaches the remote create");
    }

    fn release_interrupted_response(&self) {
        self.release_create
            .send(())
            .expect("interrupted response is released");
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("crash server exits cleanly");
        requests
    }
}

#[tokio::test]
async fn uncertain_create_adopts_one_exact_fresh_resource_without_retrying() {
    let server = TestServer::project_topology(
        r#"[{"projectId":"project-1","name":"platform","description":"Stable","environments":[]}]"#,
    );
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        "version: 1\nproject:\n  name: platform\n  description: Stable\n",
    )
    .expect("configuration fixture is writable");
    let instance = InstanceIdentity::parse(&server.url).expect("server URL is valid");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store is valid");
    let state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .expect("writer starts")
        .checkpoint(ExpectedState::absent(), &state)
        .expect("initial state is persisted");
    let target = ResourceState::new(
        ResourceKind::Project,
        RemoteId::new("recovery-pending").expect("placeholder ID is valid"),
        false,
        ManagedInputs::try_from_json(serde_json::json!({"description": "Stable"}))
            .expect("managed inputs are valid"),
        None,
        Vec::new(),
    );
    let mut write = store.begin_write().expect("writer starts");
    let mut journal = OperationJournal::begin(
        &mut write,
        PlanDigest::parse("a".repeat(64)).expect("digest is valid"),
    )
    .expect("journal begins");
    journal
        .start_recoverable_step(
            address("project.platform"),
            JournalAction::Create,
            ExpectedCheckpoint::create(target).expect("checkpoint is valid"),
        )
        .expect("recoverable step starts");
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&address("project.platform")));
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .expect("recovery succeeds");

    assert_eq!(result.recovered_steps(), 1);
    assert!(matches!(
        store
            .recovery_status()
            .expect("recovery status is readable"),
        RecoveryStatus::Clean
    ));
    let recovered = store
        .inspect()
        .expect("state is readable")
        .expect("state exists");
    assert_eq!(
        recovered
            .resource(&address("project.platform"))
            .expect("project is adopted")
            .remote_id()
            .as_str(),
        "project-1"
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /api/project.all HTTP/1.1\r\n"));
    assert!(!requests[0].contains("project.create"));
}

#[tokio::test]
async fn interrupted_move_recovers_atomically_without_remote_requests() {
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        concat!(
            "version: 1\n",
            "project:\n  name: renamed\n",
            "environments: {}\n",
            "moves:\n",
            "  - from: project.platform\n",
            "    to: project.renamed\n",
        ),
    )
    .unwrap();
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .unwrap();
    let instance = InstanceIdentity::parse(client.base_url().as_str()).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    let empty = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::absent(), &empty)
        .unwrap();
    let source = address("project.platform");
    let target = address("project.renamed");
    let child = address("environment.production");
    let mut current = empty.clone();
    current
        .upsert_resource(
            source.clone(),
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new("project-1").unwrap(),
                true,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                None,
                Vec::new(),
            ),
        )
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&empty), &current)
        .unwrap();
    let with_project = current.clone();
    current
        .upsert_resource(
            child.clone(),
            ResourceState::new(
                ResourceKind::Environment,
                RemoteId::new("environment-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                Some(source.clone()),
                vec![source.clone()],
            ),
        )
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&with_project), &current)
        .unwrap();
    let before = current.resource(&source).unwrap().clone();
    let mut expected = current.clone();
    expected.move_resource(&source, target.clone()).unwrap();
    let after = expected.resource(&target).unwrap().clone();
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("b".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            target.clone(),
            JournalAction::Move,
            ExpectedCheckpoint::move_resource(source.clone(), before, after).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&client, &config_file, |preview| {
        assert_eq!(preview.address(), Some(&target));
        assert_eq!(preview.action(), RecoveryAction::CheckpointConfirmedSuccess);
        Ok(true)
    })
    .await
    .expect("state-only move recovery succeeds without discovery");

    assert_eq!(result.recovered_steps(), 1);
    let recovered = store.inspect().unwrap().unwrap();
    assert_eq!(recovered.resource(&source), None);
    assert!(recovered.resource(&target).unwrap().is_protected());
    assert_eq!(
        recovered.resource(&child).unwrap().containment(),
        Some(&target)
    );
    assert_eq!(
        recovered.resource(&child).unwrap().dependencies(),
        std::slice::from_ref(&target)
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
}

#[test]
fn killed_apply_is_recovered_from_fresh_remote_evidence_without_duplicate_create() {
    let server = CrashServer::start();
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    fs::write(
        workspace.path().join("dokploy.yaml"),
        "version: 1\nproject:\n  name: platform\n  description: Stable\n",
    )
    .expect("configuration fixture is writable");

    let mut apply = spawn_cli(workspace.path(), &server.url, &["apply", "--auto-approve"]);
    server.wait_for_create();

    let instance = InstanceIdentity::parse(&server.url).expect("server URL is valid");
    let store = StateStore::new(workspace.path(), instance).expect("state store is valid");
    assert!(matches!(
        store.recovery_status().expect("journal is readable"),
        RecoveryStatus::RecoveryRequired(_)
    ));
    apply.kill().expect("apply process is killed");
    apply.wait().expect("killed apply is reaped");
    server.release_interrupted_response();

    let recovery = Command::new(env!("CARGO_BIN_EXE_dokploy"))
        .current_dir(workspace.path())
        .args([
            "--url",
            &server.url,
            "--api-key",
            "test-api-key",
            "recover",
            "--auto-approve",
        ])
        .output()
        .expect("recovery process runs");
    assert!(
        recovery.status.success(),
        "recovery failed: {}",
        String::from_utf8_lossy(&recovery.stderr)
    );

    assert!(matches!(
        store.recovery_status().expect("journal is readable"),
        RecoveryStatus::Clean
    ));
    let recovered = store
        .inspect()
        .expect("state is readable")
        .expect("state exists");
    assert_eq!(
        recovered
            .resource(&address("project.platform"))
            .expect("project is adopted")
            .remote_id()
            .as_str(),
        "project-1"
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /api/project.all HTTP/1.1\r\n"));
    assert!(requests[1].starts_with("POST /api/project.create HTTP/1.1\r\n"));
    assert!(requests[2].starts_with("GET /api/project.all HTTP/1.1\r\n"));
}

fn spawn_cli(workspace: &std::path::Path, url: &str, arguments: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_dokploy"))
        .current_dir(workspace)
        .args(["--url", url, "--api-key", "test-api-key"])
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("dokploy process starts")
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
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

fn write_response(stream: &mut std::net::TcpStream, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .expect("response is writable");
}

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
}

fn seed_parent_state(store: &StateStore, instance: InstanceIdentity) -> StateFile {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::absent(), &state)
        .unwrap();
    for (address_value, kind, remote_id, containment) in [
        ("project.platform", ResourceKind::Project, "project-1", None),
        (
            "environment.production",
            ResourceKind::Environment,
            "environment-1",
            Some("project.platform"),
        ),
    ] {
        let before = state.clone();
        state
            .upsert_resource(
                address(address_value),
                ResourceState::new(
                    kind,
                    RemoteId::new(remote_id).unwrap(),
                    false,
                    ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                    containment.map(address),
                    Vec::new(),
                ),
            )
            .unwrap();
        store
            .begin_write()
            .unwrap()
            .checkpoint(ExpectedState::from_state(&before), &state)
            .unwrap();
    }

    state
}

fn database_sensitive_inputs(password: u8, root_password: u8) -> SensitiveInputs {
    let key_id = FingerprintKeyId::new(
        uuid::Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").unwrap(),
    )
    .unwrap();
    SensitiveInputs::try_from_entries([
        (
            SensitivePropertyPath::parse("password").unwrap(),
            SensitiveFingerprint::new_v1(key_id.clone(), [password; 32]),
        ),
        (
            SensitivePropertyPath::parse("root_password").unwrap(),
            SensitiveFingerprint::new_v1(key_id, [root_password; 32]),
        ),
    ])
    .unwrap()
}

fn mongo_sensitive_inputs(password: u8) -> SensitiveInputs {
    let key_id = FingerprintKeyId::new(
        uuid::Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").unwrap(),
    )
    .unwrap();
    SensitiveInputs::try_from_entries([(
        SensitivePropertyPath::parse("password").unwrap(),
        SensitiveFingerprint::new_v1(key_id, [password; 32]),
    )])
    .unwrap()
}

fn compose_sensitive_inputs(document: u8) -> SensitiveInputs {
    let key_id = FingerprintKeyId::new(
        uuid::Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").unwrap(),
    )
    .unwrap();
    SensitiveInputs::try_from_entries([(
        SensitivePropertyPath::parse("document").unwrap(),
        SensitiveFingerprint::new_v1(key_id, [document; 32]),
    )])
    .unwrap()
}
