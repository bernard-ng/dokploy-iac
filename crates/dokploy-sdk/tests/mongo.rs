use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ChangeMongoPassword, CreateMongo, Dokploy, EnvironmentId, Error, MongoId, ResponseField,
    UpdateMongo,
};
use zeroize::Zeroizing;

const MONGO_RESPONSE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/mongo-one.created.owner.json");

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

fn full_mongo_page(total: u64) -> &'static str {
    let items = (0..100)
        .map(|index| {
            format!(
                r#"{{"mongoId":"mongo-{index}","environmentId":"environment-1","name":"database-{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let page = format!(r#"{{"items":[{items}],"total":{total}}}"#);

    Box::leak(page.into_boxed_str())
}

#[tokio::test]
async fn mongo_get_uses_a_strong_id_and_safe_tolerant_model() {
    let server = TestServer::respond_with_json(MONGO_RESPONSE);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let mongo = client
        .mongo()
        .get(MongoId::new("mongo-1"))
        .await
        .expect("MongoDB database is readable");

    let request = server.finish();
    assert!(request.starts_with("GET /api/mongo.one?mongoId=mongo-1 HTTP/1.1\r\n"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: test-api-key")
    );
    assert_eq!(mongo.mongo_id.as_str(), "mongo-1");
    assert_eq!(mongo.environment_id.as_str(), "environment-1");
    assert_eq!(mongo.database_user, ResponseField::Value("contract".into()));
    assert_eq!(mongo.replica_sets, ResponseField::Value(false));

    let debug = format!("{mongo:?}");
    assert!(!debug.contains("<redacted>"));
    assert!(!debug.contains("database_password"));
}

#[tokio::test]
async fn mongo_by_environment_collects_authoritative_bounded_pages() {
    let server = TestServer::respond_in_sequence(vec![
        full_mongo_page(101),
        r#"{"items":[{"mongoId":"mongo-100","environmentId":"environment-1","name":"database-100"}],"total":101}"#,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let collection = client
        .mongo()
        .by_environment(EnvironmentId::new("environment-1"))
        .await
        .expect("MongoDB collection is readable");

    assert_eq!(collection.mongo().len(), 101);
    let requests = server.finish_all();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/mongo.search?"));
    assert!(requests[0].contains("environmentId=environment-1"));
    assert!(requests[0].contains("limit=100"));
    assert!(requests[0].contains("offset=0"));
    assert!(requests[1].contains("offset=100"));
}

#[tokio::test]
async fn mongo_by_environment_rejects_ambiguous_pagination() {
    let cases = [
        vec![r#"{"items":[],"total":1}"#],
        vec![r#"{"items":[],"total":10001}"#],
        vec![
            r#"{"items":[{"mongoId":"mongo-1","environmentId":"environment-1","name":"one"},{"mongoId":"mongo-2","environmentId":"environment-1","name":"two"}],"total":1}"#,
        ],
        vec![
            full_mongo_page(101),
            r#"{"items":[{"mongoId":"mongo-100","environmentId":"environment-1","name":"database-100"}],"total":100}"#,
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
            .mongo()
            .by_environment(EnvironmentId::new("environment-1"))
            .await
            .expect_err("ambiguous pagination must fail closed");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "mongo.search"
            }
        ));
        server.finish_all();
    }
}

#[tokio::test]
async fn mongo_create_supports_explicit_replica_sets_without_leaking_the_password() {
    let server = TestServer::respond_with_json(MONGO_RESPONSE);
    let input = CreateMongo::new(
        "main",
        EnvironmentId::new("environment-1"),
        "app",
        Zeroizing::new("database-password-canary".to_owned()),
    )
    .with_replica_sets(true);

    let debug = format!("{input:?}");
    assert!(!debug.contains("database-password-canary"));

    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    let created = client
        .mongo()
        .create(input)
        .await
        .expect("MongoDB creation succeeds");

    assert_eq!(created.mongo_id().as_str(), "mongo-1");
    assert_mutation_request(
        &server.finish(),
        "mongo.create",
        serde_json::json!({
            "name": "main",
            "environmentId": "environment-1",
            "databaseUser": "app",
            "databasePassword": "database-password-canary",
            "replicaSets": true
        }),
    );
}

#[tokio::test]
async fn mongo_update_and_password_change_use_exact_narrow_requests() {
    let update_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let update_client = Dokploy::builder()
        .url(&update_server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    update_client
        .mongo()
        .update(
            UpdateMongo::new(MongoId::new("mongo-1"))
                .with_username("app_next")
                .with_replica_sets(true),
        )
        .await
        .expect("MongoDB update succeeds");
    assert_mutation_request(
        &update_server.finish(),
        "mongo.update",
        serde_json::json!({
            "mongoId": "mongo-1",
            "databaseUser": "app_next",
            "replicaSets": true
        }),
    );

    let password_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let password_client = Dokploy::builder()
        .url(&password_server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    let input = ChangeMongoPassword::new(
        MongoId::new("mongo-1"),
        Zeroizing::new("next-password-canary".to_owned()),
    );
    assert!(!format!("{input:?}").contains("next-password-canary"));
    password_client
        .mongo()
        .change_password(input)
        .await
        .expect("MongoDB password change succeeds");
    assert_mutation_request(
        &password_server.finish(),
        "mongo.changePassword",
        serde_json::json!({
            "mongoId": "mongo-1",
            "password": "next-password-canary"
        }),
    );
}

#[tokio::test]
async fn mongo_invalid_inputs_are_rejected_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let errors = [
        client
            .mongo()
            .by_environment(EnvironmentId::new(""))
            .await
            .expect_err("an empty environment ID is invalid"),
        client
            .mongo()
            .create(CreateMongo::new(
                "main",
                EnvironmentId::new("environment-1"),
                "app",
                Zeroizing::new("invalid$password".to_owned()),
            ))
            .await
            .expect_err("an invalid create password is rejected"),
        client
            .mongo()
            .update(UpdateMongo::new(MongoId::new("mongo-1")))
            .await
            .expect_err("an empty update is invalid"),
        client
            .mongo()
            .change_password(ChangeMongoPassword::new(
                MongoId::new("mongo-1"),
                Zeroizing::new("invalid$password".to_owned()),
            ))
            .await
            .expect_err("an invalid password change is rejected"),
        client
            .mongo()
            .delete(MongoId::new(""))
            .await
            .expect_err("an empty MongoDB ID is invalid"),
    ];

    assert!(
        errors
            .iter()
            .all(|error| matches!(error, Error::InvalidRequest { .. }))
    );
}

#[tokio::test]
async fn mongo_sanitizes_credential_rejections_and_preserves_unknown_transport_outcomes() {
    let success_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let success_client = Dokploy::builder()
        .url(&success_server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    success_client
        .mongo()
        .delete(MongoId::new("mongo-1"))
        .await
        .expect("MongoDB deletion succeeds");
    assert_mutation_request(
        &success_server.finish(),
        "mongo.remove",
        serde_json::json!({"mongoId": "mongo-1"}),
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
        .mongo()
        .change_password(ChangeMongoPassword::new(
            MongoId::new("mongo-1"),
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
        .mongo()
        .delete(MongoId::new("mongo-1"))
        .await
        .expect_err("missing mutation response has an unknown outcome");
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "mongo.remove",
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
