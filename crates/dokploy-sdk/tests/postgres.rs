use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{Dokploy, PostgresDetails, PostgresId};

const POSTGRES_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/postgres-one.owner.json");

struct TestServer {
    url: String,
    request: Receiver<String>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_with_json(body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (request_sender, request) = mpsc::channel();
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

            request_sender
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

    fn finish(self) -> String {
        let request = self.request.recv().expect("test receives the request");
        self.thread.join().expect("test server exits cleanly");

        request
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
    assert_eq!(postgres.database_name, "app");
    assert_eq!(postgres.database_user, "app");

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
