use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ChangeLibSqlPassword, CreateLibSql, Dokploy, EnvironmentId, Error, LibSqlId, LibSqlNode,
    ProjectId, ResponseField, UpdateLibSql,
};
use zeroize::Zeroizing;

const LIBSQL_RESPONSE: &str = r#"{
    "libsqlId":"libsql-1",
    "environmentId":"environment-1",
    "name":"main",
    "appName":"main-generated",
    "dockerImage":"ghcr.io/tursodatabase/libsql-server:v0.24.32",
    "databaseUser":"app",
    "databasePassword":"response-secret-canary",
    "sqldNode":"primary",
    "sqldPrimaryUrl":null,
    "enableNamespaces":false,
    "applicationStatus":"idle",
    "description":null,
    "serverId":null,
    "env":"SECRET=environment-canary",
    "unknownRuntimeField":{"future":true}
}"#;

const EMPTY_TOPOLOGY: &str = r#"{
    "projectId":"project-1",
    "name":"project",
    "environments":[{
        "environmentId":"environment-1",
        "name":"production",
        "isDefault":true,
        "libsql":[]
    }]
}"#;

const POPULATED_TOPOLOGY: &str = r#"{
    "projectId":"project-1",
    "name":"project",
    "environments":[{
        "environmentId":"environment-1",
        "name":"production",
        "isDefault":true,
        "libsql":[{
            "libsqlId":"libsql-1",
            "name":"main",
            "appName":"main-generated",
            "applicationStatus":"idle",
            "description":null,
            "serverId":null
        }]
    }]
}"#;

const AMBIGUOUS_TOPOLOGY: &str = r#"{
    "projectId":"project-1",
    "name":"project",
    "environments":[{
        "environmentId":"environment-1",
        "name":"production",
        "isDefault":true,
        "libsql":[
            {"libsqlId":"libsql-1","name":"main","appName":"main-one"},
            {"libsqlId":"libsql-2","name":"main","appName":"main-two"}
        ]
    }]
}"#;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

enum ResponseAction {
    Json(&'static str, &'static str),
    Drop,
}

impl TestServer {
    fn respond_with_json(body: &'static str) -> Self {
        Self::respond_in_sequence(vec![body])
    }

    fn respond_in_sequence(bodies: Vec<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received_requests = Vec::new();

            for body in bodies {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                let bytes = read_request(&mut stream);
                received_requests.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }

            sender
                .send(received_requests)
                .expect("test receives the requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn respond(status: &'static str, body: impl Into<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let body = body.into();
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test server accepts a request");
            let bytes = read_request(&mut stream);
            sender
                .send(vec![String::from_utf8(bytes).expect("request is UTF-8")])
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
            requests,
            thread,
        }
    }

