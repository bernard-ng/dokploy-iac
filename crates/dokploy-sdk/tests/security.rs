use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ApplicationId, CreateSecurity, Dokploy, Error, SecurityDetails, SecurityId, UpdateSecurity,
};
use zeroize::Zeroizing;

const PASSWORD_CANARY: &str = "response-password-canary-do-not-leak";
const REQUEST_PASSWORD_CANARY: &str = "request-password-canary-do-not-leak";
const SECURITY_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/security-one.created.owner.json");
const SECURITY_RESPONSE: &str = r#"{
    "securityId":"security-1",
    "username":"owner",
    "password":"response-password-canary-do-not-leak",
    "applicationId":"application-1",
    "application":{"env":"nested-secret-canary"},
    "futureRuntimeField":{"nested":true}
}"#;
const EMPTY_PARENT: &str = r#"{"applicationId":"application-1","security":[]}"#;

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
                received_requests
                    .push(String::from_utf8(read_request(&mut stream)).expect("request is UTF-8"));
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

    fn close_after_requests(responses: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received_requests = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                received_requests
                    .push(String::from_utf8(read_request(&mut stream)).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }
            let (mut stream, _) = listener.accept().expect("test server accepts mutation");
            received_requests
                .push(String::from_utf8(read_request(&mut stream)).expect("request is UTF-8"));
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

fn input() -> CreateSecurity {
    CreateSecurity::new(
        ApplicationId::new("application-1"),
        "owner",
        Zeroizing::new(REQUEST_PASSWORD_CANARY.to_owned()),
    )
}

fn parent_with(entries: &str) -> &'static str {
    Box::leak(
        format!(
            r#"{{"applicationId":"application-1","security":[{entries}],"env":"parent-secret-canary","future":true}}"#
        )
        .into_boxed_str(),
    )
}

fn assert_mutation_request(request: &str, operation: &str, expected: serde_json::Value) {
    assert!(request.starts_with(&format!("POST /api/{operation} HTTP/1.1\r\n")));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: test-api-key")
    );
    let body = request.split_once("\r\n\r\n").expect("request has body").1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        expected
    );
}

fn assert_no_canary(value: impl std::fmt::Debug) {
    let rendered = format!("{value:?}");
    for canary in [
        PASSWORD_CANARY,
        REQUEST_PASSWORD_CANARY,
        "nested-secret-canary",
    ] {
        assert!(
            !rendered.contains(canary),
            "secret canary leaked: {rendered}"
        );
    }
}

#[test]
fn security_response_consumes_password_and_retains_only_presence() {
    let details: SecurityDetails = serde_json::from_str(SECURITY_RESPONSE).unwrap();
    assert_eq!(details.security_id.as_str(), "security-1");
    assert_eq!(details.application_id.as_str(), "application-1");
    assert_eq!(details.username, "owner");
    assert!(details.password_present);
    assert_no_canary(&details);

    let fixture: SecurityDetails = serde_json::from_str(SECURITY_FIXTURE).unwrap();
    assert!(fixture.password_present);
    assert!(!SECURITY_FIXTURE.contains(PASSWORD_CANARY));
    assert!(SECURITY_FIXTURE.contains("<redacted>"));

    for (password, present) in [("\"\"", false), ("null", false)] {
        let body = SECURITY_RESPONSE.replace(&format!("\"{PASSWORD_CANARY}\""), password);
        let details: SecurityDetails = serde_json::from_str(&body).unwrap();
        assert_eq!(details.password_present, present);
        assert_no_canary(&details);
    }

    let invalid = SECURITY_RESPONSE.replace(&format!("\"{PASSWORD_CANARY}\""), "42");
    let error = serde_json::from_str::<SecurityDetails>(&invalid).unwrap_err();
    assert_no_canary(&error);

    for field in ["securityId", "applicationId", "username", "password"] {
        let mut invalid: serde_json::Value = serde_json::from_str(SECURITY_RESPONSE).unwrap();
        invalid.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<SecurityDetails>(invalid).is_err());
    }
}

