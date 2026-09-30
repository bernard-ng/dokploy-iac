use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{Dokploy, Error};

const SECRET_CANARY: &str = "external-selector-secret-canary";
const SERVER_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/server-all.selectors.json");
const REGISTRY_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/registry-all.selectors.json");
const DESTINATION_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/destination-all.selectors.json");

struct TestServer {
    url: String,
    request: Receiver<String>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond(status: &'static str, body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, request) = mpsc::channel();
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test server accepts a request");
            let bytes = read_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response is writable");
            sender
                .send(String::from_utf8(bytes).expect("request is UTF-8"))
                .expect("test receives request");
        });

        Self {
            url: format!("http://{address}"),
            request,
            thread,
        }
    }

    fn finish(self) -> String {
        let request = self.request.recv().expect("test receives request");
        self.thread.join().expect("test server exits cleanly");
        request
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];

    loop {
        let count = stream.read(&mut buffer).expect("request is readable");
        bytes.extend_from_slice(&buffer[..count]);
        if count == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    bytes
}

fn client(server: &TestServer) -> Dokploy {
    Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid")
}

fn assert_safe_error(error: &Error) {
    let display = error.to_string();
    let debug = format!("{error:?}");
    assert!(!display.contains(SECRET_CANARY));
    assert!(!debug.contains(SECRET_CANARY));
    let dokploy = error.dokploy().expect("HTTP error remains structured");
    assert_eq!(dokploy.status(), 400);
    assert!(!dokploy.message().contains(SECRET_CANARY));
    assert!(dokploy.issues().is_empty());
}

#[tokio::test]
async fn captured_v0306_selector_fixtures_decode_through_public_services() {
    let server = TestServer::respond("200 OK", SERVER_FIXTURE.to_owned());
    assert!(
        client(&server)
            .servers()
            .all()
            .await
            .expect("server fixture decodes")
            .servers()
            .is_empty()
    );
    let _ = server.finish();

    let server = TestServer::respond("200 OK", REGISTRY_FIXTURE.to_owned());
    assert!(
        client(&server)
            .registries()
            .all()
            .await
            .expect("registry fixture decodes")
            .registries()
            .is_empty()
    );
    let _ = server.finish();

    let server = TestServer::respond("200 OK", DESTINATION_FIXTURE.to_owned());
    assert!(
        client(&server)
            .destinations()
            .all()
            .await
            .expect("destination fixture decodes")
            .destinations()
            .is_empty()
    );
    let _ = server.finish();
}

#[tokio::test]
async fn server_all_uses_exact_operation_and_exposes_only_safe_fields() {
    let body = format!(
        r#"[
            {{"serverId":"server-1","name":"shared","serverType":"deploy",
               "command":"{SECRET_CANARY}","metricsToken":"{SECRET_CANARY}"}},
            {{"serverId":"server-2","name":"shared","serverType":"build",
               "ipAddress":"192.0.2.1","username":"root"}}
        ]"#
    );
    let server = TestServer::respond("200 OK", body);
    let collection = client(&server)
        .servers()
        .all()
        .await
        .expect("server collection is readable");

    assert_eq!(collection.servers().len(), 2);
    assert_eq!(collection.servers()[0].server_id.as_str(), "server-1");
    assert_eq!(collection.servers()[0].name, "shared");
    assert_eq!(collection.servers()[0].server_type, "deploy");
    assert_eq!(collection.servers()[1].name, "shared");
    assert!(!format!("{collection:?}").contains(SECRET_CANARY));
    assert!(
        server
            .finish()
            .starts_with("GET /api/server.all HTTP/1.1\r\n")
    );
}