    fn respond_with_actions(actions: Vec<ResponseAction>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received_requests = Vec::new();

            for action in actions {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                let bytes = read_request(&mut stream);
                received_requests.push(String::from_utf8(bytes).expect("request is UTF-8"));
                if let ResponseAction::Json(status, body) = action {
                    write!(
                        stream,
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .expect("response is writable");
                }
            }

            sender
                .send(received_requests)
                .expect("test receives the requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn close_after_request() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test server accepts a request");
            let bytes = read_request(&mut stream);
            sender
                .send(vec![String::from_utf8(bytes).expect("request is UTF-8")])
                .expect("test receives the request");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn finish(self) -> String {
        let mut requests = self.finish_all();

        assert_eq!(requests.len(), 1, "test server expected one request");
        requests.remove(0)
    }

    fn finish_all(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives the requests");
        self.thread.join().expect("test server exits cleanly");

        requests
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];

    loop {
        let count = stream.read(&mut buffer).expect("request is readable");
        bytes.extend_from_slice(&buffer[..count]);

        if count == 0 || request_is_complete(&bytes) {
            break;
        }
    }

    bytes
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

fn oversized_topology() -> &'static str {
    let items = (0..10_001)
        .map(|index| {
            format!(
                r#"{{"libsqlId":"libsql-{index}","name":"database-{index}","appName":"database-{index}-generated"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let topology = format!(
        r#"{{"projectId":"project-1","name":"project","environments":[{{"environmentId":"environment-1","name":"production","isDefault":true,"libsql":[{items}]}}]}}"#
    );

    Box::leak(topology.into_boxed_str())
}

#[tokio::test]
async fn libsql_get_uses_a_strong_id_and_never_retains_credentials() {
    let server = TestServer::respond_with_json(LIBSQL_RESPONSE);
    let client = client(&server.url);

    let libsql = client
        .libsql()
        .get(LibSqlId::new("libsql-1"))
        .await
        .expect("LibSQL database is readable");

    let request = server.finish();
    assert!(request.starts_with("GET /api/libsql.one?libsqlId=libsql-1 HTTP/1.1\r\n"));
    assert_authenticated(&request);
    assert_eq!(libsql.libsql_id.as_str(), "libsql-1");
    assert_eq!(libsql.environment_id.as_str(), "environment-1");
    assert_eq!(libsql.database_user, ResponseField::Value("app".into()));
    assert_eq!(libsql.enable_namespaces, ResponseField::Value(false));

    let debug = format!("{libsql:?}");
    assert!(!debug.contains("response-secret-canary"));
    assert!(!debug.contains("environment-canary"));
    assert!(!debug.contains("database_password"));
}

#[tokio::test]
async fn libsql_by_environment_uses_exact_project_topology() {
    let server = TestServer::respond_with_json(POPULATED_TOPOLOGY);
    let client = client(&server.url);

    let collection = client
        .libsql()
        .by_environment(
            ProjectId::new("project-1"),
            EnvironmentId::new("environment-1"),
        )
        .await
        .expect("LibSQL topology is readable");

    assert_eq!(collection.libsql().len(), 1);
    assert_eq!(collection.libsql()[0].libsql_id.as_str(), "libsql-1");
    assert_eq!(collection.libsql()[0].name, "main");
    let request = server.finish();
    assert!(request.starts_with("GET /api/project.one?projectId=project-1 HTTP/1.1\r\n"));
    assert_authenticated(&request);
}

#[tokio::test]
async fn libsql_by_environment_fails_closed_when_the_scope_is_not_authoritative() {
    let cases = [
        r#"{"projectId":"project-1","name":"project","environments":[]}"#,
        r#"{"projectId":"project-1","name":"project","environments":[{"environmentId":"environment-1","name":"one","isDefault":true,"libsql":[]},{"environmentId":"environment-1","name":"duplicate","isDefault":false,"libsql":[]}]}"#,
        r#"{"projectId":"project-1","name":"project","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"libsql":[{"libsqlId":"libsql-1"}]}]}"#,
        oversized_topology(),
    ];

    for topology in cases {
        let server = TestServer::respond_with_json(topology);
        let client = client(&server.url);
        let error = client
            .libsql()
            .by_environment(
                ProjectId::new("project-1"),
                EnvironmentId::new("environment-1"),
            )
            .await
            .expect_err("missing or duplicate environments are ambiguous");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "project.one"
            }
        ));
        server.finish();
    }
}

#[tokio::test]
async fn libsql_create_proves_absence_then_recovers_one_identity() {
    let server = TestServer::respond_in_sequence(vec![EMPTY_TOPOLOGY, "true", POPULATED_TOPOLOGY]);
    let input = CreateLibSql::new(
        "main",
        "main",
        ProjectId::new("project-1"),
        EnvironmentId::new("environment-1"),
        "app",
        Zeroizing::new("auth-token-canary".to_owned()),
        LibSqlNode::Primary,
    )
    .with_description("primary database")
    .with_namespaces(true);
    assert!(!format!("{input:?}").contains("auth-token-canary"));
    let client = client(&server.url);

    let created = client
        .libsql()
        .create(input)
        .await
        .expect("LibSQL creation establishes one identity");

    assert_eq!(created.libsql_id().as_str(), "libsql-1");
    let requests = server.finish_all();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /api/project.one?projectId=project-1 HTTP/1.1\r\n"));
    assert_mutation_request(
        &requests[1],
        "libsql.create",
        serde_json::json!({
            "name": "main",
            "appName": "main",
            "dockerImage": "ghcr.io/tursodatabase/libsql-server:v0.24.32",
            "environmentId": "environment-1",
            "description": "primary database",
            "databaseUser": "app",
            "databasePassword": "auth-token-canary",
            "sqldNode": "primary",
            "sqldPrimaryUrl": null,
            "enableNamespaces": true,
            "serverId": null
        }),
    );
    assert!(requests[2].starts_with("GET /api/project.one?projectId=project-1 HTTP/1.1\r\n"));
}

#[tokio::test]
async fn libsql_create_fails_closed_on_preexisting_or_missing_identity() {
    let collision_server = TestServer::respond_with_json(POPULATED_TOPOLOGY);
    let collision_client = client(&collision_server.url);
    let collision = collision_client
        .libsql()
        .create(create_input())
        .await
        .expect_err("a preexisting name collision must stop before mutation");
    assert!(matches!(
        collision,
        Error::UnexpectedResponse {
            operation: "libsql.create"
        }
    ));
    collision_server.finish();

    let missing_server =
        TestServer::respond_in_sequence(vec![EMPTY_TOPOLOGY, "true", EMPTY_TOPOLOGY]);
    let missing_client = client(&missing_server.url);
    let missing = missing_client
        .libsql()
        .create(create_input())
        .await
        .expect_err("a missing post-create identity must fail closed");
    assert!(matches!(
        missing,
        Error::OutcomeUnknown {
            operation: "libsql.create",
            ..
        }
    ));
    missing_server.finish_all();
}

#[tokio::test]
async fn libsql_create_marks_unproven_post_mutation_results_outcome_unknown_without_retrying_post()
{
    for mutation_response in ["false", "{"] {
        let server = TestServer::respond_in_sequence(vec![EMPTY_TOPOLOGY, mutation_response]);
        let error = client(&server.url)
            .libsql()
            .create(create_input())
            .await
            .expect_err("false or malformed create responses cannot prove mutation outcome");

        assert!(matches!(
            error,
            Error::OutcomeUnknown {
                operation: "libsql.create",
                ..
            }
        ));
        assert_one_create_request(&server.finish_all());
    }

    let ambiguous_server =
        TestServer::respond_in_sequence(vec![EMPTY_TOPOLOGY, "true", AMBIGUOUS_TOPOLOGY]);
    let ambiguous = client(&ambiguous_server.url)
        .libsql()
        .create(create_input())
        .await
        .expect_err("ambiguous postflight identity requires recovery");
    assert!(matches!(
        ambiguous,
        Error::OutcomeUnknown {
            operation: "libsql.create",
            ..
        }
    ));
    assert_one_create_request(&ambiguous_server.finish_all());
}

#[tokio::test]
async fn libsql_create_sanitizes_postflight_transport_and_api_failures_as_outcome_unknown() {
    let api_server = TestServer::respond_with_actions(vec![
        ResponseAction::Json("200 OK", EMPTY_TOPOLOGY),
        ResponseAction::Json("200 OK", "true"),
        ResponseAction::Json("500 Internal Server Error", r#"{"message":"first"}"#),
        ResponseAction::Json("500 Internal Server Error", r#"{"message":"second"}"#),
        ResponseAction::Json("500 Internal Server Error", r#"{"message":"third"}"#),
    ]);
    let api_error = client(&api_server.url)
        .libsql()
        .create(create_input())
        .await
        .expect_err("postflight 5xx cannot prove the accepted create");
    assert!(matches!(
        api_error,
        Error::OutcomeUnknown {
            operation: "libsql.create",
            ..
        }
    ));
    assert_one_create_request(&api_server.finish_all());

    let transport_server = TestServer::respond_with_actions(vec![
        ResponseAction::Json("200 OK", EMPTY_TOPOLOGY),
        ResponseAction::Json("200 OK", "true"),
        ResponseAction::Drop,
        ResponseAction::Drop,
        ResponseAction::Drop,
    ]);
    let transport_error = client(&transport_server.url)
        .libsql()
        .create(create_input())
        .await
        .expect_err("dropped postflight reads cannot prove the accepted create");
    assert!(matches!(
        transport_error,
        Error::OutcomeUnknown {
            operation: "libsql.create",
            ..
        }
    ));
    assert_one_create_request(&transport_server.finish_all());
}

#[tokio::test]
async fn libsql_update_and_password_change_use_narrow_redacted_requests() {
    let update_server = TestServer::respond_with_json("true");
    client(&update_server.url)
        .libsql()
        .update(
            UpdateLibSql::new(LibSqlId::new("libsql-1"))
                .with_username("app_next")
                .with_description("updated"),
        )
        .await
        .expect("LibSQL update succeeds");
    assert_mutation_request(
        &update_server.finish(),
        "libsql.update",
        serde_json::json!({
            "libsqlId": "libsql-1",
            "description": "updated",
            "databaseUser": "app_next"
        }),
    );

    let password_server = TestServer::respond_with_json("true");
    let password = ChangeLibSqlPassword::new(
        LibSqlId::new("libsql-1"),
        Zeroizing::new("next-auth-token-canary".to_owned()),
    );
    assert!(!format!("{password:?}").contains("next-auth-token-canary"));
    client(&password_server.url)
        .libsql()
        .change_password(password)
        .await
        .expect("LibSQL password change succeeds");
    assert_mutation_request(
        &password_server.finish(),
        "libsql.update",
        serde_json::json!({
            "libsqlId": "libsql-1",
            "databasePassword": "next-auth-token-canary"
        }),
    );
}

#[tokio::test]
async fn libsql_invalid_inputs_are_rejected_before_transport() {
    let client = client("http://127.0.0.1:9");
    let errors = [
        client
            .libsql()
            .by_environment(ProjectId::new(""), EnvironmentId::new("environment-1"))
            .await
            .expect_err("an empty project ID is invalid"),
        client
            .libsql()
            .create(CreateLibSql::new(
                "main",
                "main",
                ProjectId::new("project-1"),
                EnvironmentId::new("environment-1"),
                "app",
                Zeroizing::new(String::new()),
                LibSqlNode::Primary,
            ))
            .await
            .expect_err("an empty credential is invalid"),
        client
            .libsql()
            .update(UpdateLibSql::new(LibSqlId::new("libsql-1")))
            .await
            .expect_err("an empty update is invalid"),
        client
            .libsql()
            .change_password(ChangeLibSqlPassword::new(
                LibSqlId::new("libsql-1"),
                Zeroizing::new(String::new()),
            ))
            .await
            .expect_err("an empty password is invalid"),
        client
            .libsql()
            .delete(LibSqlId::new(""))
            .await
            .expect_err("an empty LibSQL ID is invalid"),
    ];

    assert!(
        errors
            .iter()
            .all(|error| matches!(error, Error::InvalidRequest { .. }))
    );
}

#[tokio::test]
async fn libsql_sanitizes_credential_rejections_and_preserves_unknown_transport_outcomes() {
    let password = "next-password";
    let rejected_server = TestServer::respond(
        "400 Bad Request",
        format!(
            r#"{{"code":"ECHO_{password}","message":"Invalid password {password}","issues":[{{"message":"{password}"}}]}}"#
        ),
    );
    let error = client(&rejected_server.url)
        .libsql()
        .change_password(ChangeLibSqlPassword::new(
            LibSqlId::new("libsql-1"),
            Zeroizing::new(password.to_owned()),
        ))
        .await
        .expect_err("Dokploy rejection must be sanitized");
    let details = error.dokploy().expect("Dokploy status remains available");
    assert_eq!(details.status(), 400);
    assert_eq!(details.code(), "BAD_REQUEST");
    assert_eq!(details.message(), "Bad Request");
    assert!(details.issues().is_empty());
    assert!(!format!("{error}").contains(password));
    assert!(!format!("{error:?}").contains(password));
    rejected_server.finish();

    let unknown_server = TestServer::close_after_request();
    let error = client(&unknown_server.url)
        .libsql()
        .delete(LibSqlId::new("libsql-1"))
        .await
        .expect_err("missing mutation response has an unknown outcome");
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "libsql.remove",
            ..
        }
    ));
    unknown_server.finish();
}

fn client(url: &str) -> Dokploy {
    Dokploy::builder()
        .url(url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid")
}

fn create_input() -> CreateLibSql {
    CreateLibSql::new(
        "main",
        "main",
        ProjectId::new("project-1"),
        EnvironmentId::new("environment-1"),
        "app",
        Zeroizing::new("auth-token".to_owned()),
        LibSqlNode::Primary,
    )
}

fn assert_authenticated(request: &str) {
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: test-api-key")
    );
}

fn assert_one_create_request(requests: &[String]) {
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /api/libsql.create "))
            .count(),
        1,
        "LibSQL create must never be retried"
    );
}

fn assert_mutation_request(request: &str, operation: &str, expected_body: serde_json::Value) {
    assert!(request.starts_with(&format!("POST /api/{operation} HTTP/1.1\r\n")));
    assert_authenticated(request);
    let body = request
        .split_once("\r\n\r\n")
        .expect("request contains a body")
        .1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).expect("request body is JSON"),
        expected_body
    );
}
