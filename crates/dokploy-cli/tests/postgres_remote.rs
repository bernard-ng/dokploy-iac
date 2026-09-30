use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{
    ApplicationTopologyAuthority, DiscoverRemoteError, DiscoveryAuthority,
    EnvironmentTopologyAuthority, PostgresTopologyAuthority, ProjectTopologyAuthority,
    RedisTopologyAuthority, discover_remote,
};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, PlanDiagnosticCode, PropertyObservation, PropertyPath,
    PropertyUnknownReason, RemoteFailureKind, RemoteObservation, StoredState, plan,
};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceKind, ResourceState, StateFile,
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

fn compile(yaml: &str) -> dokploy_cli::desired::CompiledDesired {
    let config = DokployConfig::parse(yaml).expect("configuration is valid");
    let digest = ConfigDigest::parse("a".repeat(64)).expect("digest is valid");
    compile_desired(&config, digest).expect("configuration compiles")
}

fn authoritative() -> DiscoveryAuthority {
    DiscoveryAuthority {
        projects: ProjectTopologyAuthority::Authoritative,
        environments: EnvironmentTopologyAuthority::Authoritative,
        applications: ApplicationTopologyAuthority::Authoritative,
        postgres: PostgresTopologyAuthority::Authoritative,
        redis: RedisTopologyAuthority::Authoritative,
    }
}

fn insert(
    state: &mut StateFile,
    address: &str,
    kind: ResourceKind,
    remote_id: &str,
    inputs: serde_json::Value,
    dependencies: &[&str],
) {
    state
        .upsert_resource(
            address.parse().expect("address is valid"),
            ResourceState::new(
                kind,
                RemoteId::new(remote_id).expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(inputs).expect("managed inputs are valid"),
                dependencies
                    .iter()
                    .map(|dependency| dependency.parse().expect("dependency is valid"))
                    .collect(),
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
        serde_json::json!({}),
        &[],
    );
    insert(
        &mut state,
        "environment.production",
        ResourceKind::Environment,
        "environment-1",
        serde_json::json!({}),
        &["project.platform"],
    );
    state
}

fn config(postgres: &str) -> String {
    format!(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main:
{postgres}
"#
    )
}

const PROJECT: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;

#[tokio::test]
async fn compiler_discovery_and_planner_create_an_absent_postgres_database() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        database: app"));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("Postgres state is projected");

    assert!(matches!(
        remote.observation(&"postgres.main".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Create && change.address().to_string() == "postgres.main"
    }));

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].starts_with("GET /api/postgres.search?"));
    assert!(requests[3].contains("environmentId=environment-1"));
}

#[tokio::test]
async fn managed_postgres_projects_requested_fields_without_secret_bytes() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"main","appName":"postgres-main","dockerImage":"postgres:16","databaseName":"old","databaseUser":null,"databasePassword":"password-canary","env":"SECRET=environment-canary"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({"database":"old","username":null,"password":null}),
        &["environment.production"],
    );
    let desired = compile(&config(
        r#"        database: new
        username: null
        password: null"#,
    ));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("Postgres state is projected");
    let Some(RemoteObservation::Present(postgres)) =
        remote.observation(&"postgres.main".parse().unwrap())
    else {
        panic!("Postgres database should be present");
    };

    assert!(matches!(
        postgres.property(&PropertyPath::Database),
        Some(PropertyObservation::Known(_))
    ));
    assert!(matches!(
        postgres.property(&PropertyPath::Username),
        Some(PropertyObservation::KnownAbsent)
    ));
    assert!(matches!(
        postgres.property(&PropertyPath::Password),
        Some(PropertyObservation::Unknown(
            PropertyUnknownReason::Sensitive
        ))
    ));
    let debug = format!("{remote:?}");
    assert!(!debug.contains("password-canary"));
    assert!(!debug.contains("environment-canary"));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Update && change.address().to_string() == "postgres.main"
    }));
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn missing_managed_id_exposes_a_same_name_replacement_without_adopting_it() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"replacement-2","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"missing"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "original-1",
        serde_json::json!({"database":"app"}),
        &["environment.production"],
    );
    let desired = compile(&config("        database: app"));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("replacement is projected for planner identity validation");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(plan.changes().is_empty());
    assert_eq!(plan.diagnostics().len(), 1);
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteIdentityMismatch
    );
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn partial_postgres_search_cannot_prove_absence() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        database: app"));
    let authority = DiscoveryAuthority {
        postgres: PostgresTopologyAuthority::Partial,
        ..authoritative()
    };

    let remote = discover_remote(&client, &desired, &state, authority)
        .await
        .expect("partial search projects conservatively");

    assert!(matches!(
        remote.observation(&"postgres.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn managed_postgres_in_another_environment_fails_closed() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"other-environment","name":"main","appName":"postgres-main","dockerImage":"postgres:16"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(&config("        {}"));

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("cross-environment containment is unsafe");

    assert!(matches!(error, DiscoverRemoteError::PostgresContainment));
    server.finish();
}

#[tokio::test]
async fn duplicate_scoped_postgres_names_are_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"main"},{"postgresId":"postgres-2","environmentId":"environment-1","name":"main"}],"total":2}"#,
        ),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("duplicate exact names are ambiguous");

    assert!(matches!(error, DiscoverRemoteError::DuplicatePostgresName));
    server.finish();
}

