use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::{compile_desired, compile_desired_for_instance};
use dokploy_cli::remote::{
    DiscoverRemoteError, DiscoveryAuthority, LibSqlTopologyAuthority, discover_remote,
};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, PlanDiagnosticCode, PropertyObservation, PropertyPath,
    RemoteFailureKind, RemoteObservation, ReplacementOrder, StoredState, plan,
};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    FingerprintKeyId, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath, StateFile,
};

const PROJECTS: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
const EMPTY_LIBSQL: &str = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"libsql":[]}]}"#;
const POPULATED_LIBSQL: &str = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"libsql":[{"libsqlId":"libsql-1","name":"main","appName":"main-generated","description":"old"}]}]}"#;
const LIBSQL_ONE: &str = r#"{"libsqlId":"libsql-1","environmentId":"environment-1","name":"main","appName":"main-generated","dockerImage":"libsql","description":"old","databaseUser":"app","sqldNode":"primary","sqldPrimaryUrl":null}"#;

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

fn base_state(server: &TestServer) -> StateFile {
    let mut state = StateFile::new(
        "0.1.0".parse().unwrap(),
        InstanceIdentity::parse(&server.url).unwrap(),
    );
    insert(
        &mut state,
        "project.platform",
        ResourceKind::Project,
        "project-1",
        serde_json::json!({}),
        None,
        SensitiveInputs::default(),
    );
    insert(
        &mut state,
        "environment.production",
        ResourceKind::Environment,
        "environment-1",
        serde_json::json!({}),
        Some("project.platform"),
        SensitiveInputs::default(),
    );
    state
}

fn insert(
    state: &mut StateFile,
    address: &str,
    kind: ResourceKind,
    remote_id: &str,
    inputs: serde_json::Value,
    containment: Option<&str>,
    sensitive: SensitiveInputs,
) {
    state
        .upsert_resource(
            address.parse().unwrap(),
            ResourceState::try_new(
                kind,
                RemoteId::new(remote_id).unwrap(),
                false,
                ManagedInputs::try_from_json(inputs).unwrap(),
                sensitive,
                containment.map(|value| value.parse::<ResourceAddress>().unwrap()),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
}

fn compile(yaml: &str) -> dokploy_cli::desired::CompiledDesired {
    compile_desired(
        &DokployConfig::parse(yaml).unwrap(),
        ConfigDigest::parse("a".repeat(64)).unwrap(),
    )
    .unwrap()
}

fn password_fingerprint(byte: u8) -> SensitiveInputs {
    let key = FingerprintKeyId::new(
        uuid::Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").unwrap(),
    )
    .unwrap();
    SensitiveInputs::try_from_entries([(
        SensitivePropertyPath::parse("password").unwrap(),
        SensitiveFingerprint::new_v1(key, [byte; 32]),
    )])
    .unwrap()
}

#[tokio::test]
async fn authoritative_absence_allows_libsql_create_with_required_inputs() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECTS),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", EMPTY_LIBSQL),
    ]);
    let state = base_state(&server);
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("password"), "create-canary").unwrap();
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    libsql:
      main:
        username: app
        password: { file: password }
        node: { type: primary }
