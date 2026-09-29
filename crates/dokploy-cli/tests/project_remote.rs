use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{DiscoverProjectsError, ProjectTopologyAuthority, discover_projects};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, PlanDiagnosticCode, PropertyPath, PropertyUnknownReason,
    RemoteFailureKind, RemoteObservation, StoredState, plan,
};
use dokploy_sdk::Dokploy;
use dokploy_state::{InstanceIdentity, ManagedInputs, RemoteId, ResourceState, StateFile};

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
                .expect("test receives request");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn state(&self) -> StateFile {
        let instance = InstanceIdentity::parse(&self.url).expect("server URL is an instance");
        StateFile::new("0.1.0".parse().expect("version is valid"), instance)
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("test server exits cleanly");
        requests
    }
}

fn request_is_complete(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\r\n\r\n")
}

fn compile(yaml: &str) -> dokploy_cli::desired::CompiledDesired {
    let config = DokployConfig::parse(yaml).expect("configuration is valid");
    let digest = ConfigDigest::parse("a".repeat(64)).expect("digest is valid");
    compile_desired(&config, digest).expect("configuration compiles")
}

fn insert_project(
    state: &mut StateFile,
    address: &str,
    remote_id: &str,
    last_applied: serde_json::Value,
) {
    let address = address.parse().expect("address is valid");
    state
        .upsert_resource(
            address,
            ResourceState::new(
                dokploy_state::ResourceKind::Project,
                RemoteId::new(remote_id).expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(last_applied).expect("managed inputs are valid"),
                Vec::new(),
            ),
        )
        .expect("state accepts project");
}

