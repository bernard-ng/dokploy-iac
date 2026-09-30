use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ChangeMySqlPassword, CreateMySql, Dokploy, EnvironmentId, Error, MySqlDetails, MySqlId,
    ResponseField, UpdateMySql,
};
use zeroize::Zeroizing;

const MYSQL_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/mysql-one.created.owner.json");

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
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 1024];

                loop {
                    let count = stream.read(&mut buffer).expect("request is readable");
                    bytes.extend_from_slice(&buffer[..count]);

                    if count == 0 || request_is_complete(&bytes) {
                        break;
                    }
                }

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

#[test]
fn mysql_details_expose_only_safe_reconciliation_fields() {
    let mysql: MySqlDetails =
        serde_json::from_str(MYSQL_FIXTURE).expect("fixture must deserialize");

    assert_eq!(mysql.mysql_id.as_str(), "mysql-1");
    assert_eq!(mysql.environment_id.as_str(), "environment-1");
    assert_eq!(mysql.name, "MySQL Contract Test");
    assert_eq!(mysql.app_name, "mysql-contract-test");
    assert_eq!(mysql.docker_image, "mysql:8");
    assert_eq!(
        mysql.database_name,
        ResponseField::Value("contract".to_owned())
    );
    assert_eq!(
        mysql.database_user,
        ResponseField::Value("contract".to_owned())
    );

    let debug = format!("{mysql:?}");
    assert!(!debug.contains("<redacted>"));
    assert!(!debug.contains("database_password"));
    assert!(!debug.contains("database_root_password"));
    assert!(!debug.contains("env:"));
}

