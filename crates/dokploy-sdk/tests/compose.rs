use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ComposeDetails, ComposeId, ComposeVolumePolicy, CreateCompose, Dokploy, EnvironmentId, Error,
    ResponseField, ServerId, UpdateCompose,
};
use zeroize::Zeroizing;

const COMPOSE_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/compose-one.created.owner.json");
const COMPOSE_WITH_SERVER_RESPONSE: &str = r#"{
    "composeId":"compose-1",
    "environmentId":"environment-1",
    "name":"Compose Contract Test",
    "serverId":"server-1"
}"#;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_with_json(body: &'static str) -> Self {
        Self::respond_in_sequence(vec![("200 OK", body)])
    }

    fn respond_in_sequence(responses: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received_requests = Vec::new();

            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                let bytes = read_request(&mut stream);
                received_requests.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
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

fn client(server: &TestServer) -> Dokploy {
    Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid")
}

fn assert_mutation_request(request: &str, operation: &str, expected: serde_json::Value) {
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
        expected
    );
}

#[test]
fn compose_details_fixture_exposes_only_safe_reconciliation_fields() {
    let compose: ComposeDetails =
        serde_json::from_str(COMPOSE_FIXTURE).expect("fixture must deserialize");

    assert_eq!(compose.compose_id.as_str(), "compose-1");
    assert_eq!(compose.environment_id.as_str(), "environment-1");
    assert_eq!(compose.name, "Compose Contract Test");
    assert_eq!(compose.app_name, "compose-contract-test");
    assert_eq!(compose.source_type, ResponseField::Value("raw".to_owned()));
    assert_eq!(
        compose.compose_type,
        ResponseField::Value("docker-compose".to_owned())
    );
    assert_eq!(compose.server_id, ResponseField::Null);

    let debug = format!("{compose:?}");
    assert!(!debug.contains("composeFile"));
    assert!(!debug.contains("refreshToken"));
    assert!(!debug.contains("environment:"));
    assert!(!debug.contains("services:"));
}

#[tokio::test]
async fn compose_get_uses_a_strong_id_and_safe_tolerant_model() {
    let server = TestServer::respond_with_json(COMPOSE_FIXTURE);
    let compose = client(&server)
        .composes()
        .get(ComposeId::new("compose-1"))
        .await
        .expect("Compose record is readable");

    let request = server.finish();
    assert!(request.starts_with("GET /api/compose.one?composeId=compose-1 HTTP/1.1\r\n"));
    assert_eq!(compose.compose_id.as_str(), "compose-1");
    assert_eq!(compose.environment_id.as_str(), "environment-1");
}

#[tokio::test]
async fn compose_by_environment_collects_every_bounded_page() {
    let first_items = (0..100)
        .map(|index| {
            format!(
                r#"{{"composeId":"compose-{index}","environmentId":"environment-1","name":"compose-{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let first_page: &'static str =
        Box::leak(format!(r#"{{"items":[{first_items}],"total":101}}"#).into_boxed_str());
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", first_page),
        (
            "200 OK",
            r#"{"items":[{"composeId":"compose-100","environmentId":"environment-1","name":"compose-100"}],"total":101}"#,
        ),
    ]);

    let collection = client(&server)
        .composes()
        .by_environment(EnvironmentId::new("environment-1"))
        .await
        .expect("Compose collection is readable");

    assert_eq!(collection.composes().len(), 101);
    let requests = server.finish_all();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/compose.search?"));
    assert!(requests[0].contains("environmentId=environment-1"));
    assert!(requests[0].contains("limit=100"));
    assert!(requests[0].contains("offset=0"));
    assert!(requests[1].contains("offset=100"));
}

#[tokio::test]
async fn compose_by_environment_rejects_ambiguous_pagination() {
    let cases = [
        vec![("200 OK", r#"{"items":[],"total":1}"#)],
        vec![("200 OK", r#"{"items":[],"total":10001}"#)],
        vec![(
            "200 OK",
            r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"one"},{"composeId":"compose-2","environmentId":"environment-1","name":"two"}],"total":1}"#,
        )],
        vec![(
            "200 OK",
            r#"{"items":[{"composeId":"compose-1","environmentId":"other-environment","name":"one"}],"total":1}"#,
        )],
        vec![(
            "200 OK",
            r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"one"},{"composeId":"compose-1","environmentId":"environment-1","name":"two"}],"total":2}"#,
        )],
    ];

    for responses in cases {
        let server = TestServer::respond_in_sequence(responses);
        let error = client(&server)
            .composes()
            .by_environment(EnvironmentId::new("environment-1"))
            .await
            .expect_err("ambiguous pagination must fail closed");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "compose.search"
            }
        ));
        server.finish_all();
    }
}

