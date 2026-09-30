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

fn compile(yaml: &str) -> dokploy_cli::desired::CompiledDesired {
    let config = DokployConfig::parse(yaml).expect("configuration is valid");
    let digest = ConfigDigest::parse("a".repeat(64)).expect("digest is valid");
    compile_desired(&config, digest).expect("configuration compiles")
}

fn authority(applications: ApplicationTopologyAuthority) -> DiscoveryAuthority {
    DiscoveryAuthority {
        projects: ProjectTopologyAuthority::Authoritative,
        environments: EnvironmentTopologyAuthority::Authoritative,
        applications,
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
    let mut dependencies = dependencies
        .iter()
        .map(|dependency| dependency.parse().expect("dependency is valid"))
        .collect::<Vec<ResourceAddress>>();
    let containment = kind.containment_parent_kind().and_then(|required| {
        dependencies
            .iter()
            .find(|dependency| dependency.kind() == required)
            .cloned()
    });
    if let Some(parent) = containment.as_ref() {
        dependencies.retain(|dependency| dependency != parent);
    }
    state
        .upsert_resource(
            address.parse().expect("address is valid"),
            ResourceState::new(
                kind,
                RemoteId::new(remote_id).expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(inputs).expect("managed inputs are valid"),
                containment,
                dependencies,
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

fn config(application: &str) -> String {
    format!(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
{application}
"#
    )
}

const PROJECT: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;

#[tokio::test]
async fn compiler_discovery_and_planner_create_an_absent_application() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        description: API"));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("application state is projected");

    assert!(matches!(
        remote.observation(&"application.api".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Create && change.address().to_string() == "application.api"
    }));

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].starts_with("GET /api/application.search?"));
    assert!(requests[3].contains("environmentId=environment-1"));
}

#[tokio::test]
async fn managed_application_projects_only_requested_fields_without_secret_bytes() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api","description":"Old","replicas":1,"repository":"owner/old","branch":null,"sourceType":"github","buildPath":"secret/path","env":"TOKEN=environment-value-canary"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({
            "description": "Old",
            "replicas": 1,
            "source": {"repository": "owner/old", "branch": null},
            "environment": {"TOKEN": null}
        }),
        &["environment.production"],
    );
    let desired = compile(&config(
        r#"        description: New
        replicas: 2
        source:
          type: github
          repository: owner/new
          branch: null
        environment:
          TOKEN: null"#,
    ));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("application state is projected");
    let Some(RemoteObservation::Present(application)) =
        remote.observation(&"application.api".parse().unwrap())
    else {
        panic!("application should be present");
    };

    assert!(matches!(
        application.property(&PropertyPath::Description),
        Some(PropertyObservation::Known(_))
    ));
    assert!(matches!(
        application.property(&PropertyPath::environment_variable("TOKEN").unwrap()),
        Some(PropertyObservation::Unknown(
            PropertyUnknownReason::Sensitive
        ))
    ));
    assert!(!format!("{remote:?}").contains("environment-value-canary"));
    assert!(application.property(&PropertyPath::Source).is_none());
    assert!(application.property(&PropertyPath::Environment).is_none());
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Update && change.address().to_string() == "application.api"
    }));
    assert!(
        !String::from_utf8(plan.to_json_bytes())
            .expect("plan is UTF-8")
            .contains("environment-value-canary")
    );
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
            r#"{"items":[{"applicationId":"replacement-2","environmentId":"environment-1","name":"api"}],"total":1}"#,
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
        "application.api",
        ResourceKind::Application,
        "original-1",
        serde_json::json!({"description":"Old"}),
        &["environment.production"],
    );
    let desired = compile(&config("        description: Old"));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
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
async fn partial_application_search_cannot_prove_absence() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        description: API"));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Partial),
    )
    .await
    .expect("partial search projects conservatively");

    assert!(matches!(
        remote.observation(&"application.api".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn managed_application_in_another_environment_fails_closed() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"other-environment","name":"api","appName":"api"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(&config("        {}"));

    let error = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect_err("cross-environment containment is unsafe");

    assert!(matches!(error, DiscoverRemoteError::ApplicationContainment));
    server.finish();
}

#[tokio::test]
async fn duplicate_scoped_application_names_are_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"},{"applicationId":"application-2","environmentId":"environment-1","name":"api"}],"total":2}"#,
        ),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let error = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect_err("duplicate exact names are ambiguous");

    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateApplicationName
    ));
    server.finish();
}

