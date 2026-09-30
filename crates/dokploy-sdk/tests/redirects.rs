use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ApplicationId, CreateRedirect, Dokploy, Error, RedirectDetails, RedirectId, UpdateRedirect,
};

const REDIRECT_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/redirect-one.created.owner.json");
const REDIRECT_RESPONSE: &str = r#"{
    "redirectId":"redirect-1",
    "regex":"^/old/(.*)$",
    "replacement":"/new/$1",
    "permanent":false,
    "applicationId":"application-1",
    "application":{"env":"nested-secret-canary"},
    "futureRuntimeField":{"nested":true}
}"#;
const EMPTY_PARENT: &str = r#"{"applicationId":"application-1","redirects":[]}"#;

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

    fn close_after_requests(responses: Vec<(&'static str, &'static str)>) -> Self {
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
            let (mut stream, _) = listener.accept().expect("test server accepts mutation");
            let bytes = read_request(&mut stream);
            received_requests.push(String::from_utf8(bytes).expect("request is UTF-8"));
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

fn input() -> CreateRedirect {
    CreateRedirect::new(
        ApplicationId::new("application-1"),
        "^/old/(.*)$",
        "/new/$1",
        false,
    )
}

fn parent_with(redirects: &str) -> &'static str {
    Box::leak(
        format!(
            r#"{{"applicationId":"application-1","redirects":[{redirects}],"env":"parent-secret-canary","future":true}}"#
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
fn redirect_response_is_safe_tolerant_and_requires_owned_fields() {
    let details: RedirectDetails =
        serde_json::from_str(REDIRECT_FIXTURE).expect("fixture deserializes");

    assert_eq!(details.redirect_id.as_str(), "redirect-1");
    assert_eq!(details.application_id.as_str(), "application-1");
    assert_eq!(details.regex, "^/legacy/(.*)$");
    assert_eq!(details.replacement, "/current/$1");
    assert!(!details.permanent);
    assert!(!format!("{details:?}").contains("nested-secret-canary"));

    for field in [
        "redirectId",
        "applicationId",
        "regex",
        "replacement",
        "permanent",
    ] {
        let mut invalid =
            serde_json::from_str::<serde_json::Value>(REDIRECT_RESPONSE).expect("response is JSON");
        invalid
            .as_object_mut()
            .expect("response is an object")
            .remove(field);
        serde_json::from_value::<RedirectDetails>(invalid)
            .expect_err("required Redirect fields must not be omitted");
    }
}

#[tokio::test]
async fn redirect_get_requires_direct_and_parent_agreement() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", REDIRECT_RESPONSE),
        ("200 OK", parent_with(REDIRECT_RESPONSE)),
    ]);
    let details = client(&server)
        .redirects()
        .get(RedirectId::new("redirect-1"))
        .await
        .expect("Redirect is authoritatively readable");

    assert_eq!(details.regex, "^/old/(.*)$");
    let requests = server.finish_all();
    assert!(requests[0].starts_with("GET /api/redirects.one?redirectId=redirect-1 HTTP/1.1\r\n"));
    assert!(
        requests[1]
            .starts_with("GET /api/application.one?applicationId=application-1 HTTP/1.1\r\n")
    );

    let conflicting = REDIRECT_RESPONSE.replace("\"permanent\":false", "\"permanent\":true");
    let conflicting = Box::leak(conflicting.into_boxed_str());
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", REDIRECT_RESPONSE),
        ("200 OK", parent_with(conflicting)),
    ]);
    let error = client(&server)
        .redirects()
        .get(RedirectId::new("redirect-1"))
        .await
        .expect_err("direct and parent disagreement must fail closed");
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "redirects.one"
        }
    ));
    server.finish_all();

    let invalid = REDIRECT_RESPONSE.replace("^/old/(.*)$", "");
    let server = TestServer::respond_with_json(Box::leak(invalid.into_boxed_str()));
    let error = client(&server)
        .redirects()
        .get(RedirectId::new("redirect-1"))
        .await
        .expect_err("empty owned response fields must fail before parent lookup");
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "redirects.one"
        }
    ));
    assert_eq!(server.finish_all().len(), 1);
}