"#,
    )
    .unwrap();
    let desired = compile_desired_for_instance(
        &config,
        ConfigDigest::parse("a".repeat(64)).unwrap(),
        InstanceIdentity::parse(&server.url).unwrap(),
        workspace.path(),
    )
    .unwrap();

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

    assert!(matches!(
        remote.observation(&"libsql.main".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    assert!(plan.applyable());
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn managed_libsql_requires_direct_agreement_and_projects_atomic_node() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECTS),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", POPULATED_LIBSQL),
        ("200 OK", LIBSQL_ONE),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "libsql.main",
        ResourceKind::LibSql,
        "libsql-1",
        serde_json::json!({"description":"old","username":"app","node":{"type":"primary"}}),
        Some("environment.production"),
        password_fingerprint(1),
    );
    let desired = compile(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    libsql:
      main:
        description: old
        username: app
        node: { type: primary }
"#,
    );

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let Some(RemoteObservation::Present(resource)) =
        remote.observation(&"libsql.main".parse().unwrap())
    else {
        panic!("LibSQL should be present")
    };
    assert!(matches!(
        resource.property(&PropertyPath::Node),
        Some(PropertyObservation::Known(_))
    ));
    assert!(resource.property(&PropertyPath::Password).is_none());
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn node_change_is_delete_before_create_replacement() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECTS),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", POPULATED_LIBSQL),
        ("200 OK", LIBSQL_ONE),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "libsql.main",
        ResourceKind::LibSql,
        "libsql-1",
        serde_json::json!({"description":"old","username":"app","node":{"type":"primary"}}),
        Some("environment.production"),
        password_fingerprint(1),
    );
    let desired = compile(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    libsql:
      main:
        description: old
        username: app
        node:
          type: replica
          primary_url: http://primary.internal
"#,
    );
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
    assert!(plan.applyable());
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Replace);
    assert_eq!(
        plan.changes()[0].replacement_order(),
        Some(ReplacementOrder::DeleteBeforeCreate)
    );
    server.finish();
}

#[tokio::test]
async fn password_rotation_and_metadata_are_one_applyable_update_plan() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECTS),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", POPULATED_LIBSQL),
        ("200 OK", LIBSQL_ONE),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "libsql.main",
        ResourceKind::LibSql,
        "libsql-1",
        serde_json::json!({"description":"old","username":"app","node":{"type":"primary"}}),
        Some("environment.production"),
        password_fingerprint(1),
    );
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("password"), "rotation-canary").unwrap();
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    libsql:
      main:
        description: next
        username: next
        password: { file: password }
        node: { type: primary }
"#,
    )
    .unwrap();
    let desired = compile_desired_for_instance(
        &config,
        ConfigDigest::parse("a".repeat(64)).unwrap(),
        InstanceIdentity::parse(&server.url).unwrap(),
        workspace.path(),
    )
    .unwrap();
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
    assert!(plan.applyable());
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(plan.changes()[0].fields().len(), 3);
    server.finish();
}

#[tokio::test]
async fn failed_required_collection_blocks_a_successful_direct_read() {
    let unavailable = r#"{"code":"INTERNAL_SERVER_ERROR","message":"retry"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECTS),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("500 Internal Server Error", unavailable),
        ("500 Internal Server Error", unavailable),
        ("500 Internal Server Error", unavailable),
        ("200 OK", LIBSQL_ONE),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "libsql.main",
        ResourceKind::LibSql,
        "libsql-1",
        serde_json::json!({"description":"old","username":"app","node":{"type":"primary"}}),
        Some("environment.production"),
        password_fingerprint(1),
    );
    let desired = compile(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    libsql:
      main:
        description: next
"#,
    );
    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    assert!(matches!(
        remote.observation(&"libsql.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unavailable
        ))
    ));
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == PlanDiagnosticCode::RemoteUnavailable)
    );
    assert!(
        server
            .finish()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

#[tokio::test]
async fn partial_project_topology_cannot_prove_libsql_absence() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECTS),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", EMPTY_LIBSQL),
    ]);
    let state = base_state(&server);
    let desired = compile(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    libsql: { main: {} }
"#,
    );
    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority {
            libsql: LibSqlTopologyAuthority::Partial,
            ..DiscoveryAuthority::reconciliation()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        remote.observation(&"libsql.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn duplicate_names_and_direct_disagreement_fail_closed() {
    let duplicates = r#"{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","projectId":"project-1","name":"production","isDefault":true,"libsql":[{"libsqlId":"libsql-1","name":"main","appName":"a"},{"libsqlId":"libsql-2","name":"main","appName":"b"}]}]}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECTS),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", duplicates),
    ]);
    let state = base_state(&server);
    let desired = compile(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    libsql: { main: {} }
"#,
    );
    let error = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("duplicates fail closed");
    assert!(matches!(error, DiscoverRemoteError::DuplicateLibSqlName));
    server.finish();
}
