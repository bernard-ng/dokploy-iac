use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{
    ApplicationTopologyAuthority, DiscoveryAuthority, DomainTopologyAuthority,
    EnvironmentTopologyAuthority, MariaDbTopologyAuthority, MySqlTopologyAuthority,
    PostgresTopologyAuthority, ProjectTopologyAuthority, RedisTopologyAuthority, discover_remote,
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
        mysql: MySqlTopologyAuthority::Authoritative,
        mariadb: MariaDbTopologyAuthority::Authoritative,
        redis: RedisTopologyAuthority::Authoritative,
        domains: DomainTopologyAuthority::Authoritative,
    }
}

fn insert(
    state: &mut StateFile,
    address: &str,
    kind: ResourceKind,
    remote_id: &str,
    inputs: serde_json::Value,
    containment: Option<&str>,
) {
    state
        .upsert_resource(
            address.parse().expect("address is valid"),
            ResourceState::new(
                kind,
                RemoteId::new(remote_id).expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(inputs).expect("managed inputs are valid"),
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
        serde_json::json!({}),
        None,
    );
    insert(
        &mut state,
        "environment.production",
        ResourceKind::Environment,
        "environment-1",
        serde_json::json!({}),
        Some("project.platform"),
    );
    state
}

fn config(mysql: &str) -> String {
    format!(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    mysql:
      main:
{mysql}
"#
    )
}

const PROJECT: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;

#[tokio::test]
async fn absent_mysql_requires_both_credentials_before_create() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ]);
    let client = server.client();
    let state = base_state(&server);
    let desired = compile(&config(
        r#"        database: app
        username: app
        password: null"#,
    ));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("MySQL state is projected");
    assert!(matches!(
        remote.observation(&"mysql.main".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));

    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);
    assert!(!plan.applyable());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::MissingCreateProperty
    );
    assert_eq!(
        plan.diagnostics()[0].property(),
        Some(&PropertyPath::RootPassword)
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].starts_with("GET /api/mysql.search?"));
}

#[tokio::test]
async fn managed_mysql_projects_readable_fields_and_two_write_only_credentials() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main","appName":"mysql-main","dockerImage":"mysql:8","databaseName":"app","databaseUser":"app","databasePassword":"user-canary","databaseRootPassword":"root-canary","env":"environment-canary"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "mysql.main",
        ResourceKind::MySql,
        "mysql-1",
        serde_json::json!({"database":"app","username":"app","password":null,"root_password":null}),
        Some("environment.production"),
    );
    let desired = compile(&config(
        r#"        database: app
        username: app
        password: null
        root_password: null"#,
    ));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("MySQL state is projected");
    let Some(RemoteObservation::Present(mysql)) =
        remote.observation(&"mysql.main".parse().unwrap())
    else {
        panic!("MySQL database should be present");
    };
    assert!(matches!(
        mysql.property(&PropertyPath::Database),
        Some(PropertyObservation::Known(_))
    ));
    assert!(matches!(
        mysql.property(&PropertyPath::Username),
        Some(PropertyObservation::Known(_))
    ));
    assert!(matches!(
        mysql.property(&PropertyPath::Password),
        Some(PropertyObservation::Unknown(
            PropertyUnknownReason::Sensitive
        ))
    ));
    assert!(matches!(
        mysql.property(&PropertyPath::RootPassword),
        Some(PropertyObservation::Unknown(
            PropertyUnknownReason::Sensitive
        ))
    ));
    let debug = format!("{remote:?}");
    assert!(!debug.contains("user-canary"));
    assert!(!debug.contains("root-canary"));
    assert!(!debug.contains("environment-canary"));

    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn mysql_metadata_changes_update_in_place() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main","appName":"mysql-main","dockerImage":"mysql:8","databaseName":"old","databaseUser":"old"}"#,
        ),
    ]);
    let client = server.client();
    let mut state = base_state(&server);
    insert(
        &mut state,
        "mysql.main",
        ResourceKind::MySql,
        "mysql-1",
        serde_json::json!({"database":"old","username":"old"}),
        Some("environment.production"),
    );
    let desired = compile(&config(
        r#"        database: next
        username: next"#,
    ));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("MySQL state is projected");
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let plan = plan(desired.desired_state(), &stored, &remote);

    assert!(plan.applyable());
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Update && change.address().to_string() == "mysql.main"
    }));
    server.finish();
}

#[tokio::test]
async fn mysql_password_clear_after_create_is_unsupported() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"mysqlId":"mysql-1","environmentId":"environment-1","name":"main","appName":"mysql-main","dockerImage":"mysql:8","databaseName":"app","databaseUser":"app"}"#,
        ),
    ]);
    let client = server.client();
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
    let mysql = ResourceState::try_new(
        ResourceKind::MySql,
        RemoteId::new("mysql-1").unwrap(),
        false,
        ManagedInputs::try_from_json(serde_json::json!({"database":"app","username":"app"}))
            .unwrap(),
        sensitive,
        Some("environment.production".parse().unwrap()),
        Vec::new(),
    )
    .unwrap();
    state
        .upsert_resource("mysql.main".parse().unwrap(), mysql)
        .unwrap();
    let desired = compile(&config(
        r#"        database: app
        username: app
        password: null
        root_password: null"#,
    ));

    let remote = discover_remote(&client, &desired, &state, authoritative())
        .await
        .expect("MySQL state is projected");
    let stored = StoredState::try_from_state(&state).expect("state projects");
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
async fn partial_mysql_search_cannot_prove_absence() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ]);
    let state = base_state(&server);
    let desired = compile(&config("        {}"));
    let authority = DiscoveryAuthority {
        mysql: MySqlTopologyAuthority::Partial,
        ..authoritative()
    };

    let remote = discover_remote(&server.client(), &desired, &state, authority)
        .await
        .expect("partial search projects conservatively");
    assert!(matches!(
        remote.observation(&"mysql.main".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}
