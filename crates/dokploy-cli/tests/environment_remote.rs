use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{
    ApplicationTopologyAuthority, ComposeTopologyAuthority, DiscoverRemoteError,
    DiscoveryAuthority, DomainTopologyAuthority, EnvironmentTopologyAuthority,
    LibSqlTopologyAuthority, MariaDbTopologyAuthority, MongoTopologyAuthority,
    MountTopologyAuthority, MySqlTopologyAuthority, PortTopologyAuthority,
    PostgresTopologyAuthority, ProjectTopologyAuthority, RedisTopologyAuthority,
    ScheduleTopologyAuthority, discover_remote,
};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, PlanDiagnosticCode, PropertyObservation, PropertyPath,
    RemoteFailureKind, RemoteObservation, StoredState, plan,
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

fn request_is_complete(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\r\n\r\n")
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
        compose: ComposeTopologyAuthority::Authoritative,
        postgres: PostgresTopologyAuthority::Authoritative,
        mysql: MySqlTopologyAuthority::Authoritative,
        mariadb: MariaDbTopologyAuthority::Authoritative,
        mongo: MongoTopologyAuthority::Authoritative,
        libsql: LibSqlTopologyAuthority::Authoritative,
        redis: RedisTopologyAuthority::Authoritative,
        domains: DomainTopologyAuthority::Authoritative,
        ports: PortTopologyAuthority::Authoritative,
        redirects: dokploy_cli::remote::RedirectTopologyAuthority::Authoritative,
        security: dokploy_cli::remote::SecurityTopologyAuthority::Authoritative,
        schedules: ScheduleTopologyAuthority::Authoritative,
        mounts: MountTopologyAuthority::Authoritative,
    }
}

fn insert_project(state: &mut StateFile, address: &str, remote_id: &str) {
    state
        .upsert_resource(
            address.parse().expect("address is valid"),
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new(remote_id).expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(serde_json::json!({}))
                    .expect("managed inputs are valid"),
                None,
                Vec::new(),
            ),
        )
        .expect("state accepts project");
}

fn insert_environment(
    state: &mut StateFile,
    address: &str,
    remote_id: &str,
    description: &str,
    parent: &str,
) {
    state
        .upsert_resource(
            address.parse().expect("address is valid"),
            ResourceState::new(
                ResourceKind::Environment,
                RemoteId::new(remote_id).expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(serde_json::json!({"description": description}))
                    .expect("managed inputs are valid"),
                Some(parent.parse().expect("parent address is valid")),
                Vec::new(),
            ),
        )
        .expect("state accepts environment");
}