#[tokio::test]
async fn authoritative_project_topology_proves_an_unmanaged_project_is_missing() {
    let server = TestServer::respond_in_sequence(vec![("200 OK", "[]")]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("authoritative topology is projected");

    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /api/project.all HTTP/1.1\r\n"));
}

#[tokio::test]
async fn partial_project_topology_cannot_prove_an_unmanaged_project_is_missing() {
    let server = TestServer::respond_in_sequence(vec![("200 OK", "[]")]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let remote = discover_projects(&client, &desired, &state, ProjectTopologyAuthority::Partial)
        .await
        .expect("partial topology is projected conservatively");

    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn managed_project_identity_is_matched_by_remote_id_not_logical_name() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[{"projectId":"project-1","name":"renamed-remotely","description":"Stable","environments":[]}]"#,
    )]);
    let client = server.client();
    let mut state = server.state();
    insert_project(
        &mut state,
        "project.platform",
        "project-1",
        serde_json::json!({"description": "Stable"}),
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
  description: Stable
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("managed project is projected");
    let observation = remote
        .observation(&"project.platform".parse().unwrap())
        .expect("project was observed");
    let RemoteObservation::Present(project) = observation else {
        panic!("managed project should be present");
    };

    assert_eq!(project.remote_id().as_str(), "project-1");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().is_empty());
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn reused_managed_name_with_a_new_remote_id_is_never_adopted() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[{"projectId":"replacement-2","name":"platform","environments":[]}]"#,
    )]);
    let client = server.client();
    let mut state = server.state();
    insert_project(
        &mut state,
        "project.platform",
        "original-1",
        serde_json::json!({}),
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("same-name replacement is projected as a collision probe");
    let Some(RemoteObservation::Present(project)) =
        remote.observation(&"project.platform".parse().unwrap())
    else {
        panic!("same-name replacement should be visible to the planner");
    };
    assert_eq!(project.remote_id().as_str(), "replacement-2");

    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.changes().is_empty());
    assert_eq!(plan.diagnostics().len(), 1);
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteIdentityMismatch
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn compiler_remote_and_planner_select_an_in_place_project_update() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[{"projectId":"project-1","name":"platform","description":"Old","environments":[]}]"#,
    )]);
    let client = server.client();
    let mut state = server.state();
    insert_project(
        &mut state,
        "project.platform",
        "project-1",
        serde_json::json!({"description": "Old"}),
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
  description: New
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("managed project is projected");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(plan.diagnostics().is_empty());
    assert_eq!(plan.changes().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(plan.changes()[0].address().to_string(), "project.platform");
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn unmanaged_project_name_collision_is_projected_without_unrequested_properties() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[{"projectId":"project-9","name":"platform","description":"Remote only","environments":[]}]"#,
    )]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("collision is projected");
    let Some(RemoteObservation::Present(project)) =
        remote.observation(&"project.platform".parse().unwrap())
    else {
        panic!("name collision should be present");
    };

    assert_eq!(project.remote_id().as_str(), "project-9");
    assert_eq!(project.property(&PropertyPath::Description), None);
    let stored = StoredState::try_from_state(&state).expect("empty state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert_eq!(plan.diagnostics().len(), 1);
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::UnmanagedAddressCollision
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn pending_move_observes_the_stored_source_by_id_and_target_by_name() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[{"projectId":"project-1","name":"legacy-remote-name","description":"Stable","environments":[]}]"#,
    )]);
    let client = server.client();
    let mut state = server.state();
    insert_project(
        &mut state,
        "project.legacy",
        "project-1",
        serde_json::json!({"description": "Stable"}),
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
  description: Stable
moves:
  - from: project.legacy
    to: project.platform
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("move probes are projected");

    let Some(RemoteObservation::Present(source)) =
        remote.observation(&"project.legacy".parse().unwrap())
    else {
        panic!("move source should be present");
    };
    assert_eq!(source.remote_id().as_str(), "project-1");
    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn retained_stored_project_is_observed_by_remote_id() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[{"projectId":"legacy-1","name":"renamed-legacy","description":null,"environments":[]}]"#,
    )]);
    let client = server.client();
    let mut state = server.state();
    insert_project(
        &mut state,
        "project.legacy",
        "legacy-1",
        serde_json::json!({}),
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
removed:
  - from: project.legacy
    destroy: false
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("retained project is projected");

    let Some(RemoteObservation::Present(project)) =
        remote.observation(&"project.legacy".parse().unwrap())
    else {
        panic!("retained stored project should be present");
    };
    assert_eq!(project.remote_id().as_str(), "legacy-1");
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn destroyed_stored_project_can_be_proven_missing() {
    let server = TestServer::respond_in_sequence(vec![("200 OK", "[]")]);
    let client = server.client();
    let mut state = server.state();
    insert_project(
        &mut state,
        "project.legacy",
        "legacy-1",
        serde_json::json!({}),
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
removed:
  - from: project.legacy
    destroy: true
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("missing destroyed project is projected");

    assert!(matches!(
        remote.observation(&"project.legacy".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn duplicate_remote_project_names_are_rejected_as_ambiguous_topology() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[
          {"projectId":"project-1","name":"platform","environments":[]},
          {"projectId":"project-2","name":"platform","environments":[]}
        ]"#,
    )]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let error = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect_err("duplicate names are ambiguous");

    assert!(matches!(error, DiscoverProjectsError::DuplicateProjectName));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn duplicate_remote_project_ids_are_rejected_as_ambiguous_topology() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[
          {"projectId":"project-1","name":"platform","environments":[]},
          {"projectId":"project-1","name":"other","environments":[]}
        ]"#,
    )]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let error = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect_err("duplicate remote IDs are ambiguous");

    assert!(matches!(error, DiscoverProjectsError::DuplicateProjectId));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn authentication_failure_marks_each_relevant_project_unauthorized() {
    let server = TestServer::respond_in_sequence(vec![(
        "401 Unauthorized",
        r#"{"message":"Unauthorized"}"#,
    )]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("authentication failure is a closed observation");

    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unauthorized
        ))
    ));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn transport_failure_marks_each_relevant_project_unavailable() {
    let server = TestServer::close_after_request();
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("transport failure is a closed observation");

    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unavailable
        ))
    ));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn exhausted_server_failure_marks_each_relevant_project_unavailable() {
    let body = r#"{"code":"SERVICE_UNAVAILABLE","message":"Try again"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("503 Service Unavailable", body),
        ("503 Service Unavailable", body),
        ("503 Service Unavailable", body),
    ]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("server failure is a closed observation");

    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unavailable
        ))
    ));
    assert_eq!(server.finish().len(), 3, "the SDK owns bounded GET retries");
}