#[tokio::test]
async fn security_get_requires_safe_direct_and_parent_agreement() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", SECURITY_RESPONSE),
        ("200 OK", parent_with(SECURITY_RESPONSE)),
    ]);
    let details = client(&server)
        .security()
        .get(SecurityId::new("security-1"))
        .await
        .unwrap();
    assert!(details.password_present);
    assert_no_canary(&details);
    let requests = server.finish_all();
    assert!(requests[0].starts_with("GET /api/security.one?securityId=security-1 HTTP/1.1\r\n"));
    assert!(
        requests[1]
            .starts_with("GET /api/application.one?applicationId=application-1 HTTP/1.1\r\n")
    );

    for conflict in [
        SECURITY_RESPONSE.replace("\"username\":\"owner\"", "\"username\":\"other\""),
        SECURITY_RESPONSE.replace(&format!("\"{PASSWORD_CANARY}\""), "\"\""),
    ] {
        let server = TestServer::respond_in_sequence(vec![
            ("200 OK", SECURITY_RESPONSE),
            ("200 OK", parent_with(Box::leak(conflict.into_boxed_str()))),
        ]);
        let error = client(&server)
            .security()
            .get(SecurityId::new("security-1"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "security.one"
            }
        ));
        assert_no_canary(&error);
        server.finish_all();
    }
}

#[tokio::test]
async fn security_decode_errors_do_not_echo_response_canaries() {
    let invalid = SECURITY_RESPONSE.replace(
        &format!("\"{PASSWORD_CANARY}\""),
        &format!(r#"{{"canary":"{PASSWORD_CANARY}"}}"#),
    );
    let server = TestServer::respond_with_json(Box::leak(invalid.into_boxed_str()));
    let error = client(&server)
        .security()
        .get(SecurityId::new("security-1"))
        .await
        .expect_err("non-string password responses must fail safely");

    assert_no_canary(&error);
    assert_eq!(server.finish_all().len(), 1);
}

#[tokio::test]
async fn application_one_is_authoritative_and_rejects_duplicate_ids_or_usernames() {
    let server = TestServer::respond_with_json(parent_with(SECURITY_RESPONSE));
    let collection = client(&server)
        .security()
        .by_application(ApplicationId::new("application-1"))
        .await
        .unwrap();
    assert_eq!(collection.entries().len(), 1);
    assert_no_canary(&collection);
    server.finish();

    let duplicate_id =
        SECURITY_RESPONSE.replace("\"username\":\"owner\"", "\"username\":\"other\"");
    let duplicate_username = SECURITY_RESPONSE.replace("security-1", "security-2");
    let wrong_parent = SECURITY_RESPONSE.replace("application-1", "application-2");
    let wrong_root = Box::leak(
        format!(r#"{{"applicationId":"application-2","security":[{SECURITY_RESPONSE}]}}"#)
            .into_boxed_str(),
    );
    let too_many = (0..10_001)
        .map(|index| {
            SECURITY_RESPONSE
                .replace("security-1", &format!("security-{index}"))
                .replace("\"owner\"", &format!("\"owner-{index}\""))
        })
        .collect::<Vec<_>>()
        .join(",");

    for body in [
        parent_with(&format!("{SECURITY_RESPONSE},{duplicate_id}")),
        parent_with(&format!("{SECURITY_RESPONSE},{duplicate_username}")),
        parent_with(&wrong_parent),
        wrong_root,
        parent_with(&too_many),
    ] {
        let server = TestServer::respond_with_json(body);
        let error = client(&server)
            .security()
            .by_application(ApplicationId::new("application-1"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "application.one"
            }
        ));
        assert_no_canary(&error);
        server.finish();
    }

    let server = TestServer::respond_with_json(r#"{"applicationId":"application-1"}"#);
    let error = client(&server)
        .security()
        .by_application(ApplicationId::new("application-1"))
        .await
        .expect_err("an omitted authoritative Security relation must fail closed");
    assert!(matches!(
        error,
        Error::Decode {
            operation: "application.one",
            ..
        }
    ));
    assert_no_canary(&error);
    server.finish();
}

#[tokio::test]
async fn security_create_proves_exactly_one_new_matching_identity() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", EMPTY_PARENT),
        ("200 OK", "true"),
        ("200 OK", parent_with(SECURITY_RESPONSE)),
    ]);
    let input = input();
    assert_no_canary(&input);
    let created = client(&server).security().create(input).await.unwrap();
    assert_eq!(created.security_id().as_str(), "security-1");
    let requests = server.finish_all();
    assert_eq!(requests.len(), 3);
    assert_mutation_request(
        &requests[1],
        "security.create",
        serde_json::json!({
            "applicationId": "application-1",
            "username": "owner",
            "password": REQUEST_PASSWORD_CANARY
        }),
    );
}

