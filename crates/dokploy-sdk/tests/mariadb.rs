use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ChangeMariaDbPassword, CreateMariaDb, Dokploy, EnvironmentId, Error, MariaDbId, ResponseField,
    UpdateMariaDb,
};
use zeroize::Zeroizing;

const MARIADB_RESPONSE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/mariadb-one.created.owner.json");

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
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

fn full_mariadb_page(total: u64) -> &'static str {
    let items = (0..100)
        .map(|index| {
            format!(
                r#"{{"mariadbId":"mariadb-{index}","environmentId":"environment-1","name":"database-{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let page = format!(r#"{{"items":[{items}],"total":{total}}}"#);

    Box::leak(page.into_boxed_str())
}

#[tokio::test]
async fn mariadb_get_uses_a_strong_id_and_safe_tolerant_model() {
    let server = TestServer::respond_with_json(MARIADB_RESPONSE);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let mariadb = client
        .mariadb()
        .get(MariaDbId::new("mariadb-1"))
        .await
        .expect("MariaDB database is readable");

    let request = server.finish();
    assert!(request.starts_with("GET /api/mariadb.one?mariadbId=mariadb-1 HTTP/1.1\r\n"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: test-api-key")
    );
    assert_eq!(mariadb.mariadb_id.as_str(), "mariadb-1");
    assert_eq!(mariadb.environment_id.as_str(), "environment-1");
    assert_eq!(mariadb.app_name, "mariadb-contract-test");
    assert_eq!(mariadb.docker_image, "mariadb:11");
    assert_eq!(
        mariadb.database_name,
        ResponseField::Value("contract".to_owned())
    );
    assert_eq!(
        mariadb.database_user,
        ResponseField::Value("contract".to_owned())
    );

    let debug = format!("{mariadb:?}");
    assert!(!debug.contains("<redacted>"));
    assert!(!debug.contains("database_password"));
    assert!(!debug.contains("database_root_password"));
}

#[tokio::test]
async fn mariadb_by_environment_collects_authoritative_bounded_pages() {
    let server = TestServer::respond_in_sequence(vec![
        full_mariadb_page(101),
        r#"{"items":[{"mariadbId":"mariadb-100","environmentId":"environment-1","name":"database-100"}],"total":101}"#,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let collection = client
        .mariadb()
        .by_environment(EnvironmentId::new("environment-1"))
        .await
        .expect("MariaDB collection is readable");

    assert_eq!(collection.mariadb().len(), 101);
    let requests = server.finish_all();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/mariadb.search?"));
    assert!(requests[0].contains("environmentId=environment-1"));
    assert!(requests[0].contains("limit=100"));
    assert!(requests[0].contains("offset=0"));
    assert!(requests[1].contains("offset=100"));
}

#[tokio::test]
async fn mariadb_by_environment_rejects_ambiguous_pagination() {
    let cases = [
        vec![r#"{"items":[],"total":1}"#],
        vec![r#"{"items":[],"total":10001}"#],
        vec![
            r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"one"},{"mariadbId":"mariadb-2","environmentId":"environment-1","name":"two"}],"total":1}"#,
        ],
        vec![
            full_mariadb_page(101),
            r#"{"items":[{"mariadbId":"mariadb-100","environmentId":"environment-1","name":"database-100"}],"total":100}"#,
        ],
    ];

    for responses in cases {
        let server = TestServer::respond_in_sequence(responses);
        let client = Dokploy::builder()
            .url(&server.url)
            .api_key("test-api-key")
            .build()
            .expect("client configuration is valid");

        let error = client
            .mariadb()
            .by_environment(EnvironmentId::new("environment-1"))
            .await
            .expect_err("ambiguous pagination must fail closed");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "mariadb.search"
            }
        ));
        server.finish_all();
    }
}

#[tokio::test]
async fn mariadb_create_supports_an_optional_root_password_without_leaking_secrets() {
    let server = TestServer::respond_with_json(MARIADB_RESPONSE);
    let input = CreateMariaDb::new(
        "main",
        EnvironmentId::new("environment-1"),
        "app",
        "app",
        Zeroizing::new("user-password-canary".to_owned()),
    )
    .with_root_password(Zeroizing::new("root-password-canary".to_owned()));

    let debug = format!("{input:?}");
    assert!(!debug.contains("user-password-canary"));
    assert!(!debug.contains("root-password-canary"));

    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    let created = client
        .mariadb()
        .create(input)
        .await
        .expect("MariaDB creation succeeds");

    assert_eq!(created.mariadb_id().as_str(), "mariadb-1");
    assert_mutation_request(
        &server.finish(),
        "mariadb.create",
        serde_json::json!({
            "name": "main",
            "environmentId": "environment-1",
            "databaseName": "app",
            "databaseUser": "app",
            "databasePassword": "user-password-canary",
            "databaseRootPassword": "root-password-canary"
        }),
    );

    let server = TestServer::respond_with_json(MARIADB_RESPONSE);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    client
        .mariadb()
        .create(CreateMariaDb::new(
            "main",
            EnvironmentId::new("environment-1"),
            "app",
            "app",
            Zeroizing::new("user-password-canary".to_owned()),
        ))
        .await
        .expect("MariaDB creation without an explicit root password succeeds");
    let request = server.finish();
    let body = request
        .split_once("\r\n\r\n")
        .expect("request contains a body")
        .1;
    let body: serde_json::Value = serde_json::from_str(body).expect("request body is JSON");
    assert_eq!(body.get("databaseRootPassword"), None);
}

#[tokio::test]
async fn mariadb_update_and_password_targets_use_exact_narrow_requests() {
    let update_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let update_client = Dokploy::builder()
        .url(&update_server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    update_client
        .mariadb()
        .update(
            UpdateMariaDb::new(MariaDbId::new("mariadb-1"))
                .with_database("app_next")
                .with_username("app_next"),
        )
        .await
        .expect("MariaDB update succeeds");
    assert_mutation_request(
        &update_server.finish(),
        "mariadb.update",
        serde_json::json!({
            "mariadbId": "mariadb-1",
            "databaseName": "app_next",
            "databaseUser": "app_next"
        }),
    );

    for (input, expected_type, expected_password) in [
        (
            ChangeMariaDbPassword::user(
                MariaDbId::new("mariadb-1"),
                Zeroizing::new("user-password-canary".to_owned()),
            ),
            "user",
            "user-password-canary",
        ),
        (
            ChangeMariaDbPassword::root(
                MariaDbId::new("mariadb-1"),
                Zeroizing::new("root-password-canary".to_owned()),
            ),
            "root",
            "root-password-canary",
        ),
    ] {
        let debug = format!("{input:?}");
        assert!(!debug.contains("password-canary"));
        let server = TestServer::respond_with_json(r#"{"ok":true}"#);
        let client = Dokploy::builder()
            .url(&server.url)
            .api_key("test-api-key")
            .build()
            .expect("client configuration is valid");
        client
            .mariadb()
            .change_password(input)
            .await
            .expect("MariaDB password change succeeds");
        assert_mutation_request(
            &server.finish(),
            "mariadb.changePassword",
            serde_json::json!({
                "mariadbId": "mariadb-1",
                "password": expected_password,
                "type": expected_type
            }),
        );
    }
}

#[tokio::test]
async fn mariadb_invalid_inputs_are_rejected_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let inputs = [
        client
            .mariadb()
            .by_environment(EnvironmentId::new(""))
            .await
            .expect_err("an empty environment ID is invalid"),
        client
            .mariadb()
            .create(CreateMariaDb::new(
                "main",
                EnvironmentId::new("environment-1"),
                "app",
                "app",
                Zeroizing::new("invalid$password".to_owned()),
            ))
            .await
            .expect_err("an invalid create password is rejected"),
        client
            .mariadb()
            .update(UpdateMariaDb::new(MariaDbId::new("mariadb-1")))
            .await
            .expect_err("an empty update is invalid"),
        client
            .mariadb()
            .change_password(ChangeMariaDbPassword::root(
                MariaDbId::new("mariadb-1"),
                Zeroizing::new("invalid$password".to_owned()),
            ))
            .await
            .expect_err("an invalid password change is rejected"),
        client
            .mariadb()
            .delete(MariaDbId::new(""))
            .await
            .expect_err("an empty MariaDB ID is invalid"),
    ];

    assert!(
        inputs
            .iter()
            .all(|error| matches!(error, Error::InvalidRequest { .. }))
    );
}

#[tokio::test]
async fn mariadb_sanitizes_credential_rejections_and_preserves_unknown_transport_outcomes() {
    let success_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let success_client = Dokploy::builder()
        .url(&success_server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    success_client
        .mariadb()
        .delete(MariaDbId::new("mariadb-1"))
        .await
        .expect("MariaDB deletion succeeds");
    assert_mutation_request(
        &success_server.finish(),
        "mariadb.remove",
        serde_json::json!({"mariadbId": "mariadb-1"}),
    );

    let password = "password-canary";
    let rejected_server = TestServer::respond(
        "400 Bad Request",
        format!(
            r#"{{"code":"ECHO_{password}","message":"Rejected password {password}","issues":[{{"message":"{password}"}}]}}"#
        ),
    );
    let rejected_client = Dokploy::builder()
        .url(&rejected_server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    let error = rejected_client
        .mariadb()
        .change_password(ChangeMariaDbPassword::user(
            MariaDbId::new("mariadb-1"),
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
    let unknown_client = Dokploy::builder()
        .url(&unknown_server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    let error = unknown_client
        .mariadb()
        .delete(MariaDbId::new("mariadb-1"))
        .await
        .expect_err("missing mutation response has an unknown outcome");
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "mariadb.remove",
            ..
        }
    ));
    unknown_server.finish();
}

fn assert_mutation_request(request: &str, operation: &str, expected_body: serde_json::Value) {
    assert!(request.starts_with(&format!("POST /api/{operation} HTTP/1.1\r\n")));
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