#[tokio::test]
async fn registry_all_uses_exact_operation_and_exposes_only_safe_fields() {
    let body = format!(
        r#"[
            {{"registryId":"registry-1","registryName":"shared",
               "username":"{SECRET_CANARY}","password":"{SECRET_CANARY}"}},
            {{"registryId":"registry-2","registryName":"shared",
               "registryUrl":"https://registry.example.test"}}
        ]"#
    );
    let server = TestServer::respond("200 OK", body);
    let collection = client(&server)
        .registries()
        .all()
        .await
        .expect("registry collection is readable");

    assert_eq!(collection.registries().len(), 2);
    assert_eq!(
        collection.registries()[0].registry_id.as_str(),
        "registry-1"
    );
    assert_eq!(collection.registries()[0].registry_name, "shared");
    assert_eq!(collection.registries()[1].registry_name, "shared");
    assert!(!format!("{collection:?}").contains(SECRET_CANARY));
    assert!(
        server
            .finish()
            .starts_with("GET /api/registry.all HTTP/1.1\r\n")
    );
}

#[tokio::test]
async fn destination_all_uses_exact_operation_and_exposes_only_safe_fields() {
    let body = format!(
        r#"[
            {{"destinationId":"destination-1","name":"shared",
               "accessKey":"{SECRET_CANARY}","secretAccessKey":"{SECRET_CANARY}"}},
            {{"destinationId":"destination-2","name":"shared","bucket":"private"}}
        ]"#
    );
    let server = TestServer::respond("200 OK", body);
    let collection = client(&server)
        .destinations()
        .all()
        .await
        .expect("destination collection is readable");

    assert_eq!(collection.destinations().len(), 2);
    assert_eq!(
        collection.destinations()[0].destination_id.as_str(),
        "destination-1"
    );
    assert_eq!(collection.destinations()[0].name, "shared");
    assert_eq!(collection.destinations()[1].name, "shared");
    assert!(!format!("{collection:?}").contains(SECRET_CANARY));
    assert!(
        server
            .finish()
            .starts_with("GET /api/destination.all HTTP/1.1\r\n")
    );
}

#[tokio::test]
async fn selector_collections_reject_duplicate_or_empty_identities() {
    let cases = [
        (
            "server.all",
            r#"[{"serverId":"same","name":"one","serverType":"deploy"},{"serverId":"same","name":"two","serverType":"build"}]"#,
        ),
        (
            "registry.all",
            r#"[{"registryId":"same","registryName":"one"},{"registryId":"same","registryName":"two"}]"#,
        ),
        ("destination.all", r#"[{"destinationId":"","name":"one"}]"#),
    ];

    for (operation, body) in cases {
        let server = TestServer::respond("200 OK", body.to_owned());
        let result = match operation {
            "server.all" => client(&server).servers().all().await.map(|_| ()),
            "registry.all" => client(&server).registries().all().await.map(|_| ()),
            "destination.all" => client(&server).destinations().all().await.map(|_| ()),
            _ => unreachable!(),
        };
        assert!(
            matches!(result, Err(Error::UnexpectedResponse { operation: actual }) if actual == operation)
        );
        let _ = server.finish();
    }
}

#[tokio::test]
async fn selector_collection_item_count_is_bounded() {
    let items = (0..=10_000)
        .map(|index| {
            serde_json::json!({
                "destinationId": format!("destination-{index}"),
                "name": "destination"
            })
        })
        .collect::<Vec<_>>();
    let server = TestServer::respond(
        "200 OK",
        serde_json::to_string(&items).expect("test collection serializes"),
    );
    let error = client(&server)
        .destinations()
        .all()
        .await
        .expect_err("oversized item collection fails closed");

    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "destination.all"
        }
    ));
    let _ = server.finish();
}

#[tokio::test]
async fn selector_non_success_bodies_never_enter_errors() {
    for operation in ["server.all", "registry.all", "destination.all"] {
        let body = format!(
            r#"{{"code":"BAD_REQUEST","message":"{SECRET_CANARY}","issues":[{{"message":"{SECRET_CANARY}"}}]}}"#
        );
        let server = TestServer::respond("400 Bad Request", body);
        let error = match operation {
            "server.all" => client(&server)
                .servers()
                .all()
                .await
                .expect_err("read fails"),
            "registry.all" => client(&server)
                .registries()
                .all()
                .await
                .expect_err("read fails"),
            "destination.all" => client(&server)
                .destinations()
                .all()
                .await
                .expect_err("read fails"),
            _ => unreachable!(),
        };
        assert_safe_error(&error);
        let _ = server.finish();
    }
}
