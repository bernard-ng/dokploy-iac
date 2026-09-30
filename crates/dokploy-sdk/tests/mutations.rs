use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ApplicationId, CreateApplication, CreateDomain, CreateEnvironment, CreatePostgres,
    CreateProject, CreateRedis, Dokploy, EnvironmentId, ProjectId,
};
use zeroize::Zeroizing;

struct TestServer {
    url: String,
    request: Receiver<String>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_with_json(body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, request) = mpsc::channel();
        let thread = thread::spawn(move || {
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

            sender
                .send(String::from_utf8(bytes).expect("request is UTF-8"))
                .expect("test receives the request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response is writable");
        });

        Self {
            url: format!("http://{address}"),
            request,
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

    fn finish(self) -> String {
        let request = self.request.recv().expect("test receives the request");
        self.thread.join().expect("test server exits cleanly");
        request
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
async fn project_create_returns_both_physical_identities() {
    let server = TestServer::respond_with_json(include_str!(
        "../../../fixtures/api/live/v0.30.6/project-create.owner.json"
    ));

    let created = server
        .client()
        .projects()
        .create(CreateProject::new("Platform").with_description("Managed by Dokploy IaC"))
        .await
        .expect("project creation succeeds");

    assert_eq!(created.project_id().as_str(), "project-1");
    assert_eq!(created.default_environment_id().as_str(), "environment-1");
    assert_eq!(created.default_environment_name(), "production");

    let request = server.finish();
    assert!(request.starts_with("POST /api/project.create HTTP/1.1\r\n"));
    let body = request
        .split_once("\r\n\r\n")
        .expect("request contains a body")
        .1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).expect("request body is JSON"),
        serde_json::json!({
            "name": "Platform",
            "description": "Managed by Dokploy IaC"
        })
    );
}

#[tokio::test]
async fn environment_create_returns_its_physical_identity() {
    let server = TestServer::respond_with_json(
        r#"{"environmentId":"environment-2","projectId":"project-1","name":"staging","description":"Managed"}"#,
    );

    let created = server
        .client()
        .environments()
        .create(
            CreateEnvironment::new("staging", ProjectId::new("project-1"))
                .with_description("Managed"),
        )
        .await
        .expect("environment creation succeeds");

    assert_eq!(created.environment_id().as_str(), "environment-2");

    let request = server.finish();
    assert!(request.starts_with("POST /api/environment.create HTTP/1.1\r\n"));
    let body = request
        .split_once("\r\n\r\n")
        .expect("request contains a body")
        .1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).expect("request body is JSON"),
        serde_json::json!({
            "name": "staging",
            "projectId": "project-1",
            "description": "Managed"
        })
    );
}

#[tokio::test]
async fn application_create_returns_its_physical_identity() {
    let server = TestServer::respond_with_json(include_str!(
        "../../../fixtures/api/live/v0.30.6/application-create.owner.json"
    ));

    let created = server
        .client()
        .applications()
        .create(CreateApplication::new(
            "api",
            EnvironmentId::new("environment-1"),
        ))
        .await
        .expect("application creation succeeds");

    assert_eq!(created.application_id().as_str(), "application-1");

    let request = server.finish();
    assert!(request.starts_with("POST /api/application.create HTTP/1.1\r\n"));
    let body = request
        .split_once("\r\n\r\n")
        .expect("request contains a body")
        .1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).expect("request body is JSON"),
        serde_json::json!({
            "name": "api",
            "environmentId": "environment-1"
        })
    );
}

#[tokio::test]
async fn postgres_create_returns_its_physical_identity_without_debugging_the_password() {
    let server = TestServer::respond_with_json(include_str!(
        "../../../fixtures/api/live/v0.30.6/postgres-create.owner.json"
    ));
    let input = CreatePostgres::new(
        "main",
        EnvironmentId::new("environment-1"),
        "app",
        "app",
        Zeroizing::new("password-canary".to_owned()),
    );

    assert!(!format!("{input:?}").contains("password-canary"));
    let created = server
        .client()
        .postgres()
        .create(input)
        .await
        .expect("Postgres creation succeeds");

    assert_eq!(created.postgres_id().as_str(), "postgres-1");

    let request = server.finish();
    assert!(request.starts_with("POST /api/postgres.create HTTP/1.1\r\n"));
    let body: serde_json::Value = serde_json::from_str(
        request
            .split_once("\r\n\r\n")
            .expect("request contains a body")
            .1,
    )
    .expect("request body is JSON");
    assert_eq!(body["name"], "main");
    assert_eq!(body["environmentId"], "environment-1");
    assert_eq!(body["databaseName"], "app");
    assert_eq!(body["databaseUser"], "app");
    assert_eq!(body["databasePassword"], "password-canary");
}

#[tokio::test]
async fn redis_create_returns_its_physical_identity_without_debugging_the_password() {
    let server = TestServer::respond_with_json(include_str!(
        "../../../fixtures/api/live/v0.30.6/redis-create.owner.json"
    ));
    let input = CreateRedis::new(
        "cache",
        EnvironmentId::new("environment-1"),
        Zeroizing::new("password-canary".to_owned()),
    );

    assert!(!format!("{input:?}").contains("password-canary"));
    let created = server
        .client()
        .redis()
        .create(input)
        .await
        .expect("Redis creation succeeds");

    assert_eq!(created.redis_id().as_str(), "redis-1");
    let request = server.finish();
    assert!(request.starts_with("POST /api/redis.create HTTP/1.1\r\n"));
    assert!(request.contains(r#""databasePassword":"password-canary""#));
}

#[tokio::test]
async fn domain_create_returns_its_physical_identity() {
    let server = TestServer::respond_with_json(include_str!(
        "../../../fixtures/api/live/v0.30.6/domain-create.owner.json"
    ));

    let created = server
        .client()
        .domains()
        .create(CreateDomain::new(
            "api.example.test",
            ApplicationId::new("application-1"),
        ))
        .await
        .expect("domain creation succeeds");

    assert_eq!(created.domain_id().as_str(), "domain-1");
    let request = server.finish();
    assert!(request.starts_with("POST /api/domain.create HTTP/1.1\r\n"));
    assert!(request.contains(r#""host":"api.example.test""#));
    assert!(request.contains(r#""applicationId":"application-1""#));
}
