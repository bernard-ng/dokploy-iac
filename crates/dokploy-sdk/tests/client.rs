use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{Dokploy, Error, ImperativeRequest, MAX_JSON_RESPONSE_BYTES};

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_with_json(body: &'static str) -> Self {
        Self::respond("200 OK", body)
    }

    fn respond(status: &'static str, body: &'static str) -> Self {
        Self::respond_in_sequence(vec![(status, body)])
    }

    fn respond_in_sequence(responses: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (request_sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received_requests = Vec::new();

            for (status, body) in responses {
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
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
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

    fn close_after_request() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (request_sender, requests) = mpsc::channel();
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
                .send(vec![String::from_utf8(bytes).expect("request is UTF-8")])
                .expect("test receives the request");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn respond_raw(response: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (request_sender, requests) = mpsc::channel();
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

            let _ = stream.write_all(&response);
            request_sender
                .send(vec![String::from_utf8(bytes).expect("request is UTF-8")])
                .expect("test receives the request");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn url(&self) -> &str {
        &self.url
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

fn content_length_response(status: &str, declared_length: usize, body: Vec<u8>) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {declared_length}\r\nconnection: close\r\n\r\n"
    )
    .into_bytes();
    response.extend_from_slice(&body);
    response
}

fn chunked_response(status: &str, chunks: Vec<Vec<u8>>) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n"
    )
    .into_bytes();
    for chunk in chunks {
        response.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        response.extend_from_slice(&chunk);
        response.extend_from_slice(b"\r\n");
    }
    response.extend_from_slice(b"0\r\n\r\n");
    response
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
fn client_exposes_its_normalized_api_base_url_without_credentials() {
    let client = Dokploy::builder()
        .url("https://deploy.example.com/dokploy/")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    assert_eq!(
        client.base_url().as_str(),
        "https://deploy.example.com/dokploy/api"
    );
    assert!(!client.base_url().as_str().contains("test-api-key"));
}

#[tokio::test]
async fn requests_authenticate_without_exposing_transport_configuration() {
    let server = TestServer::respond_with_json("[]");
    let client = Dokploy::builder()
        .url(server.url())
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    client
        .imperative()
        .execute(ImperativeRequest::get("project.all"))
        .await
        .expect("project.all is readable");

    let request = server.finish().to_ascii_lowercase();
    assert!(request.contains("\r\nx-api-key: test-api-key\r\n"));
    assert!(request.contains(concat!(
        "\r\nuser-agent: dokploy-iac/",
        env!("CARGO_PKG_VERSION"),
        "\r\n"
    )));
}

#[tokio::test]
async fn imperative_reads_return_the_unmodified_json_contract() {
    let server = TestServer::respond_with_json(r#"{"applicationId":"application-1","extra":true}"#);
    let client = Dokploy::builder()
        .url(server.url())
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let response = client
        .imperative()
        .execute(ImperativeRequest::get("application.one").query("applicationId", "application-1"))
        .await
        .expect("imperative read succeeds");

    assert_eq!(response["applicationId"], "application-1");
    assert_eq!(response["extra"], true);
    assert!(
        server
            .finish()
            .starts_with("GET /api/application.one?applicationId=application-1 HTTP/1.1\r\n")
    );
}

#[tokio::test]
async fn imperative_requests_must_match_generated_endpoint_metadata() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let error = client
        .imperative()
        .execute(ImperativeRequest::get("application.create"))
        .await
        .expect_err("a GET cannot execute a generated POST endpoint");

    assert!(matches!(
        error,
        Error::InvalidRequest {
            operation: "application.create",
            ..
        }
    ));
}

#[tokio::test]
async fn imperative_mutations_send_json_once_and_return_remote_status() {
    let server = TestServer::respond(
        "503 Service Unavailable",
        r#"{"code":"SERVICE_UNAVAILABLE","message":"Try again"}"#,
    );
    let client = Dokploy::builder()
        .url(server.url())
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let error = client
        .imperative()
        .execute(
            ImperativeRequest::post("application.create").body(serde_json::json!({
                "name": "API",
                "environmentId": "environment-1"
            })),
        )
        .await
        .expect_err("the transient mutation failure is returned without retrying");

    let details = error.dokploy().expect("remote error details are retained");
    assert_eq!(details.status(), 503);
    let request = server.finish();
    assert!(request.starts_with("POST /api/application.create HTTP/1.1\r\n"));
    let (_, body) = request
        .split_once("\r\n\r\n")
        .expect("the request contains a body separator");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).expect("request body is JSON"),
        serde_json::json!({"name": "API", "environmentId": "environment-1"})
    );
}