#[tokio::test]
async fn mysql_get_uses_a_strong_id_authentication_and_safe_details() {
    let server = TestServer::respond_with_json(MYSQL_FIXTURE);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let mysql = client
        .mysql()
        .get(MySqlId::new("mysql-1"))
        .await
        .expect("MySQL database is readable");

    let request = server.finish();
    assert!(request.starts_with("GET /api/mysql.one?mysqlId=mysql-1 HTTP/1.1\r\n"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: test-api-key")
    );
    assert_eq!(mysql.mysql_id.as_str(), "mysql-1");
    assert_eq!(mysql.environment_id.as_str(), "environment-1");
}

#[tokio::test]
async fn mysql_by_environment_collects_every_bounded_page() {
    let first_page_items = (0..100)
        .map(|index| {
            format!(
                r#"{{"mysqlId":"mysql-{index}","environmentId":"environment-1","name":"database-{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let first_page = format!(r#"{{"items":[{first_page_items}],"total":101}}"#);
    let first_page: &'static str = Box::leak(first_page.into_boxed_str());
    let server = TestServer::respond_in_sequence(vec![
        first_page,
        r#"{"items":[{"mysqlId":"mysql-100","environmentId":"environment-1","name":"database-100"}],"total":101}"#,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let collection = client
        .mysql()
        .by_environment(EnvironmentId::new("environment-1"))
        .await
        .expect("MySQL collection is readable");

    assert_eq!(collection.mysql().len(), 101);
    let requests = server.finish_all();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/mysql.search?"));
    assert!(requests[0].contains("environmentId=environment-1"));
    assert!(requests[0].contains("limit=100"));
    assert!(requests[0].contains("offset=0"));
    assert!(requests[1].contains("offset=100"));
}

#[tokio::test]
async fn mysql_by_environment_rejects_unstable_or_incomplete_pagination() {
    let first_page_items = (0..100)
        .map(|index| {
            format!(
                r#"{{"mysqlId":"mysql-{index}","environmentId":"environment-1","name":"database-{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let first_page = format!(r#"{{"items":[{first_page_items}],"total":101}}"#);
    let first_page: &'static str = Box::leak(first_page.into_boxed_str());
    let cases = vec![
        vec![first_page, r#"{"items":[],"total":100}"#],
        vec![r#"{"items":[],"total":1}"#],
        vec![
            r#"{"items":[{"mysqlId":"mysql-1","environmentId":"environment-1","name":"one"},{"mysqlId":"mysql-2","environmentId":"environment-1","name":"two"}],"total":1}"#,
        ],
        vec![r#"{"items":[],"total":10001}"#],
    ];

    for responses in cases {
        let server = TestServer::respond_in_sequence(responses);
        let client = Dokploy::builder()
            .url(&server.url)
            .api_key("test-api-key")
            .build()
            .expect("client configuration is valid");

        let error = client
            .mysql()
            .by_environment(EnvironmentId::new("environment-1"))
            .await
            .expect_err("malformed pagination must fail closed");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "mysql.search"
            }
        ));
        server.finish_all();
    }
}

#[tokio::test]
async fn mysql_inputs_are_rejected_before_transport_when_invalid() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let error = client
        .mysql()
        .by_environment(EnvironmentId::new(""))
        .await
        .expect_err("an empty environment ID is invalid");
    assert!(matches!(
        error,
        Error::InvalidRequest {
            operation: "mysql.search",
            ..
        }
    ));

    let error = client
        .mysql()
        .create(CreateMySql::new(
            "main",
            EnvironmentId::new("environment-1"),
            "app",
            "app",
            Zeroizing::new("user-password".to_owned()),
            Zeroizing::new(String::new()),
        ))
        .await
        .expect_err("the required root password cannot be empty");
    assert!(matches!(
        error,
        Error::InvalidRequest {
            operation: "mysql.create",
            ..
        }
    ));

    let error = client
        .mysql()
        .update(UpdateMySql::new(MySqlId::new("mysql-1")))
        .await
        .expect_err("an empty update is invalid");
    assert!(matches!(
        error,
        Error::InvalidRequest {
            operation: "mysql.update",
            ..
        }
    ));

    let error = client
        .mysql()
        .change_password(ChangeMySqlPassword::root(
            MySqlId::new("mysql-1"),
            Zeroizing::new("invalid$password".to_owned()),
        ))
        .await
        .expect_err("a password outside Dokploy's contract is invalid");
    assert!(matches!(
        error,
        Error::InvalidRequest {
            operation: "mysql.changePassword",
            ..
        }
    ));

    let error = client
        .mysql()
        .delete(MySqlId::new(""))
        .await
        .expect_err("an empty MySQL identity is invalid");
    assert!(matches!(
        error,
        Error::InvalidRequest {
            operation: "mysql.remove",
            ..
        }
    ));
}

#[tokio::test]
async fn mysql_create_requires_both_passwords_and_returns_its_identity() {
    let server = TestServer::respond_with_json(MYSQL_FIXTURE);
    let input = CreateMySql::new(
        "main",
        EnvironmentId::new("environment-1"),
        "app",
        "app",
        Zeroizing::new("user-password-canary".to_owned()),
        Zeroizing::new("root-password-canary".to_owned()),
    );

    let debug = format!("{input:?}");
    assert!(!debug.contains("user-password-canary"));
    assert!(!debug.contains("root-password-canary"));

    let created = server.url.as_str();
    let client = Dokploy::builder()
        .url(created)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    let created = client
        .mysql()
        .create(input)
        .await
        .expect("MySQL creation succeeds");

    assert_eq!(created.mysql_id().as_str(), "mysql-1");
    let request = server.finish();
    assert!(request.starts_with("POST /api/mysql.create HTTP/1.1\r\n"));
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
        serde_json::json!({
            "name": "main",
            "environmentId": "environment-1",
            "databaseName": "app",
            "databaseUser": "app",
            "databasePassword": "user-password-canary",
            "databaseRootPassword": "root-password-canary"
        })
    );
}

#[tokio::test]
async fn mysql_update_and_both_password_targets_use_narrow_typed_requests() {
    let update_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let update_client = Dokploy::builder()
        .url(&update_server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    update_client
        .mysql()
        .update(
            UpdateMySql::new(MySqlId::new("mysql-1"))
                .with_database("app_next")
                .with_username("app_next"),
        )
        .await
        .expect("MySQL update succeeds");
    let request = update_server.finish();
    assert_mutation_request(
        &request,
        "mysql.update",
        serde_json::json!({
            "mysqlId": "mysql-1",
            "databaseName": "app_next",
            "databaseUser": "app_next"
        }),
    );

    for (input, expected_type, expected_password) in [
        (
            ChangeMySqlPassword::user(
                MySqlId::new("mysql-1"),
                Zeroizing::new("user-password-canary".to_owned()),
            ),
            "user",
            "user-password-canary",
        ),
        (
            ChangeMySqlPassword::root(
                MySqlId::new("mysql-1"),
                Zeroizing::new("root-password-canary".to_owned()),
            ),
            "root",
            "root-password-canary",
        ),
    ] {
        let debug = format!("{input:?}");
        assert!(!debug.contains("password-canary"));
        let password_server = TestServer::respond_with_json(r#"{"ok":true}"#);
        let password_client = Dokploy::builder()
            .url(&password_server.url)
            .api_key("test-api-key")
            .build()
            .expect("client configuration is valid");
        password_client
            .mysql()
            .change_password(input)
            .await
            .expect("MySQL password change succeeds");
        let request = password_server.finish();
        let body = request
            .split_once("\r\n\r\n")
            .expect("request contains a body")
            .1;
        let body: serde_json::Value = serde_json::from_str(body).expect("request body is JSON");

        assert!(request.starts_with("POST /api/mysql.changePassword HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("x-api-key: test-api-key")
        );
        assert_eq!(
            body,
            serde_json::json!({
                "mysqlId": "mysql-1",
                "password": expected_password,
                "type": expected_type
            })
        );
    }
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

#[tokio::test]
async fn mysql_sanitizes_credential_rejections_and_preserves_unknown_transport_outcomes() {
    let success_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let success_client = Dokploy::builder()
        .url(&success_server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    success_client
        .mysql()
        .delete(MySqlId::new("mysql-1"))
        .await
        .expect("MySQL deletion succeeds");
    assert_mutation_request(
        &success_server.finish(),
        "mysql.remove",
        serde_json::json!({"mysqlId": "mysql-1"}),
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
        .mysql()
        .change_password(ChangeMySqlPassword::user(
            MySqlId::new("mysql-1"),
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
        .mysql()
        .delete(MySqlId::new("mysql-1"))
        .await
        .expect_err("missing mutation response has an unknown outcome");
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "mysql.remove",
            ..
        }
    ));
    unknown_server.finish();
}
