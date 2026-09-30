use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::{compile_desired, compile_desired_for_instance};
use dokploy_cli::remote::{
    DiscoverRemoteError, DiscoveryAuthority, MariaDbTopologyAuthority, discover_remote,
};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, PlanDiagnosticCode, PropertyObservation, PropertyPath,
    PropertyUnknownReason, RemoteFailureKind, RemoteObservation, StoredState, plan,
};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    FingerprintKeyId, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath, StateFile,
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
) {
    state
        .upsert_resource(
            address.parse().expect("address is valid"),
            ResourceState::new(
                kind,
                RemoteId::new(remote_id).expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(serde_json::json!({}))
                    .expect("managed inputs are valid"),
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
    );
    insert(
        &mut state,
        "environment.production",
        ResourceKind::Environment,
        "environment-1",
        Some("project.platform"),
    );
    state
}

#[tokio::test]
async fn authoritative_absence_allows_mariadb_create_without_a_root_password() {
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
    fs::write(workspace.path().join("mariadb-password"), "password-canary")
        .expect("password fixture is writable");
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    mariadb:
      main:
        database: app
        username: app
        password:
          file: mariadb-password
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
    .expect("MariaDB state is projected");

    assert!(matches!(
        remote.observation(&"mariadb.main".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.applyable());
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Create && change.address().to_string() == "mariadb.main"
    }));
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].starts_with("GET /api/mariadb.search?"));
}

#[tokio::test]
async fn duplicate_mariadb_names_in_one_environment_fail_closed() {
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
            r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main"},{"mariadbId":"mariadb-2","environmentId":"environment-1","name":"main"}],"total":2}"#,
        ),
    ]);
    let state = base_state(&server);
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    mariadb:
      main: {}
"#,
    )
    .unwrap();
    let desired = compile_desired(&config, ConfigDigest::parse("a".repeat(64)).unwrap()).unwrap();

    let error = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("ambiguous scoped names must fail closed");

    assert!(matches!(error, DiscoverRemoteError::DuplicateMariaDbName));
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn duplicate_mariadb_identities_fail_closed() {
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
            r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main"},{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"shadow"}],"total":2}"#,
        ),
    ]);
    let state = base_state(&server);
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    mariadb:
      main: {}
"#,
    )
    .unwrap();
    let desired = compile_desired(&config, ConfigDigest::parse("a".repeat(64)).unwrap()).unwrap();

    let error = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("duplicate physical identities must fail closed");

    assert!(matches!(error, DiscoverRemoteError::DuplicateMariaDbId));
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn managed_mariadb_requires_direct_and_collection_agreement_and_redacts_credentials() {
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
            include_str!("../../../fixtures/api/live/v0.30.6/mariadb-search.created.owner.json"),
        ),
        (
            "200 OK",
            include_str!("../../../fixtures/api/live/v0.30.6/mariadb-one.created.owner.json"),
        ),
    ]);
    let mut state = base_state(&server);
    state
        .upsert_resource(
            "mariadb.main".parse().unwrap(),
            ResourceState::new(
                ResourceKind::MariaDb,
                RemoteId::new("mariadb-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({
                    "database": "app",
                    "username": "app",
                    "password": null,
                    "root_password": null
                }))
                .unwrap(),
                Some("environment.production".parse().unwrap()),
                Vec::new(),
            ),
        )
        .unwrap();
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    mariadb:
      main:
        database: app
        username: app
        password: null
        root_password: null
"#,
    )
    .unwrap();
    let desired = compile_desired(&config, ConfigDigest::parse("a".repeat(64)).unwrap()).unwrap();

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect("MariaDB state is projected");
    let Some(RemoteObservation::Present(mariadb)) =
        remote.observation(&"mariadb.main".parse().unwrap())
    else {
        panic!("MariaDB database should be present");
    };

    assert!(matches!(
        mariadb.property(&PropertyPath::Database),
        Some(PropertyObservation::Known(_))
    ));
    assert!(matches!(
        mariadb.property(&PropertyPath::Username),
        Some(PropertyObservation::Known(_))
    ));
    for path in [PropertyPath::Password, PropertyPath::RootPassword] {
        assert!(matches!(
            mariadb.property(&path),
            Some(PropertyObservation::Unknown(
                PropertyUnknownReason::Sensitive
            ))
        ));
    }
    let debug = format!("{remote:?}");
    assert!(!debug.contains("<redacted>"));
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn unavailable_mariadb_collection_blocks_a_successful_direct_read_and_any_mutation() {
    let unavailable = r#"{"code":"INTERNAL_SERVER_ERROR","message":"retry"}"#;
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
        ("500 Internal Server Error", unavailable),
        ("500 Internal Server Error", unavailable),
        ("500 Internal Server Error", unavailable),
        (
            "200 OK",
            r#"{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main","appName":"mariadb-main","dockerImage":"mariadb:11","databaseName":"old","databaseUser":"old"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    state
        .upsert_resource(
            "mariadb.main".parse().unwrap(),
            ResourceState::new(
                ResourceKind::MariaDb,
                RemoteId::new("mariadb-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({
                    "database": "old",
                    "username": "old"
                }))
                .unwrap(),
                Some("environment.production".parse().unwrap()),
                Vec::new(),
            ),
        )
        .unwrap();
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    mariadb:
      main:
        database: next
        username: next