#[tokio::test]
async fn compose_create_redacts_content_and_validates_the_returned_identity() {
    let server = TestServer::respond_with_json(COMPOSE_WITH_SERVER_RESPONSE);
    let input = CreateCompose::new(
        "Compose Contract Test",
        EnvironmentId::new("environment-1"),
        Zeroizing::new("services:\n  private:\n    environment: SECRET=canary\n".to_owned()),
    )
    .with_app_name("compose-contract-test")
    .with_description("Disposable Compose SDK contract")
    .with_server(ServerId::new("server-1"));

    let debug = format!("{input:?}");
    assert!(!debug.contains("SECRET=canary"));

    let created = client(&server)
        .composes()
        .create(input)
        .await
        .expect("Compose creation succeeds");

    assert_eq!(created.compose_id().as_str(), "compose-1");
    assert_mutation_request(
        &server.finish(),
        "compose.create",
        serde_json::json!({
            "name": "Compose Contract Test",
            "description": "Disposable Compose SDK contract",
            "environmentId": "environment-1",
            "composeType": "docker-compose",
            "appName": "compose-contract-test",
            "serverId": "server-1",
            "composeFile": "services:\n  private:\n    environment: SECRET=canary\n",
            "sourceType": "raw"
        }),
    );

    let mismatch = TestServer::respond_with_json(
        r#"{"composeId":"compose-2","environmentId":"other-environment","name":"Compose Contract Test"}"#,
    );
    let error = client(&mismatch)
        .composes()
        .create(CreateCompose::new(
            "Compose Contract Test",
            EnvironmentId::new("environment-1"),
            Zeroizing::new("services: {}".to_owned()),
        ))
        .await
        .expect_err("a conflicting returned parent must fail closed");
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "compose.create"
        }
    ));
    mismatch.finish();

    let server_mismatch = TestServer::respond_with_json(
        r#"{"composeId":"compose-3","environmentId":"environment-1","name":"Compose Contract Test","serverId":null}"#,
    );
    let error = client(&server_mismatch)
        .composes()
        .create(
            CreateCompose::new(
                "Compose Contract Test",
                EnvironmentId::new("environment-1"),
                Zeroizing::new("services: {}".to_owned()),
            )
            .with_server(ServerId::new("server-1")),
        )
        .await
        .expect_err("a conflicting returned server must fail closed");
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "compose.create"
        }
    ));
    server_mismatch.finish();
}

#[tokio::test]
async fn compose_update_and_delete_use_narrow_explicit_requests() {
    let update_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let update = UpdateCompose::new(ComposeId::new("compose-1"))
        .with_name("Compose Contract Updated")
        .clear_description()
        .with_compose_file(Zeroizing::new(
            "services:\n  private:\n    environment: SECRET=updated-canary\n".to_owned(),
        ));
    let debug = format!("{update:?}");
    assert!(!debug.contains("updated-canary"));
    client(&update_server)
        .composes()
        .update(update)
        .await
        .expect("Compose update succeeds");
    assert_mutation_request(
        &update_server.finish(),
        "compose.update",
        serde_json::json!({
            "composeId": "compose-1",
            "name": "Compose Contract Updated",
            "description": null,
            "composeFile": "services:\n  private:\n    environment: SECRET=updated-canary\n"
        }),
    );

    for (policy, expected) in [
        (ComposeVolumePolicy::Preserve, false),
        (ComposeVolumePolicy::Delete, true),
    ] {
        let delete_server = TestServer::respond_with_json(r#"{"ok":true}"#);
        client(&delete_server)
            .composes()
            .delete(ComposeId::new("compose-1"), policy)
            .await
            .expect("Compose deletion succeeds");
        assert_mutation_request(
            &delete_server.finish(),
            "compose.delete",
            serde_json::json!({"composeId": "compose-1", "deleteVolumes": expected}),
        );
    }
}

#[tokio::test]
async fn compose_rejects_invalid_inputs_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let errors = [
        client
            .composes()
            .by_environment(EnvironmentId::new(""))
            .await
            .expect_err("an empty parent ID is invalid"),
        client
            .composes()
            .create(CreateCompose::new(
                "",
                EnvironmentId::new("environment-1"),
                Zeroizing::new("services: {}".to_owned()),
            ))
            .await
            .expect_err("an empty name is invalid"),
        client
            .composes()
            .update(UpdateCompose::new(ComposeId::new("compose-1")))
            .await
            .expect_err("an empty update is invalid"),
        client
            .composes()
            .delete(ComposeId::new(""), ComposeVolumePolicy::Preserve)
            .await
            .expect_err("an empty identity is invalid"),
    ];

    assert!(errors.iter().all(|error| matches!(
        error,
        Error::InvalidRequest { operation, .. } if operation.starts_with("compose.")
    )));
}

#[tokio::test]
async fn compose_delete_preserves_rejections_and_unknown_transport_outcomes() {
    let rejected = TestServer::respond_in_sequence(vec![(
        "400 Bad Request",
        r#"{"code":"BAD_REQUEST","message":"Compose cannot be deleted"}"#,
    )]);
    let error = client(&rejected)
        .composes()
        .delete(ComposeId::new("compose-1"), ComposeVolumePolicy::Preserve)
        .await
        .expect_err("Dokploy rejection must stay structured");
    let details = error
        .dokploy()
        .expect("Dokploy error details are preserved");
    assert_eq!(details.status(), 400);
    assert_eq!(details.code(), "BAD_REQUEST");
    rejected.finish();

    let unknown = TestServer::close_after_request();
    let error = client(&unknown)
        .composes()
        .delete(ComposeId::new("compose-1"), ComposeVolumePolicy::Preserve)
        .await
        .expect_err("missing mutation response has an unknown outcome");
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "compose.delete",
            ..
        }
    ));
    unknown.finish();
}