#[tokio::test]
async fn omitted_source_shape_blocks_an_owned_source_clear() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api","repository":null,"branch":null}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({"source":{"repository":"owner/repository"}}),
        &["environment.production"],
    );
    let desired = compile(&config("        source: null"));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("omitted source shape remains an explicit unknown");
    let Some(RemoteObservation::Present(application)) =
        remote.observation(&"application.api".parse().unwrap())
    else {
        panic!("application should be present");
    };
    assert!(matches!(
        application.property(&PropertyPath::Source),
        Some(PropertyObservation::Unknown(
            PropertyUnknownReason::NotReturned
        ))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.changes().is_empty());
    assert_eq!(plan.diagnostics().len(), 1);
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::UnknownPropertyObservation
    );
    server.finish();
}

#[tokio::test]
async fn non_github_source_type_blocks_owned_github_child_comparison() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api","sourceType":"docker","repository":"owner/repository","branch":null}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({"source":{"repository":"owner/repository","branch":null}}),
        &["environment.production"],
    );
    let desired = compile(&config(
        r#"        source:
          type: github
          repository: owner/repository
          branch: null"#,
    ));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("provider mismatch is represented as an invalid child observation");
    let Some(RemoteObservation::Present(application)) =
        remote.observation(&"application.api".parse().unwrap())
    else {
        panic!("application should be present");
    };
    assert!(matches!(
        application.property(&PropertyPath::SourceRepository),
        Some(PropertyObservation::Unknown(
            PropertyUnknownReason::InvalidResponse
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
async fn environment_root_shape_is_projected_without_retaining_its_bytes() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api","env":"TOKEN=environment-root-canary"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({"environment":{}}),
        &["environment.production"],
    );
    let desired = compile(&config("        environment: {}"));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("environment shape is projected");
    let Some(RemoteObservation::Present(application)) =
        remote.observation(&"application.api".parse().unwrap())
    else {
        panic!("application should be present");
    };
    assert!(matches!(
        application.property(&PropertyPath::Environment),
        Some(PropertyObservation::Known(_))
    ));
    assert!(!format!("{remote:?}").contains("environment-root-canary"));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Update && change.address().to_string() == "application.api"
    }));
    server.finish();
}

#[tokio::test]
async fn null_environment_root_proves_requested_sensitive_children_absent() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api","env":null}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({"environment":{"TOKEN":null}}),
        &["environment.production"],
    );
    let desired = compile(&config(
        r#"        environment:
          TOKEN: null"#,
    ));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("null environment proves child absence");
    let Some(RemoteObservation::Present(application)) =
        remote.observation(&"application.api".parse().unwrap())
    else {
        panic!("application should be present");
    };
    assert!(matches!(
        application.property(&PropertyPath::environment_variable("TOKEN").unwrap()),
        Some(PropertyObservation::KnownAbsent)
    ));
    server.finish();
}

#[tokio::test]
async fn omitted_environment_root_blocks_requested_sensitive_children() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({"environment":{"TOKEN":null}}),
        &["environment.production"],
    );
    let desired = compile(&config(
        r#"        environment:
          TOKEN: null"#,
    ));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("omitted environment remains a distinct unknown");
    let Some(RemoteObservation::Present(application)) =
        remote.observation(&"application.api".parse().unwrap())
    else {
        panic!("application should be present");
    };
    assert!(matches!(
        application.property(&PropertyPath::environment_variable("TOKEN").unwrap()),
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
async fn ignored_application_field_is_not_projected_or_planned() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({"description":"Old"}),
        &["environment.production"],
    );
    let desired = compile(&config(
        r#"        description: New
        lifecycle:
          ignore_changes:
            - description"#,
    ));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("ignored application field needs no observation");
    let Some(RemoteObservation::Present(application)) =
        remote.observation(&"application.api".parse().unwrap())
    else {
        panic!("application should be present");
    };
    assert!(application.property(&PropertyPath::Description).is_none());
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().is_empty());
    server.finish();
}