"#,
    )
    .unwrap();
    let desired = compile_desired(&config, ConfigDigest::parse("a".repeat(64)).unwrap()).unwrap();

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect("collection failure is projected conservatively");

    assert!(matches!(
        remote.observation(&"mariadb.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unavailable
        ))
    ));
    let stored = StoredState::try_from_state(&state).unwrap();
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == PlanDiagnosticCode::RemoteUnavailable)
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 7);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[tokio::test]
async fn mariadb_database_and_username_changes_update_in_place() {
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
            r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main","appName":"mariadb-main","dockerImage":"mariadb:11","databaseName":"old","databaseUser":"old"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "mariadb.main",
        ResourceKind::MariaDb,
        "mariadb-1",
        Some("environment.production"),
    );
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    mariadb:
      main:
        database: next
        username: next
"#,
    )
    .unwrap();
    let desired = compile_desired(&config, ConfigDigest::parse("a".repeat(64)).unwrap()).unwrap();

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect("MariaDB state is projected");
    let stored = StoredState::try_from_state(&state).unwrap();
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(plan.applyable());
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Update && change.address().to_string() == "mariadb.main"
    }));
    server.finish();
}

#[tokio::test]
async fn mariadb_password_changes_after_create_are_unsupported() {
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
            r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main","appName":"mariadb-main","dockerImage":"mariadb:11","databaseName":"app","databaseUser":"app"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    let key_id = FingerprintKeyId::new(
        uuid::Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").unwrap(),
    )
    .unwrap();
    let sensitive = SensitiveInputs::try_from_entries([
        (
            SensitivePropertyPath::parse("password").unwrap(),
            SensitiveFingerprint::new_v1(key_id.clone(), [1; 32]),
        ),
        (
            SensitivePropertyPath::parse("root_password").unwrap(),
            SensitiveFingerprint::new_v1(key_id, [2; 32]),
        ),
    ])
    .unwrap();
    let mariadb = ResourceState::try_new(
        ResourceKind::MariaDb,
        RemoteId::new("mariadb-1").unwrap(),
        false,
        ManagedInputs::try_from_json(serde_json::json!({
            "database": "app",
            "username": "app"
        }))
        .unwrap(),
        sensitive,
        Some("environment.production".parse().unwrap()),
        Vec::new(),
    )
    .unwrap();
    state
        .upsert_resource("mariadb.main".parse().unwrap(), mariadb)
        .unwrap();
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    mariadb:
      main:
        database: app
        username: app
        password: null
        root_password: null
"#,
    )
    .unwrap();
    let desired = compile_desired(&config, ConfigDigest::parse("a".repeat(64)).unwrap()).unwrap();

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let stored = StoredState::try_from_state(&state).unwrap();
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert!(plan.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == PlanDiagnosticCode::UnsupportedMutation
            && matches!(
                diagnostic.property(),
                Some(PropertyPath::Password | PropertyPath::RootPassword)
            )
    }));
    server.finish();
}

#[tokio::test]
async fn partial_mariadb_search_cannot_prove_absence() {
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
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    mariadb:
      main: {}
"#,
    )
    .unwrap();
    let desired = compile_desired(&config, ConfigDigest::parse("a".repeat(64)).unwrap()).unwrap();
    let authority = DiscoveryAuthority {
        mariadb: MariaDbTopologyAuthority::Partial,
        ..DiscoveryAuthority::reconciliation()
    };

    let remote = discover_remote(&server.client(), &desired, &state, authority)
        .await
        .expect("partial search projects conservatively");

    assert!(matches!(
        remote.observation(&"mariadb.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn mariadb_direct_and_collection_disagreement_fails_closed() {
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
            r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"other"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"main","appName":"mariadb-main","dockerImage":"mariadb:11","databaseName":"app","databaseUser":"app"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "mariadb.main",
        ResourceKind::MariaDb,
        "mariadb-1",
        Some("environment.production"),
    );
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    mariadb:
      main: {}
"#,
    )
    .unwrap();
    let desired = compile_desired(&config, ConfigDigest::parse("a".repeat(64)).unwrap()).unwrap();

    let error = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("contradictory endpoints must fail closed");

    assert!(matches!(
        error,
        DiscoverRemoteError::MariaDbTopologyConflict
    ));
    server.finish();
}