#[tokio::test]
async fn application_one_is_the_authoritative_bounded_redirect_collection() {
    let server = TestServer::respond_with_json(parent_with(REDIRECT_RESPONSE));
    let collection = client(&server)
        .redirects()
        .by_application(ApplicationId::new("application-1"))
        .await
        .expect("application Redirect collection is readable");

    assert_eq!(collection.application_id().as_str(), "application-1");
    assert_eq!(collection.redirects().len(), 1);
    assert!(!format!("{collection:?}").contains("parent-secret-canary"));
    assert!(
        server
            .finish()
            .starts_with("GET /api/application.one?applicationId=application-1 HTTP/1.1\r\n")
    );

    let duplicate = parent_with(&format!("{REDIRECT_RESPONSE},{REDIRECT_RESPONSE}"));
    let wrong_parent = parent_with(&REDIRECT_RESPONSE.replace("application-1", "application-2"));
    let wrong_root = Box::leak(
        format!(r#"{{"applicationId":"application-2","redirects":[{REDIRECT_RESPONSE}]}}"#)
            .into_boxed_str(),
    );
    let too_many = (0..10_001)
        .map(|index| REDIRECT_RESPONSE.replace("redirect-1", &format!("redirect-{index}")))
        .collect::<Vec<_>>()
        .join(",");
    let too_many = parent_with(&too_many);

    for body in [duplicate, wrong_parent, wrong_root, too_many] {
        let server = TestServer::respond_with_json(body);
        let error = client(&server)
            .redirects()
            .by_application(ApplicationId::new("application-1"))
            .await
            .expect_err("contradictory parent evidence must fail closed");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "application.one"
            }
        ));
        server.finish();
    }
}

#[tokio::test]
async fn redirect_create_discovers_exactly_one_new_matching_identity() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", EMPTY_PARENT),
        ("200 OK", "true"),
        ("200 OK", parent_with(REDIRECT_RESPONSE)),
    ]);
    let created = client(&server)
        .redirects()
        .create(input())
        .await
        .expect("Redirect creation discovers one identity");

    assert_eq!(created.redirect_id().as_str(), "redirect-1");
    let requests = server.finish_all();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .starts_with("GET /api/application.one?applicationId=application-1 HTTP/1.1\r\n")
    );
    assert_mutation_request(
        &requests[1],
        "redirects.create",
        serde_json::json!({
            "applicationId": "application-1",
            "regex": "^/old/(.*)$",
            "replacement": "/new/$1",
            "permanent": false
        }),
    );
    assert!(
        requests[2]
            .starts_with("GET /api/application.one?applicationId=application-1 HTTP/1.1\r\n")
    );
}

#[tokio::test]
async fn redirect_create_rejects_collisions_and_ambiguous_identity_evidence() {
    let collision_server = TestServer::respond_with_json(parent_with(REDIRECT_RESPONSE));
    let collision = client(&collision_server)
        .redirects()
        .create(input())
        .await
        .expect_err("application and regex collision must stop before mutation");
    assert!(matches!(
        collision,
        Error::UnexpectedResponse {
            operation: "redirects.create"
        }
    ));
    assert_eq!(collision_server.finish_all().len(), 1);

    let second = REDIRECT_RESPONSE.replace("redirect-1", "redirect-2");
    let ambiguous_parent = parent_with(&format!("{REDIRECT_RESPONSE},{second}"));
    let missing_before = REDIRECT_RESPONSE
        .replace("redirect-1", "preexisting")
        .replace("^/old/(.*)$", "^/unrelated$");
    let missing_before_parent = parent_with(&missing_before);
    let cases = [
        (EMPTY_PARENT, "true", EMPTY_PARENT),
        (EMPTY_PARENT, "true", ambiguous_parent),
        (
            missing_before_parent,
            "true",
            parent_with(REDIRECT_RESPONSE),
        ),
        (EMPTY_PARENT, "false", EMPTY_PARENT),
    ];

    for (before, accepted, after) in cases {
        let response_count = if accepted == "false" { 2 } else { 3 };
        let responses = if response_count == 2 {
            vec![("200 OK", before), ("200 OK", accepted)]
        } else {
            vec![("200 OK", before), ("200 OK", accepted), ("200 OK", after)]
        };
        let server = TestServer::respond_in_sequence(responses);
        let error = client(&server)
            .redirects()
            .create(input())
            .await
            .expect_err("unproven create identity must fail closed");
        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "redirects.create"
            }
        ));
        assert_eq!(server.finish_all().len(), response_count);
    }
}