#[tokio::test]
async fn exhausted_throttling_marks_each_relevant_project_unavailable() {
    let body = r#"{"code":"TOO_MANY_REQUESTS","message":"Try again"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("429 Too Many Requests", body),
        ("429 Too Many Requests", body),
        ("429 Too Many Requests", body),
    ]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("throttling is a closed observation");

    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unavailable
        ))
    ));
    assert_eq!(server.finish().len(), 3, "the SDK owns bounded GET retries");
}

#[tokio::test]
async fn decode_failure_marks_each_relevant_project_invalid_response() {
    let server = TestServer::respond_in_sequence(vec![("200 OK", "not-json")]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("decode failure is a closed observation");

    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn unsupported_resource_kinds_are_rejected_before_remote_transport() {
    let url = "http://127.0.0.1:9";
    let client = Dokploy::builder()
        .url(url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    let state = StateFile::new(
        "0.1.0".parse().expect("version is valid"),
        InstanceIdentity::parse(url).expect("instance is valid"),
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    let error = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect_err("project discovery cannot project environment addresses");

    assert!(matches!(
        error,
        DiscoverProjectsError::UnsupportedResourceKind { .. }
    ));
}

#[tokio::test]
async fn client_and_state_instance_mismatch_is_rejected_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");
    let state = StateFile::new(
        "0.1.0".parse().expect("version is valid"),
        InstanceIdentity::parse("https://other.example.com").expect("instance is valid"),
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    let error = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect_err("mismatched client and state instances are unsafe");
    let rendered = format!("{error} {error:?}");

    assert_eq!(
        error.to_string(),
        "DOKREM001: client and state instances differ"
    );
    assert!(matches!(&error, DiscoverProjectsError::InstanceMismatch));
    assert!(!rendered.contains("127.0.0.1"));
    assert!(!rendered.contains("other.example.com"));
}

#[tokio::test]
async fn every_discovery_invocation_performs_a_fresh_project_read() {
    let server = TestServer::respond_in_sequence(vec![("200 OK", "[]"), ("200 OK", "[]")]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
"#,
    );

    for _ in 0..2 {
        discover_projects(
            &client,
            &desired,
            &state,
            ProjectTopologyAuthority::Authoritative,
        )
        .await
        .expect("fresh project topology is projected");
    }

    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.starts_with("GET /api/project.all HTTP/1.1\r\n"))
    );
}

#[tokio::test]
async fn requested_absent_description_is_observed_as_known_absent() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[{"projectId":"project-1","name":"platform","description":null,"environments":[]}]"#,
    )]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
  description: null
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("project is projected");
    let Some(RemoteObservation::Present(project)) =
        remote.observation(&"project.platform".parse().unwrap())
    else {
        panic!("project should be present");
    };

    assert!(matches!(
        project.property(&PropertyPath::Description),
        Some(dokploy_core::PropertyObservation::KnownAbsent)
    ));
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn requested_omitted_description_is_observed_as_not_returned() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
    )]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
  description: Expected
"#,
    );

    let remote = discover_projects(
        &client,
        &desired,
        &state,
        ProjectTopologyAuthority::Authoritative,
    )
    .await
    .expect("project is projected");
    let Some(RemoteObservation::Present(project)) =
        remote.observation(&"project.platform".parse().unwrap())
    else {
        panic!("project should be present");
    };

    assert!(matches!(
        project.property(&PropertyPath::Description),
        Some(dokploy_core::PropertyObservation::Unknown(
            PropertyUnknownReason::NotReturned
        ))
    ));
    assert_eq!(server.finish().len(), 1);
}
