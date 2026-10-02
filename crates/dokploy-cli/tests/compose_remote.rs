use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::{compile_desired, compile_desired_for_instance};
use dokploy_cli::remote::{
    ComposeTopologyAuthority, DiscoverRemoteError, DiscoveryAuthority, discover_remote,
};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, PropertyObservation, PropertyPath, RemoteFailureKind,
    RemoteObservation, StoredState, plan,
};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateFile,
};

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
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
                    if count == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
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
                .expect("test receives requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn client(&self) -> Dokploy {
        Dokploy::builder()
            .url(&self.url)
            .api_key("test-api-key")
            .build()
            .expect("client configuration is valid")
    }

    fn state(&self) -> StateFile {
        StateFile::new(
            "0.1.0".parse().expect("version is valid"),
            InstanceIdentity::parse(&self.url).expect("server URL is an instance"),
        )
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("test server exits cleanly");
        requests
    }
}

fn insert(
    state: &mut StateFile,
    address: &str,
    kind: ResourceKind,
    remote_id: &str,
    containment: Option<&str>,
    managed_inputs: serde_json::Value,
) {
    state
        .upsert_resource(
            address.parse().expect("address is valid"),
            ResourceState::new(
                kind,
                RemoteId::new(remote_id).expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(managed_inputs).expect("managed inputs are valid"),
                containment.map(|value| value.parse::<ResourceAddress>().unwrap()),
                Vec::<ResourceAddress>::new(),
            ),
        )
        .expect("state accepts resource");
}

fn base_state(server: &TestServer) -> StateFile {
    let mut state = server.state();
    insert(
        &mut state,
        "project.platform",
        ResourceKind::Project,
        "project-1",
        None,
        serde_json::json!({}),
    );
    insert(
        &mut state,
        "environment.production",
        ResourceKind::Environment,
        "environment-1",
        Some("project.platform"),
        serde_json::json!({}),
    );
    state
}

fn protected_config(description: &str) -> DokployConfig {
    DokployConfig::parse(&format!(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      compose:\n        web:\n          description: {description:?}\n          lifecycle:\n            protect: true\n"
    ))
    .expect("protected Compose configuration is valid")
}

#[tokio::test]
async fn authoritative_absence_allows_compose_create_with_an_opaque_document() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ]);
    let state = base_state(&server);
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    fs::write(
        workspace.path().join("compose.yaml"),
        "services:\n  web:\n    image: private-canary\n",
    )
    .expect("Compose document is writable");
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
  environments:
    production:
      compose:
        web:
          description: Web stack
          document:
            file: compose.yaml
"#,
    )
    .expect("configuration is valid");
    let desired = compile_desired_for_instance(
        &config,
        ConfigDigest::parse("a".repeat(64)).expect("digest is valid"),
        InstanceIdentity::parse(&server.url).expect("server URL is an instance"),
        workspace.path(),
    )
    .expect("configuration compiles");

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect("Compose state is projected");

    assert!(matches!(
        remote.observation(&"compose.web".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.applyable());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Create && change.address().to_string() == "compose.web"
    }));
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].starts_with("GET /api/compose.search?"));
}

#[tokio::test]
async fn managed_compose_requires_direct_and_collection_agreement() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"web","appName":"web-app","description":"Web stack","sourceType":"raw"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"composeId":"compose-1","environmentId":"environment-1","name":"web","appName":"web-app","description":"Web stack","sourceType":"raw","composeType":"docker-compose","autoDeploy":false,"composePath":"./compose.yaml","composeStatus":"idle"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "compose.web",
        ResourceKind::Compose,
        "compose-1",
        Some("environment.production"),
        serde_json::json!({"description":"Web stack"}),
    );
    let config = protected_config("Web stack");
    let desired = compile_desired(&config, ConfigDigest::parse("b".repeat(64)).unwrap()).unwrap();

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect("Compose state is projected");

    let Some(RemoteObservation::Present(resource)) =
        remote.observation(&"compose.web".parse().unwrap())
    else {
        panic!("Compose record should be present");
    };
    assert_eq!(
        resource.property(&PropertyPath::Description),
        Some(&PropertyObservation::Known(
            dokploy_core::ComparableValue::try_from_json(serde_json::json!("Web stack")).unwrap()
        ))
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert!(requests[4].starts_with("GET /api/compose.one?"));
}

#[tokio::test]
async fn opaque_document_ownership_fails_closed_for_a_non_raw_compose_source() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"web","appName":"web-app","description":"Web stack","sourceType":"github"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"composeId":"compose-1","environmentId":"environment-1","name":"web","appName":"web-app","description":"Web stack","sourceType":"github","composeType":"docker-compose","autoDeploy":false,"composePath":"./docker-compose.yml","composeStatus":"idle"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "compose.web",
        ResourceKind::Compose,
        "compose-1",
        Some("environment.production"),
        serde_json::json!({"description":"Web stack"}),
    );
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    fs::write(
        workspace.path().join("compose.yaml"),
        "services:\n  web:\n    image: private-canary\n",
    )
    .expect("Compose document is writable");
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
  environments:
    production:
      compose:
        web:
          description: Web stack
          document: { file: compose.yaml }
"#,
    )
    .expect("configuration is valid");
    let desired = compile_desired_for_instance(
        &config,
        ConfigDigest::parse("e".repeat(64)).unwrap(),
        InstanceIdentity::parse(&server.url).unwrap(),
        workspace.path(),
    )
    .expect("configuration compiles");

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect("topology reads agree");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    let _ = server.finish();
}

#[tokio::test]
async fn duplicate_compose_names_fail_closed() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"web"},{"composeId":"compose-2","environmentId":"environment-1","name":"web"}],"total":2}"#,
        ),
    ]);
    let state = base_state(&server);
    let config = protected_config("Web stack");
    let desired = compile_desired(&config, ConfigDigest::parse("c".repeat(64)).unwrap()).unwrap();

    let error = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("duplicate Compose names must fail closed");

    assert!(matches!(error, DiscoverRemoteError::DuplicateComposeName));
    let _ = server.finish();
}

#[tokio::test]
async fn managed_compose_is_unavailable_when_collection_fails_despite_direct_success() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        ("500 Internal Server Error", r#"{"message":"unavailable"}"#),
        (
            "200 OK",
            r#"{"composeId":"compose-1","environmentId":"environment-1","name":"web","appName":"web-app","description":"Web stack","sourceType":"raw"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "compose.web",
        ResourceKind::Compose,
        "compose-1",
        Some("environment.production"),
        serde_json::json!({"description":"Web stack"}),
    );
    let config = protected_config("Changed stack");
    let desired = compile_desired(&config, ConfigDigest::parse("d".repeat(64)).unwrap()).unwrap();

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority {
            compose: ComposeTopologyAuthority::Authoritative,
            ..DiscoveryAuthority::reconciliation()
        },
    )
    .await
    .expect("collection failure is represented without trusting the direct read");

    assert!(matches!(
        remote.observation(&"compose.web".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unavailable
        ))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    let _ = server.finish();
}
