use std::io::{Read, Write};
use std::net::TcpListener;
use std::num::NonZeroU16;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ApplicationId, CreatePort, Dokploy, Error, PortDetails, PortId, PortProtocol, PublishMode,
    UpdatePort,
};

const PORT_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/port-one.created.owner.json");
const PORT_RESPONSE: &str = r#"{
    "portId":"port-1",
    "publishedPort":8080,
    "targetPort":80,
    "publishMode":"ingress",
    "protocol":"tcp",
    "applicationId":"application-1",
    "futureRuntimeField":{"nested":true}
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

fn port(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).expect("test Port is nonzero")
}

fn create_port() -> CreatePort {
    CreatePort::new(
        ApplicationId::new("application-1"),
        port(8080),
        port(80),
        PublishMode::Ingress,
        PortProtocol::Tcp,
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
    assert!(!body.contains(".0"), "port numbers must be JSON integers");
}

#[test]
fn port_input_serializes_nonzero_u16_values_as_json_integers() {
    let value = serde_json::to_value(create_port()).expect("Port input serializes");

    assert_eq!(
        value,
        serde_json::json!({
            "applicationId": "application-1",
            "publishedPort": 8080,
            "targetPort": 80,
            "publishMode": "ingress",
            "protocol": "tcp"
        })
    );
    assert!(NonZeroU16::new(0).is_none());
}

#[test]
fn port_response_is_safe_tolerant_and_rejects_invalid_port_numbers() {
    let details: PortDetails = serde_json::from_str(PORT_FIXTURE).expect("fixture deserializes");

    assert_eq!(details.port_id.as_str(), "port-1");
    assert_eq!(details.application_id.as_str(), "application-1");
    assert_eq!(details.published_port.get(), 18080);
    assert_eq!(details.target_port.get(), 8080);

    let zero = PORT_RESPONSE.replace("\"targetPort\":80", "\"targetPort\":0");
    serde_json::from_str::<PortDetails>(&zero).expect_err("zero target Port must fail closed");
    let overflow = PORT_RESPONSE.replace("\"publishedPort\":8080", "\"publishedPort\":65536");
    serde_json::from_str::<PortDetails>(&overflow)
        .expect_err("out-of-range published Port must fail closed");
}

#[tokio::test]
async fn port_get_uses_the_exact_operation_and_validates_identity() {
    let server = TestServer::respond_with_json(PORT_RESPONSE);
    let details = client(&server)
        .ports()
        .get(PortId::new("port-1"))
        .await
        .expect("Port is readable");

    assert_eq!(details.application_id.as_str(), "application-1");
    assert!(
        server
            .finish()
            .starts_with("GET /api/port.one?portId=port-1 HTTP/1.1\r\n")
    );

    for body in [
        PORT_RESPONSE.replace("port-1", "port-2"),
        PORT_RESPONSE.replace("application-1", ""),
    ] {
        let server = TestServer::respond_with_json(Box::leak(body.into_boxed_str()));
        let error = client(&server)
            .ports()
            .get(PortId::new("port-1"))
            .await
            .expect_err("contradictory Port identity must fail closed");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "port.one"
            }
        ));
        server.finish();
    }
}

