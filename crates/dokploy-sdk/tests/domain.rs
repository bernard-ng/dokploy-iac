use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{ApplicationId, Dokploy, DomainDetails, DomainId};

const DOMAIN_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/domain-one.created.owner.json");
const DOMAIN_COLLECTION_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/domain-by-application.created.owner.json");

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
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
                    if count == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
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
                .expect("test receives requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("test server exits cleanly");
        requests
    }
}

#[test]
fn domain_details_expose_only_owned_reconciliation_fields() {
    let domain: DomainDetails =
        serde_json::from_str(DOMAIN_FIXTURE).expect("domain fixture must deserialize");

    assert_eq!(domain.domain_id.as_str(), "domain-1");
    assert_eq!(domain.host, "created.domain.example.test");
    assert_eq!(
        domain.application_id.as_ref().map(ApplicationId::as_str),
        Some("application-1")
    );
    assert!(!format!("{domain:?}").contains("middlewares"));
}

#[tokio::test]
async fn domains_read_by_strong_identity_and_application_scope() {
    let server = TestServer::respond_in_sequence(vec![DOMAIN_FIXTURE, DOMAIN_COLLECTION_FIXTURE]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let domain = client
        .domains()
        .get(DomainId::new("domain-1"))
        .await
        .expect("domain is readable");
    let collection = client
        .domains()
        .by_application(ApplicationId::new("application-1"))
        .await
        .expect("domain collection is readable");

    assert_eq!(domain.domain_id.as_str(), "domain-1");
    assert_eq!(collection.domains().len(), 1);
    let requests = server.finish();
    assert!(requests[0].starts_with("GET /api/domain.one?domainId=domain-1 HTTP/1.1\r\n"));
    assert!(
        requests[1].starts_with(
            "GET /api/domain.byApplicationId?applicationId=application-1 HTTP/1.1\r\n"
        )
    );
}