#[tokio::test]
async fn redirect_update_and_delete_use_exact_complete_requests() {
    let update_server = TestServer::respond_with_json("true");
    client(&update_server)
        .redirects()
        .update(UpdateRedirect::new(
            RedirectId::new("redirect-1"),
            "^/legacy/(.*)$",
            "/current/$1",
            true,
        ))
        .await
        .expect("Redirect update succeeds");
    assert_mutation_request(
        &update_server.finish(),
        "redirects.update",
        serde_json::json!({
            "redirectId": "redirect-1",
            "regex": "^/legacy/(.*)$",
            "replacement": "/current/$1",
            "permanent": true
        }),
    );

    let delete_server = TestServer::respond_with_json("true");
    client(&delete_server)
        .redirects()
        .delete(RedirectId::new("redirect-1"))
        .await
        .expect("Redirect deletion succeeds");
    assert_mutation_request(
        &delete_server.finish(),
        "redirects.delete",
        serde_json::json!({"redirectId": "redirect-1"}),
    );
}

#[tokio::test]
async fn redirect_rejects_empty_inputs_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let errors = [
        client
            .redirects()
            .get(RedirectId::new(""))
            .await
            .expect_err("an empty Redirect ID is invalid"),
        client
            .redirects()
            .by_application(ApplicationId::new(""))
            .await
            .expect_err("an empty application ID is invalid"),
        client
            .redirects()
            .create(CreateRedirect::new(
                ApplicationId::new("application-1"),
                "",
                "/new",
                false,
            ))
            .await
            .expect_err("an empty regex is invalid"),
        client
            .redirects()
            .update(UpdateRedirect::new(
                RedirectId::new("redirect-1"),
                "^/old$",
                "",
                false,
            ))
            .await
            .expect_err("an empty replacement is invalid"),
        client
            .redirects()
            .delete(RedirectId::new(""))
            .await
            .expect_err("an empty Redirect ID is invalid"),
    ];

    assert!(errors.iter().all(|error| matches!(
        error,
        Error::InvalidRequest { operation, .. }
            if operation == &"application.one" || operation.starts_with("redirects.")
    )));
}

#[tokio::test]
async fn redirect_mutations_are_single_attempt_and_report_unknown_outcomes() {
    let create_server = TestServer::close_after_requests(vec![("200 OK", EMPTY_PARENT)]);
    let create_error = client(&create_server)
        .redirects()
        .create(input())
        .await
        .expect_err("missing create response has an unknown outcome");
    assert!(matches!(
        create_error,
        Error::OutcomeUnknown {
            operation: "redirects.create",
            ..
        }
    ));
    assert_eq!(create_server.finish_all().len(), 2);

    for (operation, action) in [(
        "redirects.update",
        UpdateRedirect::new(RedirectId::new("redirect-1"), "^/legacy$", "/current", true),
    )] {
        let server = TestServer::close_after_requests(vec![]);
        let error = client(&server)
            .redirects()
            .update(action)
            .await
            .expect_err("missing update response has an unknown outcome");
        assert!(matches!(
            error,
            Error::OutcomeUnknown { operation: actual, .. } if actual == operation
        ));
        assert_eq!(server.finish_all().len(), 1);
    }

    let delete_server = TestServer::close_after_requests(vec![]);
    let delete_error = client(&delete_server)
        .redirects()
        .delete(RedirectId::new("redirect-1"))
        .await
        .expect_err("missing delete response has an unknown outcome");
    assert!(matches!(
        delete_error,
        Error::OutcomeUnknown {
            operation: "redirects.delete",
            ..
        }
    ));
    assert_eq!(delete_server.finish_all().len(), 1);
}