#[tokio::test]
async fn mutation_transport_failure_after_dispatch_has_an_unknown_outcome() {
    let server = TestServer::close_after_request();
    let secret = "must-not-appear-in-the-error";
    let client = Dokploy::builder()
        .url(server.url())
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let error = client
        .imperative()
        .execute(
            ImperativeRequest::post("application.create").body(serde_json::json!({
                "name": "API",
                "environmentId": "environment-1",
                "env": secret
            })),
        )
        .await
        .expect_err("a closed connection makes mutation outcome unknowable");

    assert!(matches!(
        &error,
        Error::OutcomeUnknown { operation, .. } if *operation == "application.create"
    ));
    assert!(!error.to_string().contains(secret));
    assert!(!format!("{error:?}").contains(secret));

    let requests = server.finish_all();
    assert_eq!(requests.len(), 1, "mutation must not be retried");
    assert!(requests[0].starts_with("POST /api/application.create HTTP/1.1\r\n"));
}

#[tokio::test]
async fn json_responses_reject_declared_and_streamed_bodies_over_the_byte_limit() {
    let declared = TestServer::respond_raw(content_length_response(
        "200 OK",
        MAX_JSON_RESPONSE_BYTES + 1,
        Vec::new(),
    ));
    let error = Dokploy::builder()
        .url(declared.url())
        .api_key("test-api-key")
        .build()
        .unwrap()
        .imperative()
        .execute(ImperativeRequest::get("project.all"))
        .await
        .expect_err("a declared oversized body is rejected before reading it");
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "project.all"
        }
    ));
    declared.finish();

    let streamed = TestServer::respond_raw(chunked_response(
        "200 OK",
        vec![vec![b' '; MAX_JSON_RESPONSE_BYTES], vec![b' '; 1]],
    ));
    let error = Dokploy::builder()
        .url(streamed.url())
        .api_key("test-api-key")
        .build()
        .unwrap()
        .imperative()
        .execute(ImperativeRequest::get("project.all"))
        .await
        .expect_err("a chunked body cannot grow past the limit");
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "project.all"
        }
    ));
    streamed.finish();
}

#[tokio::test]
async fn json_responses_accept_exact_and_under_limit_bodies() {
    let mut exact_body = Vec::with_capacity(MAX_JSON_RESPONSE_BYTES);
    exact_body.push(b'"');
    exact_body.resize(MAX_JSON_RESPONSE_BYTES - 1, b'a');
    exact_body.push(b'"');
    let exact = TestServer::respond_raw(content_length_response(
        "200 OK",
        exact_body.len(),
        exact_body,
    ));
    let response = Dokploy::builder()
        .url(exact.url())
        .api_key("test-api-key")
        .build()
        .unwrap()
        .imperative()
        .execute(ImperativeRequest::get("project.all"))
        .await
        .expect("a JSON body exactly at the limit is accepted");
    assert_eq!(
        response.as_str().expect("response is a string").len(),
        MAX_JSON_RESPONSE_BYTES - 2
    );
    exact.finish();

    let under = TestServer::respond_raw(content_length_response(
        "200 OK",
        11,
        br#"{"ok":true}"#.to_vec(),
    ));
    let response = Dokploy::builder()
        .url(under.url())
        .api_key("test-api-key")
        .build()
        .unwrap()
        .imperative()
        .execute(ImperativeRequest::get("project.all"))
        .await
        .expect("a JSON body under the limit is accepted");
    assert_eq!(response, serde_json::json!({"ok": true}));
    under.finish();
}

#[tokio::test]
async fn oversized_non_success_responses_preserve_only_safe_status_details() {
    const CANARY: &str = "oversized-response-body-canary-do-not-retain";
    let mut body = format!(r#"{{"message":"{CANARY}","padding":""#).into_bytes();
    body.resize(MAX_JSON_RESPONSE_BYTES + 1, b'x');
    let server = TestServer::respond_raw(chunked_response("422 Unprocessable Entity", vec![body]));
    let error = Dokploy::builder()
        .url(server.url())
        .api_key("test-api-key")
        .build()
        .unwrap()
        .imperative()
        .execute(
            ImperativeRequest::post("project.remove")
                .body(serde_json::json!({"projectId": "project-1"})),
        )
        .await
        .expect_err("an oversized rejection is represented without its body");
    let dokploy = error.dokploy().expect("HTTP status remains structured");
    assert_eq!(dokploy.status(), 422);
    assert_eq!(dokploy.code(), "UNPROCESSABLE_ENTITY");
    assert_eq!(dokploy.message(), "Unprocessable Entity");
    assert!(dokploy.issues().is_empty());
    assert!(!error.to_string().contains(CANARY));
    assert!(!format!("{error:?}").contains(CANARY));
    server.finish();
}

#[tokio::test]
async fn malformed_and_oversized_successful_mutation_responses_are_outcome_unknown() {
    let malformed =
        TestServer::respond_raw(content_length_response("200 OK", 9, b"{not-json".to_vec()));
    let error = Dokploy::builder()
        .url(malformed.url())
        .api_key("test-api-key")
        .build()
        .unwrap()
        .imperative()
        .execute(
            ImperativeRequest::post("project.create")
                .body(serde_json::json!({"name": "response-bound-test"})),
        )
        .await
        .expect_err("a malformed accepted mutation response is uncertain");
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "project.create",
            ..
        }
    ));
    malformed.finish();

    let oversized = TestServer::respond_raw(content_length_response(
        "200 OK",
        MAX_JSON_RESPONSE_BYTES + 1,
        Vec::new(),
    ));
    let error = Dokploy::builder()
        .url(oversized.url())
        .api_key("test-api-key")
        .build()
        .unwrap()
        .imperative()
        .execute(
            ImperativeRequest::post("project.create")
                .body(serde_json::json!({"name": "response-bound-test"})),
        )
        .await
        .expect_err("an oversized accepted mutation response is uncertain");
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "project.create",
            ..
        }
    ));
    oversized.finish();
}