#[tokio::test]
async fn same_address_reparenting_validates_the_stored_physical_environment() {
    let server = TestServer::respond_in_sequence(vec![
        (
            "200 OK",
            r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#,
        ),
        (
            "200 OK",
            r#"[{"environmentId":"environment-new","name":"new"},{"environmentId":"environment-old","name":"old"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-new","name":"new","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-old","name":"old","projectId":"project-1"}"#,
        ),
        ("200 OK", r#"{"items":[],"total":0}"#),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-old","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-old","name":"api","appName":"api"}"#,
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
        "environment.new",
        ResourceKind::Environment,
        "environment-new",
        serde_json::json!({}),
        &["project.platform"],
    );
    insert(
        &mut state,
        "environment.old",
        ResourceKind::Environment,
        "environment-old",
        serde_json::json!({}),
        &["project.platform"],
    );
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({}),
        &["environment.old"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  new:
    applications:
      api: {}
  old: {}
"#,
    );

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("current containment is validated before reparenting");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::NoOp && change.address().to_string() == "application.api"
    }));
    let requests = server.finish();
    assert_eq!(requests.len(), 7);
    assert!(requests[4].contains("environmentId=environment-new"));
    assert!(requests[5].contains("environmentId=environment-old"));
}

#[tokio::test]
async fn same_address_reparenting_reports_a_conclusive_target_name_collision() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-new","name":"new"},{"environmentId":"environment-old","name":"old"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-new","name":"new","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-old","name":"old","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"collision-2","environmentId":"environment-new","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-old","name":"api"}],"total":1}"#,
        ),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"missing"}"#,
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
    for (name, remote_id) in [("new", "environment-new"), ("old", "environment-old")] {
        insert(
            &mut state,
            &format!("environment.{name}"),
            ResourceKind::Environment,
            remote_id,
            serde_json::json!({}),
            &["project.platform"],
        );
    }
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({}),
        &["environment.old"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  new:
    applications:
      api: {}
  old: {}
"#,
    );

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("target collision is represented for planner identity validation");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.changes().is_empty());
    assert_eq!(plan.diagnostics().len(), 1);
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteIdentityMismatch
    );
    server.finish();
}

#[tokio::test]
async fn managed_404_and_unavailable_reparent_target_fail_closed() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-new","name":"new"},{"environmentId":"environment-old","name":"old"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-new","name":"new","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-old","name":"old","projectId":"project-1"}"#,
        ),
        ("503 Service Unavailable", r#"{"message":"busy"}"#),
        ("503 Service Unavailable", r#"{"message":"busy"}"#),
        ("503 Service Unavailable", r#"{"message":"busy"}"#),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-old","name":"api"}],"total":1}"#,
        ),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"missing"}"#,
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
    for (name, remote_id) in [("new", "environment-new"), ("old", "environment-old")] {
        insert(
            &mut state,
            &format!("environment.{name}"),
            ResourceKind::Environment,
            remote_id,
            serde_json::json!({}),
            &["project.platform"],
        );
    }
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({}),
        &["environment.old"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  new:
    applications:
      api: {}
  old: {}
"#,
    );

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("unresolved target probe is represented conservatively");

    assert!(matches!(
        remote.observation(&"application.api".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse | RemoteFailureKind::Unavailable
        ))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.changes().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteUnavailable
    );
    assert_eq!(server.finish().len(), 9);
}

#[tokio::test]
async fn managed_404_and_target_search_with_the_same_stored_id_fail_closed() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-new","name":"new"},{"environmentId":"environment-old","name":"old"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-new","name":"new","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-old","name":"old","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-new","name":"api"}],"total":1}"#,
        ),
        ("200 OK", r#"{"items":[],"total":0}"#),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"missing"}"#,
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
    for (name, remote_id) in [("new", "environment-new"), ("old", "environment-old")] {
        insert(
            &mut state,
            &format!("environment.{name}"),
            ResourceKind::Environment,
            remote_id,
            serde_json::json!({}),
            &["project.platform"],
        );
    }
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
        serde_json::json!({}),
        &["environment.old"],
    );
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  new:
    applications:
      api: {}
  old: {}
