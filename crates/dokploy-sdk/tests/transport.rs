//! The `Transport` seam: the real client behind it, and an implementor that is not HTTP.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{Dokploy, Error, OperationRequest, Transport};
use serde_json::{Value, json};
use url::Url;

/// Serves one canned response per connection; `drop_last` closes the final connection
/// after reading the request instead of answering it.
fn serve(
    responses: Vec<(&'static str, &'static str)>,
    drop_last: bool,
) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("server binds");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        let total = responses.len();
        for (index, (status, body)) in responses.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().expect("accepts");
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).expect("reads");
                bytes.extend_from_slice(&buffer[..count]);
                if count == 0 || complete(&bytes) {
                    break;
                }
            }
            requests.push(String::from_utf8_lossy(&bytes).into_owned());
            if drop_last && index + 1 == total {
                continue;
            }
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("writes");
        }
        requests
    });
    (url, handle)
}

fn complete(bytes: &[u8]) -> bool {
    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    bytes.len() >= end + 4 + length
}

fn client(url: &str) -> Dokploy {
    Dokploy::builder()
        .url(url)
        .api_key("test-key")
        .build()
        .expect("client builds")
}

#[tokio::test]
async fn a_read_goes_out_as_the_get_the_contract_declares() {
    let (url, server) = serve(vec![("200 OK", r#"[{"registryId":"r-1"}]"#)], false);

    let response = client(&url)
        .call(OperationRequest::new("registry.all"))
        .await
        .expect("the read succeeds");

    assert_eq!(response, json!([{"registryId": "r-1"}]));
    let requests = server.join().unwrap();
    assert!(
        requests[0].starts_with("GET /api/registry.all HTTP/1.1\r\n"),
        "{}",
        requests[0]
    );
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("x-api-key: test-key")
    );
}

#[tokio::test]
async fn query_parameters_travel_on_the_url() {
    let (url, server) = serve(vec![("200 OK", r#"{"registryId":"r-1"}"#)], false);

    client(&url)
        .call(OperationRequest::new("registry.one").query("registryId", "r-1"))
        .await
        .expect("succeeds");

    let requests = server.join().unwrap();
    assert!(
        requests[0].starts_with("GET /api/registry.one?registryId=r-1 HTTP/1.1\r\n"),
        "{}",
        requests[0]
    );
}

#[tokio::test]
async fn a_mutation_is_a_post_with_a_json_body() {
    let (url, server) = serve(vec![("200 OK", r#"{"registryId":"r-2"}"#)], false);

    let response = client(&url)
        .call(OperationRequest::new("registry.create").body(json!({"registryName": "main"})))
        .await
        .expect("succeeds");

    assert_eq!(response["registryId"], "r-2");
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("POST /api/registry.create HTTP/1.1\r\n"));
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("content-type: application/json")
    );
    assert!(
        requests[0].ends_with(r#"{"registryName":"main"}"#),
        "{}",
        requests[0]
    );
}

#[tokio::test]
async fn a_mutation_whose_completion_cannot_be_proven_is_never_retried() {
    // The server reads the request and closes the connection. If the client retried, the
    // server (which accepts only one connection) would block the test.
    let (url, server) = serve(vec![("200 OK", "{}")], true);

    let error = client(&url)
        .call(OperationRequest::new("registry.create").body(json!({"registryName": "main"})))
        .await
        .expect_err("no response arrived");

    assert!(matches!(error, Error::OutcomeUnknown { .. }), "{error:?}");
    assert_eq!(server.join().unwrap().len(), 1, "sent exactly once");
}

#[tokio::test]
async fn an_operation_the_contract_does_not_declare_is_refused_before_any_request() {
    // Nothing listens here: reaching the network would be a different error.
    let error = client("http://127.0.0.1:9")
        .call(OperationRequest::new("registry.nope"))
        .await
        .expect_err("refused");

    assert!(matches!(error, Error::InvalidRequest { .. }), "{error:?}");
}

#[tokio::test]
async fn dokploy_errors_keep_their_structure() {
    let body = r#"{"message":"Registry not found","code":"NOT_FOUND"}"#;
    let (url, server) = serve(vec![("404 Not Found", body)], false);

    let error = client(&url)
        .call(OperationRequest::new("registry.one").query("registryId", "gone"))
        .await
        .expect_err("a 404");

    let detail = error.dokploy().expect("a structured Dokploy error");
    assert_eq!((detail.status(), detail.code()), (404, "NOT_FOUND"));
    server.join().unwrap();
}

#[test]
fn a_request_never_shows_its_values() {
    let request = OperationRequest::new("registry.create")
        .query("name", "query-canary")
        .body(json!({"password": "body-canary"}));
    let shown = format!("{request:?}");
    assert!(shown.contains("registry.create") && shown.contains("name"));
    assert!(
        !shown.contains("query-canary") && !shown.contains("body-canary"),
        "{shown}"
    );
}

// ---------------------------------------------------------------------------
// The seam itself
// ---------------------------------------------------------------------------

/// An in-memory implementor: no socket, no HTTP. This is what the simulator will be.
struct Recording {
    url: Url,
    requests: Mutex<Vec<OperationRequest>>,
}

impl Transport for Recording {
    fn base_url(&self) -> &Url {
        &self.url
    }

    async fn call(&self, request: OperationRequest) -> Result<Value, Error> {
        self.requests.lock().unwrap().push(request);
        Ok(json!([]))
    }
}

/// Engine-style code is generic over the transport.
async fn list<T: Transport>(transport: &T) -> Result<(String, Value), Error> {
    let value = transport
        .call(OperationRequest::new("registry.all"))
        .await?;
    Ok((transport.base_url().to_string(), value))
}

#[tokio::test]
async fn code_written_against_the_trait_runs_on_any_implementor() {
    let recording = Recording {
        url: Url::parse("https://dokploy.sim.test/").unwrap(),
        requests: Mutex::new(Vec::new()),
    };
    let (instance, value) = list(&recording).await.unwrap();
    assert_eq!(
        (instance.as_str(), value),
        ("https://dokploy.sim.test/", json!([]))
    );
    assert_eq!(
        recording.requests.lock().unwrap()[0].operation(),
        "registry.all"
    );

    // References and shared handles are transports too.
    list(&&recording).await.unwrap();
    list(&Arc::new(recording)).await.unwrap();

    let (url, server) = serve(vec![("200 OK", "[]")], false);
    let shared = Arc::new(client(&url));
    let (instance, _) = list(&shared).await.unwrap();
    assert!(instance.starts_with("http://127.0.0.1:"));
    server.join().unwrap();
}
