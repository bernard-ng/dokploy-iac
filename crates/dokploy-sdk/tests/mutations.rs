use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ApplicationId, CreateApplication, CreateDomain, CreateEnvironment, CreatePostgres,
    CreateProject, CreateRedis, Dokploy, DomainId, EnvironmentId, Nullable, PostgresId, ProjectId,
    RedisId, UpdateApplication, UpdateDomain, UpdateEnvironment, UpdatePostgres, UpdateProject,
    UpdateRedis,
};
use zeroize::Zeroizing;

struct TestServer {
    url: String,
    request: Receiver<String>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_with_json(body: &'static str) -> Self {
        Self::respond("200 OK", body)
    }

    fn respond(status: &'static str, body: &'static str) -> Self {
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
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
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

    fn close_after_request() -> Self {
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

#[tokio::test]
async fn application_update_and_deploy_use_narrow_typed_requests() {
    let update_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let environment = Zeroizing::new("TOKEN=secret-canary".to_owned());
    let input = UpdateApplication::new(ApplicationId::new("application-1"))
        .with_description(Nullable::Null)
        .with_replicas(Nullable::Value(3))
        .with_github_repository("legalterlaw/platform")
        .with_branch(Nullable::Value("main".to_owned()))
        .with_environment(Nullable::Value(environment));

    assert!(!format!("{input:?}").contains("secret-canary"));
    update_server
        .client()
        .applications()
        .update(input)
        .await
        .expect("application update succeeds");

    let request = update_server.finish();
    assert!(request.starts_with("POST /api/application.update HTTP/1.1\r\n"));
    let body: serde_json::Value = serde_json::from_str(
        request
            .split_once("\r\n\r\n")
            .expect("request contains a body")
            .1,
    )
    .expect("request body is JSON");
    assert_eq!(
        body,
        serde_json::json!({
            "applicationId": "application-1",
            "description": null,
            "replicas": 3,
            "sourceType": "github",
            "repository": "legalterlaw/platform",
            "branch": "main",
            "env": "TOKEN=secret-canary"
        })
    );

    let deploy_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    deploy_server
        .client()
        .applications()
        .deploy(ApplicationId::new("application-1"))
        .await
        .expect("application deployment succeeds");
    let request = deploy_server.finish();
    assert!(request.starts_with("POST /api/application.deploy HTTP/1.1\r\n"));
    assert!(request.contains(r#""applicationId":"application-1""#));
}

#[tokio::test]
async fn resource_updates_send_only_the_owned_fields() {
    let project_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    project_server
        .client()
        .projects()
        .update(UpdateProject::new(
            ProjectId::new("project-1"),
            Nullable::Null,
        ))
        .await
        .expect("project update succeeds");
    let request = project_server.finish();
    assert!(request.starts_with("POST /api/project.update HTTP/1.1\r\n"));
    assert!(request.contains(r#""projectId":"project-1","description":null"#));

    let environment_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    environment_server
        .client()
        .environments()
        .update(UpdateEnvironment::new(
            EnvironmentId::new("environment-1"),
            Nullable::Value("Production".to_owned()),
        ))
        .await
        .expect("environment update succeeds");
    let request = environment_server.finish();
    assert!(request.starts_with("POST /api/environment.update HTTP/1.1\r\n"));
    assert!(request.contains(r#""description":"Production""#));

    let postgres_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let postgres = UpdatePostgres::new(PostgresId::new("postgres-1"))
        .with_database("app_next")
        .with_username("app")
        .with_password(Zeroizing::new("postgres-secret-canary".to_owned()));
    assert!(!format!("{postgres:?}").contains("postgres-secret-canary"));
    postgres_server
        .client()
        .postgres()
        .update(postgres)
        .await
        .expect("Postgres update succeeds");
    let request = postgres_server.finish();
    assert!(request.starts_with("POST /api/postgres.update HTTP/1.1\r\n"));
    assert!(request.contains(r#""databaseName":"app_next""#));
    assert!(request.contains(r#""databasePassword":"postgres-secret-canary""#));

    let redis_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let redis = UpdateRedis::new(
        RedisId::new("redis-1"),
        Zeroizing::new("redis-secret-canary".to_owned()),
    );
    assert!(!format!("{redis:?}").contains("redis-secret-canary"));
    redis_server
        .client()
        .redis()
        .update(redis)
        .await
        .expect("Redis update succeeds");
    let request = redis_server.finish();
    assert!(request.starts_with("POST /api/redis.update HTTP/1.1\r\n"));
    assert!(request.contains(r#""databasePassword":"redis-secret-canary""#));

    let domain_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    domain_server
        .client()
        .domains()
        .update(UpdateDomain::new(
            DomainId::new("domain-1"),
            "next.example.test",
        ))
        .await
        .expect("Domain update succeeds");
    let request = domain_server.finish();
    assert!(request.starts_with("POST /api/domain.update HTTP/1.1\r\n"));
    assert!(request.contains(r#""domainId":"domain-1","host":"next.example.test""#));
}

#[tokio::test]
async fn resource_deletes_use_the_owned_typed_endpoints() {
    let project_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    project_server
        .client()
        .projects()
        .delete(ProjectId::new("project-1"))
        .await
        .expect("project deletion succeeds");
    assert_delete_request(
        project_server.finish(),
        "project.remove",
        serde_json::json!({"projectId": "project-1"}),
    );

    let environment_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    environment_server
        .client()
        .environments()
        .delete(EnvironmentId::new("environment-1"))
        .await
        .expect("environment deletion succeeds");
    assert_delete_request(
        environment_server.finish(),
        "environment.remove",
        serde_json::json!({"environmentId": "environment-1"}),
    );

    let application_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    application_server
        .client()
        .applications()
        .delete(ApplicationId::new("application-1"))
        .await
        .expect("application deletion succeeds");
    assert_delete_request(
        application_server.finish(),
        "application.delete",
        serde_json::json!({"applicationId": "application-1"}),
    );

    let postgres_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    postgres_server
        .client()
        .postgres()
        .delete(PostgresId::new("postgres-1"))
        .await
        .expect("Postgres deletion succeeds");
    assert_delete_request(
        postgres_server.finish(),
        "postgres.remove",
        serde_json::json!({"postgresId": "postgres-1"}),
    );

    let redis_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    redis_server
        .client()
        .redis()
        .delete(RedisId::new("redis-1"))
        .await
        .expect("Redis deletion succeeds");
    assert_delete_request(
        redis_server.finish(),
        "redis.remove",
        serde_json::json!({"redisId": "redis-1"}),
    );

    let domain_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    domain_server
        .client()
        .domains()
        .delete(DomainId::new("domain-1"))
        .await
        .expect("Domain deletion succeeds");
    assert_delete_request(
        domain_server.finish(),
        "domain.delete",
        serde_json::json!({"domainId": "domain-1"}),
    );
}

#[tokio::test]
async fn resource_delete_preserves_remote_rejection_and_unknown_outcome() {
    let rejected_server = TestServer::respond(
        "409 Conflict",
        r#"{"code":"CONFLICT","message":"Resource is still in use"}"#,
    );
    let error = rejected_server
        .client()
        .projects()
        .delete(ProjectId::new("project-1"))
        .await
        .expect_err("a remote rejection must be returned");
    let details = error
        .dokploy()
        .expect("Dokploy error details are preserved");
    assert_eq!(details.status(), 409);
    assert_eq!(details.code(), "CONFLICT");
    assert_delete_request(
        rejected_server.finish(),
        "project.remove",
        serde_json::json!({"projectId": "project-1"}),
    );

    let unknown_server = TestServer::close_after_request();
    let error = unknown_server
        .client()
        .domains()
        .delete(DomainId::new("domain-1"))
        .await
        .expect_err("a missing mutation response has an unknown outcome");
    assert!(matches!(
        error,
        dokploy_sdk::Error::OutcomeUnknown {
            operation: "domain.delete",
            ..
        }
    ));
    assert_delete_request(
        unknown_server.finish(),
        "domain.delete",
        serde_json::json!({"domainId": "domain-1"}),
    );
}

fn assert_delete_request(request: String, operation: &str, expected_body: serde_json::Value) {
    assert!(
        request.starts_with(&format!("POST /api/{operation} HTTP/1.1\r\n")),
        "unexpected request target: {request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: test-api-key")
    );
    let body = request
        .split_once("\r\n\r\n")
        .expect("request contains a body")
        .1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).expect("request body is JSON"),
        expected_body
    );
}
