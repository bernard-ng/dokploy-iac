//! Tag adapter: authoritative reads, mutations, and project associations.
//!
//! The response shapes are pinned by these tests, not by a live capture: the
//! generated contract types tag responses as untyped JSON. Every shape the
//! adapter does not recognize fails closed with `UnexpectedResponse`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    CreateTag, Dokploy, Error, ProjectDetails, ProjectId, ResponseField, TagDetails, TagId,
    UpdateTag,
};

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_with_json(body: impl Into<String>) -> Self {
        Self::respond_in_sequence(vec![("200 OK", body.into())])
    }

    fn respond_in_sequence(responses: Vec<(&'static str, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                let bytes = read_request(&mut stream);
                received.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }
            sender.send(received).expect("test receives the requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    /// A server that must not be contacted: local validation failures send nothing.
    fn untouched() -> Self {
        Self::respond_in_sequence(Vec::new())
    }

    fn finish(self) -> Vec<String> {
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

fn tag(id: &str, name: &str, color: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "tagId": id, "name": name, "color": color,
        "createdAt": "2026-01-01T00:00:00.000Z", "organizationId": "org-1",
    })
}

fn body_of(request: &str) -> serde_json::Value {
    let body = request
        .split_once("\r\n\r\n")
        .expect("request has a body")
        .1;
    serde_json::from_str(body).expect("request body is JSON")
}

fn assert_post(request: &str, operation: &str, expected: serde_json::Value) {
    assert!(
        request.starts_with(&format!("POST /api/{operation} HTTP/1.1\r\n")),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: test-api-key")
    );
    assert_eq!(body_of(request), expected);
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn all_reads_the_authoritative_collection_and_keeps_color_presence() {
    let server = TestServer::respond_with_json(
        serde_json::json!([
            tag("tag-1", "prod", Some("#e11d48")),
            tag("tag-2", "plain", None),
            {"tagId": "tag-3", "name": "sparse", "futureField": {"nested": true}},
        ])
        .to_string(),
    );

    let collection = client(&server)
        .tags()
        .all()
        .await
        .expect("tags are readable");

    let tags = collection.tags();
    assert_eq!(tags.len(), 3);
    assert_eq!(tags[0].color, ResponseField::Value("#e11d48".to_owned()));
    assert_eq!(tags[1].color, ResponseField::Null);
    assert_eq!(tags[2].color, ResponseField::NotReturned);
    let requests = server.finish();
    assert!(
        requests[0].starts_with("GET /api/tag.all HTTP/1.1\r\n"),
        "{requests:?}"
    );
}

#[tokio::test]
async fn all_rejects_contradictory_collections() {
    let cases = [
        (
            "duplicate identity",
            serde_json::json!([tag("t", "a", None), tag("t", "b", None)]),
        ),
        (
            "duplicate name",
            serde_json::json!([tag("t1", "a", None), tag("t2", "a", None)]),
        ),
        ("empty name", serde_json::json!([tag("t", "", None)])),
        ("empty identity", serde_json::json!([tag("", "a", None)])),
    ];
    for (name, body) in cases {
        let server = TestServer::respond_with_json(body.to_string());
        let error = client(&server)
            .tags()
            .all()
            .await
            .expect_err("a contradictory collection must fail closed");
        assert!(
            matches!(error, Error::UnexpectedResponse { .. }),
            "{name}: {error}"
        );
        server.finish();
    }
}

#[tokio::test]
async fn all_rejects_a_collection_over_the_item_bound_and_a_non_array() {
    let too_many = (0..10_001)
        .map(|index| tag(&format!("t{index}"), &format!("n{index}"), None))
        .collect::<Vec<_>>();
    let server = TestServer::respond_with_json(serde_json::Value::Array(too_many).to_string());
    let error = client(&server).tags().all().await.expect_err("bounded");
    assert!(matches!(error, Error::UnexpectedResponse { .. }), "{error}");
    server.finish();

    let server = TestServer::respond_with_json(r#"{"tags":[]}"#);
    client(&server)
        .tags()
        .all()
        .await
        .expect_err("a non-array response is not a tag collection");
    server.finish();
}

#[tokio::test]
async fn get_reads_directly_and_requires_the_collection_to_agree() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", tag("tag-1", "prod", Some("#e11d48")).to_string()),
        (
            "200 OK",
            serde_json::json!([tag("tag-1", "prod", Some("#e11d48"))]).to_string(),
        ),
    ]);

    let details = client(&server)
        .tags()
        .get(TagId::new("tag-1"))
        .await
        .expect("tag is readable");

    assert_eq!(details.name, "prod");
    let requests = server.finish();
    assert!(
        requests[0].starts_with("GET /api/tag.one?tagId=tag-1 HTTP/1.1\r\n"),
        "{requests:?}"
    );
    assert!(requests[1].starts_with("GET /api/tag.all HTTP/1.1\r\n"));
}

