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
            while database_requests.len() < 2 && Instant::now() < deadline {
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
            let overlapped = database_requests.len() == 2;
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
        ),
    )
    .expect("configuration fixture is writable");

    let error = apply_workspace_with_approval(
        &server.client(),
        &config,
        ApplyOptions::new(2).expect("parallelism is valid"),
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
    assert!(state.resource(&postgres).is_none());
    assert_eq!(
        state
            .resource(&redis)
            .expect("successful sibling is checkpointed")
            .remote_id()
            .as_str(),
        "redis-1"
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
        "both database requests must be in flight together"
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
