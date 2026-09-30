use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::executor::apply_workspace;
use dokploy_cli::planning::plan_workspace;
use dokploy_sdk::Dokploy;
use dokploy_state::{InstanceIdentity, RecoveryStatus, ResourceAddress, StateStore};

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
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
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 1024];

                loop {
                    let count = stream.read(&mut buffer).expect("request is readable");
                    bytes.extend_from_slice(&buffer[..count]);
                    if count == 0 || request_is_complete(&bytes) {
                        break;
                    }
                }

                received.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }

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