#[tokio::test]
async fn get_fails_closed_on_identity_or_collection_disagreement() {
    // The direct read is checked before the collection is requested, so the first
    // case stops after one request.
    let cases = [
        (
            "different identity",
            vec![("200 OK", tag("tag-other", "prod", None).to_string())],
        ),
        (
            "different name",
            vec![
                ("200 OK", tag("tag-1", "prod", None).to_string()),
                (
                    "200 OK",
                    serde_json::json!([tag("tag-1", "renamed", None)]).to_string(),
                ),
            ],
        ),
        (
            "absent from the collection",
            vec![
                ("200 OK", tag("tag-1", "prod", None).to_string()),
                ("200 OK", serde_json::json!([]).to_string()),
            ],
        ),
    ];
    for (name, responses) in cases {
        let expected = responses.len();
        let server = TestServer::respond_in_sequence(responses);
        let error = client(&server)
            .tags()
            .get(TagId::new("tag-1"))
            .await
            .expect_err("disagreement must fail closed");
        assert!(
            matches!(error, Error::UnexpectedResponse { .. }),
            "{name}: {error}"
        );
        assert_eq!(server.finish().len(), expected, "{name}");
    }
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_sends_only_present_fields_and_validates_the_returned_record() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            tag("tag-new", "prod", Some("#e11d48")).to_string(),
        ),
        ("200 OK", tag("tag-plain", "plain", None).to_string()),
    ]);
    let client = client(&server);

    let colored = client
        .tags()
        .create(CreateTag::new("prod", Some("#e11d48".to_owned())))
        .await
        .expect("tag is created");
    let plain = client
        .tags()
        .create(CreateTag::new("plain", None))
        .await
        .expect("a colorless tag is created");

    assert_eq!(colored.tag_id().as_str(), "tag-new");
    assert_eq!(plain.tag_id().as_str(), "tag-plain");
    let requests = server.finish();
    assert_post(
        &requests[0],
        "tag.create",
        serde_json::json!({"name": "prod", "color": "#e11d48"}),
    );
    assert_post(
        &requests[1],
        "tag.create",
        serde_json::json!({"name": "plain"}),
    );
}

#[tokio::test]
async fn create_rejects_a_record_that_does_not_echo_the_request() {
    // A well-formed record for another tag is a contradiction; a body that is not a
    // tag record at all leaves the accepted create's outcome unknown, so the
    // executor recovers by diffing the collection instead of guessing an identity.
    for (name, response, unknown) in [
        (
            "another name",
            tag("tag-1", "other", None).to_string(),
            false,
        ),
        (
            "another color",
            tag("tag-1", "prod", Some("#000000")).to_string(),
            false,
        ),
        (
            "an array",
            serde_json::json!([tag("tag-1", "prod", None)]).to_string(),
            true,
        ),
        (
            "no identity",
            serde_json::json!({"name": "prod"}).to_string(),
            true,
        ),
    ] {
        let server = TestServer::respond_with_json(response);
        let error = client(&server)
            .tags()
            .create(CreateTag::new("prod", None))
            .await
            .expect_err("an unrecognized create response must not yield an identity");
        if unknown {
            assert!(
                matches!(error, Error::OutcomeUnknown { .. }),
                "{name}: {error}"
            );
        } else {
            assert!(
                matches!(error, Error::UnexpectedResponse { .. }),
                "{name}: {error}"
            );
        }
        server.finish();
    }
}

#[tokio::test]
async fn invalid_inputs_are_rejected_before_any_request() {
    let server = TestServer::untouched();
    let client = client(&server);

    for error in [
        client.tags().create(CreateTag::new("", None)).await.err(),
        client
            .tags()
            .create(CreateTag::new("prod", Some(String::new())))
            .await
            .err(),
        client
            .tags()
            .update(UpdateTag::new(TagId::new("tag-1")))
            .await
            .err(),
        client
            .tags()
            .update(UpdateTag::new(TagId::new("")).name("x"))
            .await
            .err(),
        client
            .tags()
            .update(UpdateTag::new(TagId::new("tag-1")).name(""))
            .await
            .err(),
    ] {
        let error = error.expect("invalid input must be rejected");
        assert!(matches!(error, Error::InvalidRequest { .. }), "{error}");
    }
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn update_writes_only_the_selected_fields() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "{}".to_owned()),
        ("200 OK", "{}".to_owned()),
        ("200 OK", "{}".to_owned()),
    ]);
    let client = client(&server);

    client
        .tags()
        .update(UpdateTag::new(TagId::new("tag-1")).name("renamed"))
        .await
        .unwrap();
    client
        .tags()
        .update(UpdateTag::new(TagId::new("tag-1")).color("#111111"))
        .await
        .unwrap();
    client
        .tags()
        .update(
            UpdateTag::new(TagId::new("tag-1"))
                .name("both")
                .color("#222222"),
        )
        .await
        .unwrap();

    let requests = server.finish();
    assert_post(
        &requests[0],
        "tag.update",
        serde_json::json!({"tagId": "tag-1", "name": "renamed"}),
    );
    assert_post(
        &requests[1],
        "tag.update",
        serde_json::json!({"tagId": "tag-1", "color": "#111111"}),
    );
    assert_post(
        &requests[2],
        "tag.update",
        serde_json::json!({"tagId": "tag-1", "name": "both", "color": "#222222"}),
    );
}