#[tokio::test]
async fn invalid_postgres_search_identity_is_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("an empty remote identity is invalid");

    assert!(matches!(error, DiscoverRemoteError::InvalidPostgresId));
    server.finish();
}

#[tokio::test]
async fn direct_404_conflicting_with_search_identity_blocks_planning() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"missing"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({"database":"app"}),
        &["environment.production"],
    );
    let desired = compile(&config("        database: app"));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("endpoint conflict is represented conservatively");
    assert!(matches!(
        remote.observation(&"postgres.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.changes().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteUnavailable
    );
    server.finish();
}

#[tokio::test]
async fn direct_404_conflicting_with_a_renamed_search_identity_blocks_planning() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"renamed"}],"total":1}"#,
        ),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"missing"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({"database":"app"}),
        &["environment.production"],
    );
    let desired = compile(&config("        database: app"));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("endpoint conflict is represented conservatively");
    assert!(matches!(
        remote.observation(&"postgres.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.changes().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteUnavailable
    );
    server.finish();
}

#[tokio::test]
async fn direct_404_conflicting_with_identity_in_another_environment_blocks_planning() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"},{"environmentId":"environment-2","name":"staging"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-2","name":"staging","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-2","name":"rogue"}],"total":1}"#,
        ),
        ("200 OK", r#"{"items":[],"total":0}"#),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"missing"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "environment.staging",
        ResourceKind::Environment,
        "environment-2",
        serde_json::json!({}),
        &["project.platform"],
    );
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({"database":"app"}),
        &["environment.production"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main:
        database: app
  staging:
    postgres:
      analytics: {}
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("cross-parent endpoint conflict is represented conservatively");
    assert!(matches!(
        remote.observation(&"postgres.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.changes().iter().all(|change| {
        !(change.kind() == ChangeKind::Create && change.address().to_string() == "postgres.main")
    }));
    assert!(
        plan.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == PlanDiagnosticCode::RemoteUnavailable)
    );
    assert_eq!(server.finish().len(), 7);
}

#[tokio::test]
async fn direct_read_conflicting_with_identity_in_another_partial_collection_is_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"},{"environmentId":"environment-2","name":"staging"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-2","name":"staging","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-2","name":"rogue"}],"total":1}"#,
        ),
        ("200 OK", r#"{"items":[],"total":0}"#),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"main","appName":"postgres-main","dockerImage":"postgres:16"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "environment.staging",
        ResourceKind::Environment,
        "environment-2",
        serde_json::json!({}),
        &["project.platform"],
    );
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main: {}
  staging:
    postgres:
      analytics: {}
"#,
    );
    let authority = DiscoveryAuthority {
        postgres: PostgresTopologyAuthority::Partial,
        ..authoritative()
    };

    let error = discover_remote(&client, &desired, &state, authority)
        .await
        .expect_err("positive cross-parent identity evidence is conclusive");

    assert!(matches!(
        error,
        DiscoverRemoteError::PostgresTopologyConflict
    ));
    assert_eq!(server.finish().len(), 7);
}

#[tokio::test]
async fn authoritative_search_omitting_a_directly_read_database_is_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"main","appName":"postgres-main","dockerImage":"postgres:16"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(&config("        {}"));

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("authoritative endpoints cannot contradict each other");

    assert!(matches!(
        error,
        DiscoverRemoteError::PostgresTopologyConflict
    ));
    server.finish();
}

#[tokio::test]
async fn postgres_parent_change_to_another_physical_environment_is_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"},{"environmentId":"environment-2","name":"staging"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-2","name":"staging","projectId":"project-1"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "environment.staging",
        ResourceKind::Environment,
        "environment-2",
        serde_json::json!({}),
        &["project.platform"],
    );
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
  staging:
    postgres:
      main: {}
"#,
    );

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("Postgres reparenting has no remote action yet");

    assert!(matches!(
        error,
        DiscoverRemoteError::PostgresReparentUnsupported
    ));
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn duplicate_postgres_identity_across_environments_is_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"},{"environmentId":"environment-2","name":"staging"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-2","name":"staging","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"duplicate-1","environmentId":"environment-2","name":"analytics"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"duplicate-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "environment.staging",
        ResourceKind::Environment,
        "environment-2",
        serde_json::json!({}),
        &["project.platform"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main: {}
  staging:
    postgres:
      analytics: {}
"#,
    );

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("one physical database cannot occupy two environments");

    assert!(matches!(error, DiscoverRemoteError::DuplicatePostgresId));
    assert_eq!(server.finish().len(), 6);
}