#[tokio::test]
async fn malformed_successful_reads_remain_decode_errors() {
    let server =
        TestServer::respond_raw(content_length_response("200 OK", 9, b"{not-json".to_vec()));
    let error = Dokploy::builder()
        .url(server.url())
        .api_key("test-api-key")
        .build()
        .unwrap()
        .imperative()
        .execute(ImperativeRequest::get("project.all"))
        .await
        .expect_err("a malformed read response remains a decode error");
    assert!(matches!(
        error,
        Error::Decode {
            operation: "project.all",
            ..
        }
    ));
    server.finish();
}

#[tokio::test]
async fn imperative_multipart_requests_stream_text_and_file_fields() {
    let server = TestServer::respond_with_json(r#"{"deploymentId":"deployment-1"}"#);
    let directory = tempfile::tempdir().expect("temporary directory exists");
    let archive = directory.path().join("drop.zip");
    std::fs::write(&archive, b"zip fixture bytes").expect("multipart fixture is writable");
    let client = Dokploy::builder()
        .url(server.url())
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let response = client
        .imperative()
        .execute(
            ImperativeRequest::post("application.dropDeployment")
                .multipart_text("applicationId", "application-1")
                .multipart_file("zip", &archive),
        )
        .await
        .expect("multipart operation succeeds");

    assert_eq!(response["deploymentId"], "deployment-1");
    let request = server.finish();
    assert!(request.starts_with("POST /api/application.dropDeployment HTTP/1.1\r\n"));
    assert!(request.contains("content-type: multipart/form-data; boundary="));
    assert!(request.contains("name=\"applicationId\""));
    assert!(request.contains("application-1"));
    assert!(request.contains("name=\"zip\"; filename=\"drop.zip\""));
    assert!(request.contains("zip fixture bytes"));
}

#[tokio::test]
async fn safe_reads_retry_transient_server_failures() {
    let transient_error = r#"{"code":"SERVICE_UNAVAILABLE","message":"Try again"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("503 Service Unavailable", transient_error),
        ("503 Service Unavailable", transient_error),
        ("200 OK", "[]"),
    ]);
    let client = Dokploy::builder()
        .url(server.url())
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let topology = client
        .imperative()
        .execute(ImperativeRequest::get("project.all"))
        .await
        .expect("safe read succeeds after transient failures");

    assert_eq!(topology, serde_json::json!([]));
    assert_eq!(server.finish_all().len(), 3);
}

#[tokio::test]
async fn imperative_gets_retry_transient_server_failures() {
    let transient_error = r#"{"code":"SERVICE_UNAVAILABLE","message":"Try again"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("503 Service Unavailable", transient_error),
        ("200 OK", r#""v0.30.6""#),
    ]);
    let client = Dokploy::builder()
        .url(server.url())
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let version = client
        .imperative()
        .execute(ImperativeRequest::get("settings.getDokployVersion"))
        .await
        .expect("safe imperative read succeeds after a transient failure");

    assert_eq!(version, "v0.30.6");
    assert_eq!(server.finish_all().len(), 2);
}

#[tokio::test]
async fn builder_normalizes_an_existing_api_suffix() {
    let server = TestServer::respond_with_json("[]");
    let client = Dokploy::builder()
        .url(format!("{}/api/", server.url()))
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    client
        .imperative()
        .execute(ImperativeRequest::get("project.all"))
        .await
        .expect("project.all is readable");

    assert!(
        server
            .finish()
            .starts_with("GET /api/project.all HTTP/1.1\r\n")
    );
}

#[test]
fn invalid_api_key_diagnostics_do_not_expose_the_secret() {
    let secret = "secret-value\nthat-is-not-a-valid-header";
    let error = match Dokploy::builder()
        .url("https://deploy.example.com")
        .api_key(secret)
        .build()
    {
        Ok(_) => panic!("invalid API key must be rejected"),
        Err(error) => error,
    };

    assert!(!error.to_string().contains(secret));
    assert!(!format!("{error:?}").contains(secret));
}

#[test]
fn imperative_request_debug_output_redacts_all_user_values() {
    let secret = "secret-query-and-body-value";
    let request = ImperativeRequest::post("application.create")
        .query("token", secret)
        .body(serde_json::json!({"apiKey": secret}));

    let output = format!("{request:?}");

    assert!(!output.contains(secret));
    assert!(output.contains("application.create"));
    assert!(output.contains("body_kind"));
}