#[tokio::test]
async fn application_one_is_the_authoritative_bounded_port_collection() {
    let response: &'static str = Box::leak(
        format!(
            r#"{{"applicationId":"application-1","ports":[{PORT_RESPONSE}],"env":"secret-canary","future":true}}"#
        )
        .into_boxed_str(),
    );
    let server = TestServer::respond_with_json(response);
    let collection = client(&server)
        .ports()
        .by_application(ApplicationId::new("application-1"))
        .await
        .expect("application Port collection is readable");

    assert_eq!(collection.application_id().as_str(), "application-1");
    assert_eq!(collection.ports().len(), 1);
    assert!(!format!("{collection:?}").contains("secret-canary"));
    assert!(
        server
            .finish()
            .starts_with("GET /api/application.one?applicationId=application-1 HTTP/1.1\r\n")
    );

    let duplicate =
        format!(r#"{{"applicationId":"application-1","ports":[{PORT_RESPONSE},{PORT_RESPONSE}]}}"#);
    let wrong_parent = format!(
        r#"{{"applicationId":"application-1","ports":[{}]}}"#,
        PORT_RESPONSE.replace("application-1", "application-2")
    );
    let wrong_root = format!(r#"{{"applicationId":"application-2","ports":[{PORT_RESPONSE}]}}"#);
    let too_many = (0..10_001)
        .map(|index| PORT_RESPONSE.replace("port-1", &format!("port-{index}")))
        .collect::<Vec<_>>()
        .join(",");
    let too_many = format!(r#"{{"applicationId":"application-1","ports":[{too_many}]}}"#);

    for body in [duplicate, wrong_parent, wrong_root, too_many] {
        let server = TestServer::respond_with_json(Box::leak(body.into_boxed_str()));
        let error = client(&server)
            .ports()
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

    let server = TestServer::respond_with_json(r#"{"applicationId":"application-1"}"#);
    let error = client(&server)
        .ports()
        .by_application(ApplicationId::new("application-1"))
        .await
        .expect_err("an omitted authoritative Port relation must fail closed");
    assert!(matches!(
        error,
        Error::Decode {
            operation: "application.one",
            ..
        }
    ));
    server.finish();
}

#[tokio::test]
async fn port_create_validates_the_complete_returned_identity() {
    let server = TestServer::respond_with_json(PORT_RESPONSE);
    let created = client(&server)
        .ports()
        .create(create_port())
        .await
        .expect("Port creation succeeds");

    assert_eq!(created.port_id().as_str(), "port-1");
    assert_mutation_request(
        &server.finish(),
        "port.create",
        serde_json::json!({
            "applicationId": "application-1",
            "publishedPort": 8080,
            "targetPort": 80,
            "publishMode": "ingress",
            "protocol": "tcp"
        }),
    );

    let mismatch = TestServer::respond_with_json(Box::leak(
        PORT_RESPONSE
            .replace("\"targetPort\":80", "\"targetPort\":81")
            .into_boxed_str(),
    ));
    let error = client(&mismatch)
        .ports()
        .create(create_port())
        .await
        .expect_err("conflicting returned fields must fail closed");
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "port.create"
        }
    ));
    mismatch.finish();
}

#[tokio::test]
async fn port_update_and_delete_use_exact_complete_requests() {
    let update_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    client(&update_server)
        .ports()
        .update(UpdatePort::new(
            PortId::new("port-1"),
            port(9090),
            port(90),
            PublishMode::Host,
            PortProtocol::Udp,
        ))
        .await
        .expect("Port update succeeds");
    assert_mutation_request(
        &update_server.finish(),
        "port.update",
        serde_json::json!({
            "portId": "port-1",
            "publishedPort": 9090,
            "targetPort": 90,
            "publishMode": "host",
            "protocol": "udp"
        }),
    );

    let delete_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    client(&delete_server)
        .ports()
        .delete(PortId::new("port-1"))
        .await
        .expect("Port deletion succeeds");
    assert_mutation_request(
        &delete_server.finish(),
        "port.delete",
        serde_json::json!({"portId": "port-1"}),
    );
}

#[tokio::test]
async fn port_rejects_empty_identities_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let errors = [
        client
            .ports()
            .get(PortId::new(""))
            .await
            .expect_err("an empty Port ID is invalid"),
        client
            .ports()
            .by_application(ApplicationId::new(""))
            .await
            .expect_err("an empty application ID is invalid"),
        client
            .ports()
            .create(CreatePort::new(
                ApplicationId::new(""),
                port(8080),
                port(80),
                PublishMode::Ingress,
                PortProtocol::Tcp,
            ))
            .await
            .expect_err("an empty application ID is invalid"),
        client
            .ports()
            .update(UpdatePort::new(
                PortId::new(""),
                port(8080),
                port(80),
                PublishMode::Ingress,
                PortProtocol::Tcp,
            ))
            .await
            .expect_err("an empty Port ID is invalid"),
        client
            .ports()
            .delete(PortId::new(""))
            .await
            .expect_err("an empty Port ID is invalid"),
    ];

    assert!(errors.iter().all(|error| matches!(
        error,
        Error::InvalidRequest { operation, .. }
            if operation == &"port.one"
                || operation == &"application.one"
                || operation.starts_with("port.")
    )));
}

#[tokio::test]
async fn port_mutations_are_single_attempt_and_report_unknown_outcomes() {
    let create_server = TestServer::close_after_request();
    let create_error = client(&create_server)
        .ports()
        .create(create_port())
        .await
        .expect_err("missing create response has an unknown outcome");
    assert!(matches!(
        create_error,
        Error::OutcomeUnknown {
            operation: "port.create",
            ..
        }
    ));
    assert_eq!(create_server.finish_all().len(), 1);

    let update_server = TestServer::close_after_request();
    let update_error = client(&update_server)
        .ports()
        .update(UpdatePort::new(
            PortId::new("port-1"),
            port(9090),
            port(90),
            PublishMode::Host,
            PortProtocol::Udp,
        ))
        .await
        .expect_err("missing update response has an unknown outcome");
    assert!(matches!(
        update_error,
        Error::OutcomeUnknown {
            operation: "port.update",
            ..
        }
    ));
    assert_eq!(update_server.finish_all().len(), 1);

    let delete_server = TestServer::close_after_request();
    let delete_error = client(&delete_server)
        .ports()
        .delete(PortId::new("port-1"))
        .await
        .expect_err("missing delete response has an unknown outcome");
    assert!(matches!(
        delete_error,
        Error::OutcomeUnknown {
            operation: "port.delete",
            ..
        }
    ));
    assert_eq!(delete_server.finish_all().len(), 1);
}
