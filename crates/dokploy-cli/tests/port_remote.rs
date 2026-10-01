use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{DiscoverRemoteError, DiscoveryAuthority, discover_remote};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, PropertyObservation, PropertyPath, RemoteObservation, StoredState,
    plan,
};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateFile,
};

const PROJECTS: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
const APPLICATIONS: &str = r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#;
const APPLICATION: &str = r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api"}"#;
const PORT: &str = r#"{"portId":"port-1","applicationId":"application-1","publishedPort":8080,"targetPort":80,"publishMode":"ingress","protocol":"tcp"}"#;
const PORTS: &str = r#"{"applicationId":"application-1","ports":[{"portId":"port-1","applicationId":"application-1","publishedPort":8080,"targetPort":80,"publishMode":"ingress","protocol":"tcp"}]}"#;
const EMPTY_PORTS: &str = r#"{"applicationId":"application-1","ports":[]}"#;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_in_sequence(responses: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut captured = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..count]);
                    if count == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                captured.push(String::from_utf8(request).unwrap());
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            sender.send(captured).unwrap();
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
            .unwrap()
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().unwrap();
        self.thread.join().unwrap();
        requests
    }
}

fn base_state(server: &TestServer, include_port: bool) -> StateFile {
    let mut state = StateFile::new(
        "0.1.0".parse().unwrap(),
        InstanceIdentity::parse(&server.url).unwrap(),
    );
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
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        Some("environment.production"),
        serde_json::json!({}),
    );
    if include_port {
        insert(
            &mut state,
            "port.http",
            ResourceKind::Port,
            "port-1",
            Some("application.api"),
            serde_json::json!({
                "published_port": 8080,
                "target_port": 80,
                "publish_mode": "ingress",
                "protocol": "tcp"
            }),
        );
    }
    state
}

fn insert(
    state: &mut StateFile,
    address: &str,
    kind: ResourceKind,
    remote_id: &str,
    containment: Option<&str>,
    inputs: serde_json::Value,
) {
    state
        .upsert_resource(
            address.parse().unwrap(),
            ResourceState::new(
                kind,
                RemoteId::new(remote_id).unwrap(),
                false,
                ManagedInputs::try_from_json(inputs).unwrap(),
                containment.map(|value| value.parse::<ResourceAddress>().unwrap()),
                Vec::new(),
            ),
        )
        .unwrap();
}

fn desired(target_port: u16) -> dokploy_cli::desired::CompiledDesired {
    compile_desired(
        &DokployConfig::parse(&format!(
            r#"
version: 1
project: {{ name: platform }}
environments:
  production:
    applications:
      api:
        ports:
          http:
            published_port: 8080
            target_port: {target_port}
            publish_mode: ingress
            protocol: tcp
"#
        ))
        .unwrap(),
        ConfigDigest::parse("a".repeat(64)).unwrap(),
    )
    .unwrap()
}

fn discovery_responses(port_collection: &'static str) -> Vec<(&'static str, &'static str)> {
    vec![
        ("200 OK", PROJECTS),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", APPLICATIONS),
        ("200 OK", APPLICATION),
        ("200 OK", port_collection),
    ]
}

#[tokio::test]
async fn authoritative_parent_absence_plans_port_creation() {
    let server = TestServer::respond_in_sequence(discovery_responses(EMPTY_PORTS));
    let state = base_state(&server, false);
    let desired = desired(80);

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    let observation = remote.observation(&"port.http".parse().unwrap());
    assert!(
        matches!(observation, Some(RemoteObservation::Missing)),
        "{observation:?}; parent={:?}",
        remote.observation(&"application.api".parse().unwrap())
    );
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
    assert_eq!(server.finish().len(), 6);
}

#[tokio::test]
async fn managed_port_requires_direct_and_parent_collection_agreement() {
    let mut responses = discovery_responses(PORTS);
    responses.push(("200 OK", PORT));
    let server = TestServer::respond_in_sequence(responses);
    let state = base_state(&server, true);
    let desired = desired(80);

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let Some(RemoteObservation::Present(port)) = remote.observation(&"port.http".parse().unwrap())
    else {
        panic!("Port should be present")
    };

    assert!(matches!(
        port.property(&PropertyPath::PublishedPort),
        Some(PropertyObservation::Known(_))
    ));
    assert!(matches!(
        port.property(&PropertyPath::Protocol),
        Some(PropertyObservation::Known(_))
    ));
    assert_eq!(server.finish().len(), 7);
}

#[tokio::test]
async fn port_one_400_proves_absence_only_with_authoritative_parent_absence() {
    let mut responses = discovery_responses(EMPTY_PORTS);
    responses.push(("400 Bad Request", r#"{"message":"not found"}"#));
    let server = TestServer::respond_in_sequence(responses);
    let state = base_state(&server, true);
    let desired = desired(80);

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        remote.observation(&"port.http".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    server.finish();
}

#[tokio::test]
async fn complete_field_change_plans_one_in_place_update() {
    let mut responses = discovery_responses(PORTS);
    responses.push(("200 OK", PORT));
    let server = TestServer::respond_in_sequence(responses);
    let state = base_state(&server, true);
    let desired = desired(8081);

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(plan.changes()[0].replacement_order(), None);
    server.finish();
}

#[tokio::test]
async fn duplicate_parent_collision_keys_fail_closed() {
    let duplicate = Box::leak(
        format!(
            r#"{{"applicationId":"application-1","ports":[{PORT},{}]}}"#,
            PORT.replace("port-1", "port-2")
        )
        .into_boxed_str(),
    );
    let server = TestServer::respond_in_sequence(discovery_responses(duplicate));
    let state = base_state(&server, false);
    let desired = desired(80);

    let error = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("duplicate collision key must fail closed");

    assert!(matches!(error, DiscoverRemoteError::DuplicatePortCollision));
    server.finish();
}