#[tokio::test]
async fn postgres_move_within_one_environment_preserves_remote_identity() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"legacy"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"legacy","appName":"postgres-main","dockerImage":"postgres:16"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.legacy",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main: {}
moves:
  - from: postgres.legacy
    to: postgres.main
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("same-parent logical move is observable");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Move && change.address().to_string() == "postgres.main"
    }));
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn environment_move_keeps_postgres_on_the_same_physical_parent() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"legacy"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"legacy","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"main","appName":"postgres-main","dockerImage":"postgres:16"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert(
        &mut state,
        "project.platform",
        ResourceKind::Project,
        "project-1",
        serde_json::json!({}),
        &[],
    );
    insert(
        &mut state,
        "environment.legacy",
        ResourceKind::Environment,
        "environment-1",
        serde_json::json!({}),
        &["project.platform"],
    );
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({}),
        &["environment.legacy"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main: {}
moves:
  - from: environment.legacy
    to: environment.production
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("the moved logical parent resolves to its stored physical identity");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Move
            && change.address().to_string() == "environment.production"
    }));
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn persisted_environment_move_keeps_postgres_discovery_idempotent() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"main","appName":"postgres-main","dockerImage":"postgres:16"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main: {}
moves:
  - from: environment.legacy
    to: environment.production
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("checkpointed parent move resolves through its stored target");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().is_empty());
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn state_backed_postgres_removal_is_observed_by_remote_id() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"legacy"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"legacy","appName":"postgres-main","dockerImage":"postgres:16"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.legacy",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
removed:
  - from: postgres.legacy
    destroy: true
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("state-backed removal is projected");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Delete && change.address().to_string() == "postgres.legacy"
    }));
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn persisted_postgres_removal_needs_no_parent_or_remote_probe() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
removed:
  - from: postgres.legacy
    destroy: true
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("checkpointed removal is idempotent");
    assert!(
        remote
            .observation(&"postgres.legacy".parse().unwrap())
            .is_none()
    );
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().is_empty());
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn every_combined_discovery_invocation_reads_fresh_postgres_state() {
    let responses = vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ];
    let server = TestServer::respond_in_sequence(responses);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    for _ in 0..2 {
        let remote = discover_remote(&client, &desired, &state, authoritative())
            .await
            .expect("each invocation performs fresh reads");
        assert!(matches!(
            remote.observation(&"postgres.main".parse().unwrap()),
            Some(RemoteObservation::Missing)
        ));
    }

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /api/postgres.search?"))
            .count(),
        2
    );
}

#[tokio::test]
async fn postgres_search_authentication_failure_is_closed() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "401 Unauthorized",
            r#"{"code":"UNAUTHORIZED","message":"denied"}"#,
        ),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("authentication failure is projected");

    assert!(matches!(
        remote.observation(&"postgres.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unauthorized
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn malformed_postgres_pagination_is_an_invalid_response() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":1}"#),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("pagination failure is projected");

    assert!(matches!(
        remote.observation(&"postgres.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn omitted_owned_postgres_field_blocks_planning() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"main","appName":"postgres-main","dockerImage":"postgres:16"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({"database":"app"}),
        &["environment.production"],
    );
    let desired = compile(&config("        database: app"));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("omitted field is preserved as unknown");
    let Some(RemoteObservation::Present(postgres)) =
        remote.observation(&"postgres.main".parse().unwrap())
    else {
        panic!("Postgres database should be present");
    };
    assert!(matches!(
        postgres.property(&PropertyPath::Database),
        Some(PropertyObservation::Unknown(
            PropertyUnknownReason::NotReturned
        ))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.changes().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::UnknownPropertyObservation
    );
    server.finish();
}

#[tokio::test]
async fn direct_postgres_identity_must_match_the_requested_stored_id() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"postgresId":"different-2","environmentId":"environment-1","name":"main","appName":"postgres-main","dockerImage":"postgres:16"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "postgres.main",
        ResourceKind::Postgres,
        "postgres-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(&config("        {}"));

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("a direct endpoint cannot substitute another identity");

    assert!(matches!(error, DiscoverRemoteError::InvalidPostgresId));
    server.finish();
}

#[tokio::test]
async fn exhausted_postgres_search_server_failure_is_unavailable() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "503 Service Unavailable",
            r#"{"code":"UNAVAILABLE","message":"retry"}"#,
        ),
        (
            "503 Service Unavailable",
            r#"{"code":"UNAVAILABLE","message":"retry"}"#,
        ),
        (
            "503 Service Unavailable",
            r#"{"code":"UNAVAILABLE","message":"retry"}"#,
        ),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("server failure is projected");

    assert!(matches!(
        remote.observation(&"postgres.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unavailable
        ))
    ));
    assert_eq!(server.finish().len(), 6);
}