"#,
    );

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("contradictory target presence is represented conservatively");

    assert!(matches!(
        remote.observation(&"application.api".parse().unwrap()),
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
    assert_eq!(server.finish().len(), 7);
}

#[tokio::test]
async fn application_move_resolves_a_simultaneously_moved_environment_by_stored_identity() {
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
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"backend"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"backend","appName":"backend"}"#,
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
        "application.backend",
        ResourceKind::Application,
        "application-1",
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
    applications:
      api: {}
moves:
  - from: environment.legacy
    to: environment.production
  - from: application.backend
    to: application.api
"#,
    );

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("both moves resolve from durable physical identities");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Move && change.address().to_string() == "application.api"
    }));
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert!(requests[3].contains("environmentId=environment-1"));
}

#[tokio::test]
async fn persisted_application_move_observes_source_and_managed_target_idempotently() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"backend"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"backend","appName":"backend"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.api",
        ResourceKind::Application,
        "application-1",
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
    applications:
      api: {}
moves:
  - from: application.backend
    to: application.api
"#,
    );

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("persisted move target remains observable without probing the source");
    assert!(
        remote
            .observation(&"application.backend".parse().unwrap())
            .is_none()
    );
    assert!(matches!(
        remote.observation(&"application.api".parse().unwrap()),
        Some(RemoteObservation::Present(_))
    ));
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().is_empty());
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn state_backed_application_removal_is_observed_by_remote_id() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"legacy"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"applicationId":"application-1","environmentId":"environment-1","name":"legacy","appName":"legacy"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "application.legacy",
        ResourceKind::Application,
        "application-1",
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
  - from: application.legacy
    destroy: true
"#,
    );

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("state-backed removal is projected");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Delete && change.address().to_string() == "application.legacy"
    }));
    server.finish();
}

#[tokio::test]
async fn persisted_application_removal_needs_no_parent_or_remote_probe() {
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
  - from: application.legacy
    destroy: true
"#,
    );

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("checkpointed removal is idempotent without application containment");
    assert!(
        remote
            .observation(&"application.legacy".parse().unwrap())
            .is_none()
    );
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().is_empty());
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn every_combined_discovery_invocation_reads_fresh_application_state() {
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
        let remote = discover_remote(
            &client,
            &desired,
            &state,
            authority(ApplicationTopologyAuthority::Authoritative),
        )
        .await
        .expect("each invocation performs fresh reads");
        assert!(matches!(
            remote.observation(&"application.api".parse().unwrap()),
            Some(RemoteObservation::Missing)
        ));
    }

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /api/application.search?"))
            .count(),
        2
    );
}

#[tokio::test]
async fn application_search_authentication_failure_is_closed() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("401 Unauthorized", r#"{"message":"Unauthorized"}"#),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("authentication failure is represented in remote state");

    assert!(matches!(
        remote.observation(&"application.api".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unauthorized
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn malformed_application_pagination_is_an_invalid_response() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":1}"#),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let remote = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect("pagination contract failure is represented in remote state");

    assert!(matches!(
        remote.observation(&"application.api".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn duplicate_application_ids_across_environments_are_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"one"},{"environmentId":"environment-2","name":"two"}]"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-1","name":"one","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"environmentId":"environment-2","name":"two","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"duplicate-1","environmentId":"environment-1","name":"api"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"applicationId":"duplicate-1","environmentId":"environment-2","name":"worker"}],"total":1}"#,
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
    for (name, remote_id) in [("one", "environment-1"), ("two", "environment-2")] {
        insert(
            &mut state,
            &format!("environment.{name}"),
            ResourceKind::Environment,
            remote_id,
            serde_json::json!({}),
            &["project.platform"],
        );
    }
    let desired = compile(
        r#"
version: 1
project:
  name: platform
environments:
  one:
    applications:
      api: {}
  two:
    applications:
      worker: {}
"#,
    );

    let error = discover_remote(
        &client,
        &desired,
        &state,
        authority(ApplicationTopologyAuthority::Authoritative),
    )
    .await
    .expect_err("one physical application cannot belong to two environments");

    assert!(matches!(error, DiscoverRemoteError::DuplicateApplicationId));
    server.finish();
}
