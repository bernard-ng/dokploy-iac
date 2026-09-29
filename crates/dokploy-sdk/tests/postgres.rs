use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{Dokploy, EnvironmentId, Error, PostgresDetails, PostgresId, ResponseField};

const POSTGRES_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/postgres-one.owner.json");

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
        let (request_sender, requests) = mpsc::channel();
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

            request_sender
                .send(received_requests)
                .expect("test receives the requests");
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

fn request_is_complete(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\r\n\r\n")
}

#[test]
fn postgres_details_expose_only_safe_reconciliation_fields() {
    let postgres: PostgresDetails =
        serde_json::from_str(POSTGRES_FIXTURE).expect("fixture must deserialize");

    assert_eq!(postgres.postgres_id.as_str(), "postgres-1");
    assert_eq!(postgres.environment_id.as_str(), "environment-1");
    assert_eq!(postgres.name, "Main database");
    assert_eq!(postgres.app_name, "postgres-contract-test");
    assert_eq!(postgres.docker_image, "postgres:18");
    assert_eq!(
        postgres.database_name,
        ResponseField::Value("app".to_owned())
    );
    assert_eq!(
        postgres.database_user,
        ResponseField::Value("app".to_owned())
    );

    let debug = format!("{postgres:?}");
    assert!(!debug.contains("<redacted>"));
    assert!(!debug.contains("database_password"));
    assert!(!debug.contains("env:"));
}

#[tokio::test]
async fn postgres_get_uses_a_strong_id_and_decodes_safe_details() {
    let server = TestServer::respond_with_json(POSTGRES_FIXTURE);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let postgres = client
        .postgres()
        .get(PostgresId::new("postgres-1"))
        .await
        .expect("Postgres database is readable");

    let request = server.finish();
    assert!(request.starts_with("GET /api/postgres.one?postgresId=postgres-1 HTTP/1.1\r\n"));
    assert_eq!(postgres.postgres_id.as_str(), "postgres-1");
    assert_eq!(postgres.environment_id.as_str(), "environment-1");
}

#[tokio::test]
async fn postgres_by_environment_collects_every_bounded_page() {
    let first_page_items = (0..100)
        .map(|index| {
            format!(
                r#"{{"postgresId":"postgres-{index}","environmentId":"environment-1","name":"database-{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let first_page = format!(r#"{{"items":[{first_page_items}],"total":101}}"#);
    let first_page: &'static str = Box::leak(first_page.into_boxed_str());
    let server = TestServer::respond_in_sequence(vec![
        first_page,
        r#"{"items":[{"postgresId":"postgres-100","environmentId":"environment-1","name":"database-100"}],"total":101}"#,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let collection = client
        .postgres()
        .by_environment(EnvironmentId::new("environment-1"))
        .await
        .expect("Postgres collection is readable");

    assert_eq!(collection.postgres().len(), 101);
    let requests = server.finish_all();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/postgres.search?"));
    assert!(requests[0].contains("environmentId=environment-1"));
    assert!(requests[0].contains("limit=100"));
    assert!(requests[0].contains("offset=0"));
    assert!(requests[1].contains("offset=100"));
}

#[tokio::test]
async fn postgres_by_environment_rejects_unstable_or_incomplete_pagination() {
    let first_page_items = (0..100)
        .map(|index| {
            format!(
                r#"{{"postgresId":"postgres-{index}","environmentId":"environment-1","name":"database-{index}"}}"#
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
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"one"},{"postgresId":"postgres-2","environmentId":"environment-1","name":"two"}],"total":1}"#,
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
            .postgres()
            .by_environment(EnvironmentId::new("environment-1"))
            .await
            .expect_err("malformed pagination must fail closed");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "postgres.search"
            }
        ));
        server.finish_all();
    }
}

#[tokio::test]
async fn postgres_search_rejects_an_empty_environment_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let error = client
        .postgres()
        .by_environment(EnvironmentId::new(""))
        .await
        .expect_err("an empty environment ID is invalid");

    assert!(matches!(
        error,
        Error::InvalidRequest {
            operation: "postgres.search",
            ..
        }
    ));
}