#[tokio::test]
async fn security_create_rejects_collisions_and_ambiguous_evidence() {
    let collision_server = TestServer::respond_with_json(parent_with(SECURITY_RESPONSE));
    let collision = client(&collision_server)
        .security()
        .create(input())
        .await
        .unwrap_err();
    assert!(matches!(
        collision,
        Error::UnexpectedResponse {
            operation: "security.create"
        }
    ));
    assert_eq!(collision_server.finish_all().len(), 1);

    let second = SECURITY_RESPONSE
        .replace("security-1", "security-2")
        .replace("\"owner\"", "\"second\"");
    let existing = SECURITY_RESPONSE
        .replace("security-1", "existing")
        .replace("\"owner\"", "\"existing\"");
    let cases = [
        (EMPTY_PARENT, "true", EMPTY_PARENT),
        (
            EMPTY_PARENT,
            "true",
            parent_with(&format!("{SECURITY_RESPONSE},{second}")),
        ),
        (
            parent_with(&existing),
            "true",
            parent_with(SECURITY_RESPONSE),
        ),
        (EMPTY_PARENT, "false", EMPTY_PARENT),
    ];
    for (before, accepted, after) in cases {
        let responses = if accepted == "false" {
            vec![("200 OK", before), ("200 OK", accepted)]
        } else {
            vec![("200 OK", before), ("200 OK", accepted), ("200 OK", after)]
        };
        let expected = responses.len();
        let server = TestServer::respond_in_sequence(responses);
        let error = client(&server)
            .security()
            .create(input())
            .await
            .unwrap_err();
        if accepted == "true" {
            assert!(matches!(
                error,
                Error::OutcomeUnknown {
                    operation: "security.create",
                    ..
                }
            ));
        } else {
            assert!(matches!(
                error,
                Error::UnexpectedResponse {
                    operation: "security.create"
                }
            ));
        }
        assert_no_canary(&error);
        assert_eq!(server.finish_all().len(), expected);
    }
}

#[tokio::test]
async fn security_create_postflight_failures_are_secret_safe_outcome_unknown() {
    let closed =
        TestServer::close_after_requests(vec![("200 OK", EMPTY_PARENT), ("200 OK", "true")]);
    let error = client(&closed)
        .security()
        .create(input())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "security.create",
            ..
        }
    ));
    assert_no_canary(&error);
    assert_eq!(closed.finish_all().len(), 3);

    for (status, body) in [
        ("200 OK", r#"{"applicationId":"application-1"}"#),
        (
            "500 Internal Server Error",
            r#"{"message":"request-password-canary-do-not-leak","issues":["response-password-canary-do-not-leak"]}"#,
        ),
    ] {
        let server = TestServer::respond_in_sequence(vec![
            ("200 OK", EMPTY_PARENT),
            ("200 OK", "true"),
            (status, body),
        ]);
        let error = client(&server)
            .security()
            .create(input())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::OutcomeUnknown {
                operation: "security.create",
                ..
            }
        ));
        assert_no_canary(&error);
        assert_eq!(server.finish_all().len(), 3);
    }

    let preflight = TestServer::respond_with_json(r#"{"applicationId":"application-1"}"#);
    let error = client(&preflight)
        .security()
        .create(input())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Decode {
            operation: "application.one",
            ..
        }
    ));
    assert_no_canary(&error);
    assert_eq!(preflight.finish_all().len(), 1);
}

#[tokio::test]
async fn security_mutation_rejections_never_retain_echoed_passwords() {
    let echoed = r#"{
        "code":"ECHOED_PASSWORD",
        "message":"request-password-canary-do-not-leak",
        "issues":[{"message":"response-password-canary-do-not-leak"}]
    }"#;
    let create_server = TestServer::respond_in_sequence(vec![
        ("200 OK", EMPTY_PARENT),
        ("400 Bad Request", echoed),
    ]);
    let error = client(&create_server)
        .security()
        .create(input())
        .await
        .unwrap_err();
    let dokploy = error.dokploy().expect("HTTP status remains structured");
    assert_eq!(dokploy.status(), 400);
    assert_eq!(dokploy.code(), "BAD_REQUEST");
    assert_eq!(dokploy.message(), "Bad Request");
    assert!(dokploy.issues().is_empty());
    assert_no_canary(&error);
    assert_eq!(create_server.finish_all().len(), 2);

    let update_server = TestServer::respond_in_sequence(vec![("422 Unprocessable Entity", echoed)]);
    let error = client(&update_server)
        .security()
        .update(UpdateSecurity::new(
            SecurityId::new("security-1"),
            "operator",
            Zeroizing::new(REQUEST_PASSWORD_CANARY.to_owned()),
        ))
        .await
        .unwrap_err();
    let dokploy = error.dokploy().expect("HTTP status remains structured");
    assert_eq!(dokploy.status(), 422);
    assert_eq!(dokploy.code(), "UNPROCESSABLE_ENTITY");
    assert_eq!(dokploy.message(), "Unprocessable Entity");
    assert!(dokploy.issues().is_empty());
    assert_no_canary(&error);
    assert_eq!(update_server.finish_all().len(), 1);
}