#[tokio::test]
async fn delete_and_project_association_use_the_exact_operations() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "{}".to_owned()),
        ("200 OK", "true".to_owned()),
        ("200 OK", "null".to_owned()),
    ]);
    let client = client(&server);

    client.tags().delete(TagId::new("tag-1")).await.unwrap();
    client
        .tags()
        .assign_to_project(ProjectId::new("project-1"), TagId::new("tag-1"))
        .await
        .unwrap();
    client
        .tags()
        .remove_from_project(ProjectId::new("project-1"), TagId::new("tag-1"))
        .await
        .unwrap();

    let requests = server.finish();
    assert_post(
        &requests[0],
        "tag.remove",
        serde_json::json!({"tagId": "tag-1"}),
    );
    assert_post(
        &requests[1],
        "tag.assignToProject",
        serde_json::json!({"projectId": "project-1", "tagId": "tag-1"}),
    );
    assert_post(
        &requests[2],
        "tag.removeFromProject",
        serde_json::json!({"projectId": "project-1", "tagId": "tag-1"}),
    );
}

#[tokio::test]
async fn failure_bodies_are_never_retained() {
    const CANARY: &str = "tag-error-body-canary-do-not-retain";
    let body = format!(r#"{{"message":"{CANARY}"}}"#);
    let server = TestServer::respond_in_sequence(vec![
        ("400 Bad Request", body.clone()),
        ("500 Internal Server Error", body.clone()),
        ("400 Bad Request", body.clone()),
        ("400 Bad Request", body),
    ]);
    let client = client(&server);

    let errors = [
        client.tags().all().await.expect_err("fails"),
        client
            .tags()
            .create(CreateTag::new("prod", None))
            .await
            .expect_err("fails"),
        client
            .tags()
            .update(UpdateTag::new(TagId::new("tag-1")).name("x"))
            .await
            .expect_err("fails"),
        client
            .tags()
            .delete(TagId::new("tag-1"))
            .await
            .expect_err("fails"),
    ];

    for error in errors {
        assert!(!format!("{error} {error:?}").contains(CANARY), "{error:?}");
    }
    server.finish();
}

// ---------------------------------------------------------------------------
// Project tag membership
// ---------------------------------------------------------------------------

fn project(tags: serde_json::Value) -> ProjectDetails {
    serde_json::from_value(serde_json::json!({
        "projectId": "project-1", "name": "platform", "environments": [],
        "projectTags": tags,
    }))
    .expect("project deserializes")
}

#[test]
fn project_tags_resolve_direct_and_nested_identities() {
    let direct = project(serde_json::json!([{"projectId": "project-1", "tagId": "tag-1"}]));
    let nested = project(serde_json::json!([
        {"projectId": "project-1", "tag": {"tagId": "tag-2", "name": "prod"}},
        {"tagId": "tag-3", "tag": {"tagId": "tag-3"}},
    ]));

    let ids = |project: &ProjectDetails| match &project.tags {
        ResponseField::Value(tags) => tags
            .iter()
            .map(|tag| tag.tag_id.as_str().to_owned())
            .collect::<Vec<_>>(),
        other => panic!("expected tags, got {other:?}"),
    };
    assert_eq!(ids(&direct), ["tag-1"]);
    assert_eq!(ids(&nested), ["tag-2", "tag-3"]);
}

#[test]
fn project_tags_keep_empty_absent_and_null_apart() {
    assert_eq!(
        project(serde_json::json!([])).tags,
        ResponseField::Value(Vec::new())
    );
    assert_eq!(project(serde_json::Value::Null).tags, ResponseField::Null);
    let absent: ProjectDetails = serde_json::from_value(serde_json::json!({
        "projectId": "project-1", "name": "platform",
    }))
    .unwrap();
    assert_eq!(absent.tags, ResponseField::NotReturned);
}

#[test]
fn project_tags_fail_closed_on_unusable_records() {
    for tags in [
        serde_json::json!([{"projectId": "project-1"}]),
        serde_json::json!([{"tagId": "tag-1", "tag": {"tagId": "tag-2"}}]),
        serde_json::json!(["tag-1"]),
    ] {
        serde_json::from_value::<ProjectDetails>(serde_json::json!({
            "projectId": "project-1", "name": "platform", "projectTags": tags,
        }))
        .expect_err("an unusable project tag record must not deserialize");
    }
}

#[test]
fn tag_details_ignore_unknown_fields() {
    let details: TagDetails = serde_json::from_value(tag("tag-1", "prod", None)).unwrap();

    assert_eq!(details.tag_id.as_str(), "tag-1");
    assert_eq!(details.color, ResponseField::Null);
}