#[tokio::test]
async fn combined_discovery_proves_an_unmanaged_environment_missing_under_a_present_project() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        ("200 OK", "[]"),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("combined topology is projected");

    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Present(_))
    ));
    assert!(matches!(
        remote.observation(&"environment.production".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(
        plan.changes()
            .iter()
            .any(|change| change.kind() == ChangeKind::Create
                && change.address().to_string() == "environment.production")
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/project.all HTTP/1.1\r\n"));
    assert!(
        requests[1]
            .starts_with("GET /api/environment.byProjectId?projectId=project-1 HTTP/1.1\r\n")
    );
}

#[tokio::test]
async fn replacement_project_is_not_a_trusted_parent_for_an_unmanaged_environment() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"replacement-2","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"replacement-environment","name":"production"}]"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "original-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("the replacement parent is projected conservatively");

    assert!(matches!(
        remote.observation(&"environment.production".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
}

#[tokio::test]
async fn replacement_project_is_not_a_trusted_parent_through_a_move_target() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"replacement-2","name":"legacy","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"replacement-environment","name":"production"}]"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.legacy", "original-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
moves:
  - from: project.legacy
    to: project.platform
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("the moved replacement parent is projected conservatively");

    assert!(matches!(
        remote.observation(&"environment.production".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
}

#[tokio::test]
async fn managed_environment_is_matched_by_id_and_projects_requested_description() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"renamed-remotely","description":"Old"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"renamed-remotely","description":"Old","projectId":"project-1","env":"SECRET=canary"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    insert_environment(
        &mut state,
        "environment.production",
        "environment-1",
        "Old",
        "project.platform",
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    description: New
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("managed environment is projected");
    let Some(RemoteObservation::Present(environment)) =
        remote.observation(&"environment.production".parse().unwrap())
    else {
        panic!("environment should be present");
    };

    assert_eq!(environment.remote_id().as_str(), "environment-1");
    assert!(matches!(
        environment.property(&PropertyPath::Description),
        Some(PropertyObservation::Known(_))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(
        plan.changes()
            .iter()
            .any(|change| change.kind() == ChangeKind::Update
                && change.address().to_string() == "environment.production")
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .starts_with("GET /api/environment.one?environmentId=environment-1 HTTP/1.1\r\n")
    );
}

#[tokio::test]
async fn same_name_replacement_for_a_missing_managed_environment_is_not_adopted() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"replacement-2","name":"production","description":"Old"}]"#,
        ),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"Environment not found"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    insert_environment(
        &mut state,
        "environment.production",
        "original-1",
        "Old",
        "project.platform",
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    description: Old
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("replacement collision is projected");
    let Some(RemoteObservation::Present(environment)) =
        remote.observation(&"environment.production".parse().unwrap())
    else {
        panic!("same-name replacement should be visible");
    };
    assert_eq!(environment.remote_id().as_str(), "replacement-2");

    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.changes().is_empty());
    assert_eq!(plan.diagnostics().len(), 1);
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteIdentityMismatch
    );
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn partial_environment_collection_cannot_prove_absence() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        ("200 OK", "[]"),
    ]);
    let client = server.client();
    let state = server.state();
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );
    let authority = DiscoveryAuthority {
        projects: ProjectTopologyAuthority::Authoritative,
        environments: EnvironmentTopologyAuthority::Partial,
        applications: ApplicationTopologyAuthority::Authoritative,
        compose: ComposeTopologyAuthority::Authoritative,
        postgres: PostgresTopologyAuthority::Authoritative,
        mysql: MySqlTopologyAuthority::Authoritative,
        mariadb: MariaDbTopologyAuthority::Authoritative,
        mongo: MongoTopologyAuthority::Authoritative,
        libsql: LibSqlTopologyAuthority::Authoritative,
        redis: RedisTopologyAuthority::Authoritative,
        domains: DomainTopologyAuthority::Authoritative,
        ports: PortTopologyAuthority::Authoritative,
        redirects: dokploy_cli::remote::RedirectTopologyAuthority::Authoritative,
        security: dokploy_cli::remote::SecurityTopologyAuthority::Authoritative,
        schedules: ScheduleTopologyAuthority::Authoritative,
        mounts: MountTopologyAuthority::Authoritative,
    };

    let remote = discover_remote(&client, &desired, &state, authority)
        .await
        .expect("partial environment topology is projected conservatively");

    assert!(matches!(
        remote.observation(&"environment.production".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    assert_eq!(server.finish().len(), 2);
}

#[tokio::test]
async fn managed_environment_in_another_project_fails_closed() {
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
            r#"{"environmentId":"environment-1","name":"production","projectId":"other-project"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    insert_environment(
        &mut state,
        "environment.production",
        "environment-1",
        "Old",
        "project.platform",
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

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("cross-project containment drift is unsafe");

    assert!(matches!(error, DiscoverRemoteError::EnvironmentContainment));
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn environment_move_resolves_a_simultaneously_moved_parent_project() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"legacy-remote","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"legacy-remote"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"legacy-remote","projectId":"project-1"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.legacy", "project-1");
    insert_environment(
        &mut state,
        "environment.legacy",
        "environment-1",
        "Old",
        "project.legacy",
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
moves:
  - from: project.legacy
    to: project.platform
  - from: environment.legacy
    to: environment.production
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("both move probes are projected");

    assert!(matches!(
        remote.observation(&"project.legacy".parse().unwrap()),
        Some(RemoteObservation::Present(_))
    ));
    assert!(matches!(
        remote.observation(&"project.platform".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    assert!(matches!(
        remote.observation(&"environment.legacy".parse().unwrap()),
        Some(RemoteObservation::Present(_))
    ));
    assert!(matches!(
        remote.observation(&"environment.production".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));

    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert_eq!(
        plan.changes()
            .iter()
            .filter(|change| change.kind() == ChangeKind::Move)
            .count(),
        2
    );
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn state_backed_environment_removal_is_observed_by_remote_id() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        ("200 OK", "[]"),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"Environment not found"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    insert_environment(
        &mut state,
        "environment.legacy",
        "environment-1",
        "Old",
        "project.platform",
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
removed:
  - from: environment.legacy
    destroy: true
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("removal probe is projected");
    assert!(matches!(
        remote.observation(&"environment.legacy".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));

    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(
        plan.changes()
            .iter()
            .any(|change| change.kind() == ChangeKind::Forget
                && change.address().to_string() == "environment.legacy")
    );
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn combined_discovery_rejects_instance_mismatch_before_transport() {
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
environments:
  production: {}
"#,
    );

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("mismatched instances are unsafe");
    let rendered = format!("{error} {error:?}");

    assert!(matches!(error, DiscoverRemoteError::InstanceMismatch));
    assert_eq!(
        error.to_string(),
        "DOKREM001: client and state instances differ"
    );
    assert!(!rendered.contains("127.0.0.1"));
    assert!(!rendered.contains("other.example.com"));
}

#[tokio::test]
async fn environment_collection_authentication_failure_is_closed() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        ("401 Unauthorized", r#"{"message":"Unauthorized"}"#),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("authentication failure is a closed observation");

    assert!(matches!(
        remote.observation(&"environment.production".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unauthorized
        ))
    ));
    assert_eq!(server.finish().len(), 2);
}

#[tokio::test]
async fn exhausted_environment_collection_throttling_is_unavailable() {
    let failure = r#"{"code":"TOO_MANY_REQUESTS","message":"Try again"}"#;
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        ("429 Too Many Requests", failure),
        ("429 Too Many Requests", failure),
        ("429 Too Many Requests", failure),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("throttling is a closed observation");

    assert!(matches!(
        remote.observation(&"environment.production".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unavailable
        ))
    ));
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn every_combined_discovery_invocation_performs_fresh_reads() {
    let project = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", project),
        ("200 OK", "[]"),
        ("200 OK", project),
        ("200 OK", "[]"),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    for _ in 0..2 {
        discover_remote(&client, &desired, &state, authoritative())
            .await
            .expect("fresh combined topology is projected");
    }

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /api/project.all HTTP/1.1\r\n"))
            .count(),
        2
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request
                .starts_with("GET /api/environment.byProjectId?projectId=project-1 HTTP/1.1\r\n"))
            .count(),
        2
    );
}

#[tokio::test]
async fn duplicate_environment_names_within_a_project_are_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[
              {"environmentId":"environment-1","name":"production"},
              {"environmentId":"environment-2","name":"production"}
            ]"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("duplicate scoped names are ambiguous");

    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateEnvironmentName
    ));
    assert_eq!(server.finish().len(), 2);
}

#[tokio::test]
async fn duplicate_environment_ids_within_a_project_are_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[
              {"environmentId":"environment-1","name":"production"},
              {"environmentId":"environment-1","name":"staging"}
            ]"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("duplicate environment identities are ambiguous");

    assert!(matches!(error, DiscoverRemoteError::DuplicateEnvironmentId));
    assert_eq!(server.finish().len(), 2);
}

#[tokio::test]
async fn empty_environment_id_is_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        ("200 OK", r#"[{"environmentId":"","name":"production"}]"#),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    let error = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect_err("empty environment identities are invalid");

    assert!(matches!(error, DiscoverRemoteError::InvalidEnvironmentId));
    assert_eq!(server.finish().len(), 2);
}

#[tokio::test]
async fn unowned_environment_description_is_not_projected() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production","description":"description-canary"}]"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production: {}
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("name collision is projected");
    let Some(RemoteObservation::Present(environment)) =
        remote.observation(&"environment.production".parse().unwrap())
    else {
        panic!("environment collision should be present");
    };

    assert_eq!(environment.property(&PropertyPath::Description), None);
    assert!(!format!("{remote:?}").contains("description-canary"));
    assert_eq!(server.finish().len(), 2);
}

#[tokio::test]
async fn ignored_environment_description_is_not_requested_or_planned() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production","description":"Remote"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"production","description":"Remote","projectId":"project-1"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    insert_environment(
        &mut state,
        "environment.production",
        "environment-1",
        "Stored",
        "project.platform",
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    description: Desired
    lifecycle:
      ignore_changes: [description]
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("ignored description is excluded");
    let Some(RemoteObservation::Present(environment)) =
        remote.observation(&"environment.production".parse().unwrap())
    else {
        panic!("environment should be present");
    };
    assert_eq!(environment.property(&PropertyPath::Description), None);

    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(
        plan.changes()
            .iter()
            .all(|change| change.address().to_string() != "environment.production")
    );
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn omitted_owned_environment_description_is_unknown() {
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
    ]);
    let client = server.client();
    let mut state = server.state();
    insert_project(&mut state, "project.platform", "project-1");
    insert_environment(
        &mut state,
        "environment.production",
        "environment-1",
        "Stored",
        "project.platform",
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    description: Desired
"#,
    );

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("omitted description is presence-aware");
    let Some(RemoteObservation::Present(environment)) =
        remote.observation(&"environment.production".parse().unwrap())
    else {
        panic!("environment should be present");
    };

    assert!(matches!(
        environment.property(&PropertyPath::Description),
        Some(PropertyObservation::Unknown(
            dokploy_core::PropertyUnknownReason::NotReturned
        ))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert_eq!(plan.diagnostics().len(), 1);
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::UnknownPropertyObservation
    );
    assert_eq!(server.finish().len(), 3);
}