#[tokio::test]
async fn security_update_and_delete_use_exact_complete_requests() {
    let update = UpdateSecurity::new(
        SecurityId::new("security-1"),
        "operator",
        Zeroizing::new(REQUEST_PASSWORD_CANARY.to_owned()),
    );
    assert_no_canary(&update);
    let update_server = TestServer::respond_with_json("true");
    client(&update_server)
        .security()
        .update(update)
        .await
        .unwrap();
    assert_mutation_request(
        &update_server.finish(),
        "security.update",
        serde_json::json!({
            "securityId": "security-1",
            "username": "operator",
            "password": REQUEST_PASSWORD_CANARY
        }),
    );

    let delete_server = TestServer::respond_with_json("true");
    client(&delete_server)
        .security()
        .delete(SecurityId::new("security-1"))
        .await
        .unwrap();
    assert_mutation_request(
        &delete_server.finish(),
        "security.delete",
        serde_json::json!({"securityId": "security-1"}),
    );
}

#[tokio::test]
async fn security_rejects_empty_inputs_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .unwrap();
    let errors = [
        client
            .security()
            .get(SecurityId::new(""))
            .await
            .unwrap_err(),
        client
            .security()
            .by_application(ApplicationId::new(""))
            .await
            .unwrap_err(),
        client
            .security()
            .create(CreateSecurity::new(
                ApplicationId::new("application-1"),
                "owner",
                Zeroizing::new(String::new()),
            ))
            .await
            .unwrap_err(),
        client
            .security()
            .update(UpdateSecurity::new(
                SecurityId::new("security-1"),
                "operator",
                Zeroizing::new(String::new()),
            ))
            .await
            .unwrap_err(),
        client
            .security()
            .delete(SecurityId::new(""))
            .await
            .unwrap_err(),
    ];
    assert!(errors.iter().all(|error| matches!(
        error,
        Error::InvalidRequest { operation, .. }
            if operation == &"application.one" || operation.starts_with("security.")
    )));
    for error in errors {
        assert_no_canary(error);
    }
}

#[tokio::test]
async fn security_mutations_are_single_attempt_with_secret_safe_unknown_outcomes() {
    let create_server = TestServer::close_after_requests(vec![("200 OK", EMPTY_PARENT)]);
    let create_error = client(&create_server)
        .security()
        .create(input())
        .await
        .unwrap_err();
    assert!(matches!(
        create_error,
        Error::OutcomeUnknown {
            operation: "security.create",
            ..
        }
    ));
    assert_no_canary(&create_error);
    assert_eq!(create_server.finish_all().len(), 2);

    let update_server = TestServer::close_after_requests(vec![]);
    let update_error = client(&update_server)
        .security()
        .update(UpdateSecurity::new(
            SecurityId::new("security-1"),
            "operator",
            Zeroizing::new(REQUEST_PASSWORD_CANARY.to_owned()),
        ))
        .await
        .unwrap_err();
    assert!(matches!(
        update_error,
        Error::OutcomeUnknown {
            operation: "security.update",
            ..
        }
    ));
    assert_no_canary(&update_error);
    assert_eq!(update_server.finish_all().len(), 1);

    let delete_server = TestServer::close_after_requests(vec![]);
    let delete_error = client(&delete_server)
        .security()
        .delete(SecurityId::new("security-1"))
        .await
        .unwrap_err();
    assert!(matches!(
        delete_error,
        Error::OutcomeUnknown {
            operation: "security.delete",
            ..
        }
    ));
    assert_no_canary(&delete_error);
    assert_eq!(delete_server.finish_all().len(), 1);
}
