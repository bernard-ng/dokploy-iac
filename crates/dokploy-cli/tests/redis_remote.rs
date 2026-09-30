use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{
    ApplicationTopologyAuthority, DiscoverRemoteError, DiscoveryAuthority, DomainTopologyAuthority,
    EnvironmentTopologyAuthority, MariaDbTopologyAuthority, MongoTopologyAuthority,
    MySqlTopologyAuthority, PostgresTopologyAuthority, ProjectTopologyAuthority,
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

fn authoritative() -> DiscoveryAuthority {
    DiscoveryAuthority {
        projects: ProjectTopologyAuthority::Authoritative,
        environments: EnvironmentTopologyAuthority::Authoritative,
        applications: ApplicationTopologyAuthority::Authoritative,
        postgres: PostgresTopologyAuthority::Authoritative,
        mysql: MySqlTopologyAuthority::Authoritative,
        mariadb: MariaDbTopologyAuthority::Authoritative,
        mongo: MongoTopologyAuthority::Authoritative,
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

fn config(redis: &str) -> String {
    format!(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    redis:
      cache:
{redis}
"#
    )
}

const PROJECT: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;

#[tokio::test]
async fn planner_blocks_an_absent_redis_database_with_missing_create_inputs() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ]);
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let remote = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect("Redis state is projected");

    assert!(matches!(
        remote.observation(&"redis.cache".parse().unwrap()),
        Some(RemoteObservation::Missing)
    ));
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );
    assert!(plan.complete());
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert_eq!(plan.diagnostics().len(), 1);
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::MissingCreateProperty
    );
    assert_eq!(
        plan.diagnostics()[0].property(),
        Some(&PropertyPath::Password)
    );
    assert!(server.finish()[3].starts_with("GET /api/redis.search?"));
}

#[tokio::test]
async fn managed_redis_projects_only_write_only_password_without_secret_bytes() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"redisId":"redis-1","environmentId":"environment-1","name":"cache"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"redisId":"redis-1","environmentId":"environment-1","name":"cache","appName":"redis-cache","dockerImage":"redis:8","databasePassword":"password-canary","env":"SECRET=environment-canary"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "redis.cache",
        ResourceKind::Redis,
        "redis-1",
        serde_json::json!({"password":null}),
        &["environment.production"],
    );
    let desired = compile(&config("        password: null"));

    let remote = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect("Redis state is projected");
    let Some(RemoteObservation::Present(redis)) =
        remote.observation(&"redis.cache".parse().unwrap())
    else {
        panic!("Redis database should be present");
    };

    assert!(matches!(
        redis.property(&PropertyPath::Password),
        Some(PropertyObservation::Unknown(
            PropertyUnknownReason::Sensitive
        ))
    ));
    let debug = format!("{remote:?}");
    assert!(!debug.contains("password-canary"));
    assert!(!debug.contains("environment-canary"));
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );
    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().is_empty());
    server.finish();
}

#[tokio::test]
async fn unavailable_redis_collection_blocks_a_successful_direct_read_and_any_mutation() {
    let unavailable = r#"{"code":"INTERNAL_SERVER_ERROR","message":"retry"}"#;
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("500 Internal Server Error", unavailable),
        ("500 Internal Server Error", unavailable),
        ("500 Internal Server Error", unavailable),
        (
            "200 OK",
            r#"{"redisId":"redis-1","environmentId":"environment-1","name":"cache","appName":"redis-cache","dockerImage":"redis:8"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "redis.cache",
        ResourceKind::Redis,
        "redis-1",
        serde_json::json!({"password":null}),
        &["environment.production"],
    );
    let desired = compile(&config("        password: null"));

    let remote = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect("collection failure is projected conservatively");

    assert!(matches!(
        remote.observation(&"redis.cache".parse().unwrap()),
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
    let requests = server.finish();
    assert_eq!(requests.len(), 7);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[tokio::test]
async fn missing_managed_redis_exposes_replacement_without_adopting_it() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"redisId":"replacement-2","environmentId":"environment-1","name":"cache"}],"total":1}"#,
        ),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"missing"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "redis.cache",
        ResourceKind::Redis,
        "original-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(&config("        {}"));

    let remote = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect("replacement is projected");
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(plan.changes().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteIdentityMismatch
    );
    server.finish();
}

#[tokio::test]
async fn redis_authority_and_malformed_search_fail_closed() {
    for (search, authority, expected) in [
        (
            r#"{"items":[],"total":0}"#,
            RedisTopologyAuthority::Partial,
            RemoteFailureKind::InvalidResponse,
        ),
        (
            r#"{"items":[],"total":1}"#,
            RedisTopologyAuthority::Authoritative,
            RemoteFailureKind::InvalidResponse,
        ),
    ] {
        let server = TestServer::respond_in_sequence(vec![
            ("200 OK", PROJECT),
            ("200 OK", ENVIRONMENTS),
            ("200 OK", ENVIRONMENT),
            ("200 OK", search),
        ]);
        let state = base_state(&server);
        let desired = compile(&config("        {}"));
        let authority = DiscoveryAuthority {
            redis: authority,
            ..authoritative()
        };

        let remote = discover_remote(&server.client(), &desired, &state, authority)
            .await
            .expect("inconclusive search is projected");
        assert!(matches!(
            remote.observation(&"redis.cache".parse().unwrap()),
            Some(RemoteObservation::Unavailable(reason)) if *reason == expected
        ));
        server.finish();
    }
}

#[tokio::test]
async fn redis_collection_rejects_invalid_identity_containment_and_duplicate_names() {
    let cases = [
        (
            r#"{"items":[{"redisId":"","environmentId":"environment-1","name":"cache"}],"total":1}"#,
            "invalid-id",
        ),
        (
            r#"{"items":[{"redisId":"redis-1","environmentId":"other","name":"cache"}],"total":1}"#,
            "containment",
        ),
        (
            r#"{"items":[{"redisId":"redis-1","environmentId":"environment-1","name":"cache"},{"redisId":"redis-2","environmentId":"environment-1","name":"cache"}],"total":2}"#,
            "duplicate-name",
        ),
    ];

    for (search, expected) in cases {
        let server = TestServer::respond_in_sequence(vec![
            ("200 OK", PROJECT),
            ("200 OK", ENVIRONMENTS),
            ("200 OK", ENVIRONMENT),
            ("200 OK", search),
        ]);
        let state = base_state(&server);
        let desired = compile(&config("        {}"));

        let error = discover_remote(&server.client(), &desired, &state, authoritative())
            .await
            .expect_err("ambiguous Redis topology must fail closed");
        assert!(match expected {
            "invalid-id" => matches!(error, DiscoverRemoteError::InvalidRedisId),
            "containment" => matches!(error, DiscoverRemoteError::RedisContainment),
            "duplicate-name" => matches!(error, DiscoverRemoteError::DuplicateRedisName),
            _ => unreachable!(),
        });
        server.finish();
    }
}

#[tokio::test]
async fn redis_physical_reparent_is_rejected_before_any_redis_request() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"},{"environmentId":"environment-2","name":"staging"}]"#,
        ),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"environmentId":"environment-2","name":"staging","projectId":"project-1"}"#,
        ),
    ]);
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
        "redis.cache",
        ResourceKind::Redis,
        "redis-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(
        r#"
version: 1
project: { name: platform }
environments:
  production: {}
  staging:
    redis:
      cache: {}
"#,
    );

    let error = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect_err("Redis reparenting has no explicit remote action");

    assert!(matches!(
        error,
        DiscoverRemoteError::RedisReparentUnsupported
    ));
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn redis_move_and_state_backed_removal_preserve_managed_identity() {
    for yaml in [
        r#"
version: 1
project: { name: platform }
environments:
  production:
    redis:
      cache: {}
moves:
  - from: redis.legacy
    to: redis.cache
"#,
        r#"
version: 1
project: { name: platform }
environments:
  production: {}
removed:
  - from: redis.legacy
    destroy: true
"#,
    ] {
        let server = TestServer::respond_in_sequence(vec![
            ("200 OK", PROJECT),
            ("200 OK", ENVIRONMENTS),
            ("200 OK", ENVIRONMENT),
            (
                "200 OK",
                r#"{"items":[{"redisId":"redis-1","environmentId":"environment-1","name":"legacy"}],"total":1}"#,
            ),
            (
                "200 OK",
                r#"{"redisId":"redis-1","environmentId":"environment-1","name":"legacy","appName":"redis-legacy","dockerImage":"redis:8"}"#,
            ),
        ]);
        let mut state = base_state(&server);
        insert(
            &mut state,
            "redis.legacy",
            ResourceKind::Redis,
            "redis-1",
            serde_json::json!({}),
            &["environment.production"],
        );
        let desired = compile(yaml);

        let remote = discover_remote(&server.client(), &desired, &state, authoritative())
            .await
            .expect("stored Redis identity is projected");
        let plan = plan(
            desired.desired_state(),
            &StoredState::try_from_state(&state).unwrap(),
            &remote,
        );

        assert!(plan.diagnostics().is_empty());
        assert!(plan.changes().iter().any(|change| {
            matches!(change.kind(), ChangeKind::Move | ChangeKind::Delete)
                && matches!(
                    change.address().to_string().as_str(),
                    "redis.cache" | "redis.legacy"
                )
        }));
        server.finish();
    }
}

#[tokio::test]
async fn persisted_redis_removal_needs_no_parent_or_remote_probe() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
    ]);
    let state = base_state(&server);
    let desired = compile(
        r#"
version: 1
project: { name: platform }
environments:
  production: {}
removed:
  - from: redis.legacy
    destroy: true
"#,
    );

    let remote = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect("checkpointed removal is idempotent");
    assert!(
        remote
            .observation(&"redis.legacy".parse().unwrap())
            .is_none()
    );
    assert!(
        plan(
            desired.desired_state(),
            &StoredState::try_from_state(&state).unwrap(),
            &remote
        )
        .changes()
        .is_empty()
    );
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn redis_search_authentication_failure_is_unavailable() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "401 Unauthorized",
            r#"{"code":"UNAUTHORIZED","message":"denied"}"#,
        ),
    ]);
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    let remote = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect("authentication failure is projected");

    assert!(matches!(
        remote.observation(&"redis.cache".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unauthorized
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn environment_move_keeps_redis_on_the_same_physical_parent() {
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
            r#"{"items":[{"redisId":"redis-1","environmentId":"environment-1","name":"cache"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"redisId":"redis-1","environmentId":"environment-1","name":"cache","appName":"redis-cache","dockerImage":"redis:8"}"#,
        ),
    ]);
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
        "redis.cache",
        ResourceKind::Redis,
        "redis-1",
        serde_json::json!({}),
        &["environment.legacy"],
    );
    let desired = compile(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    redis:
      cache: {}
moves:
  - from: environment.legacy
    to: environment.production
"#,
    );

    let remote = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect("moved logical parent resolves to its stored physical identity");
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(plan.diagnostics().is_empty());
    assert!(plan.changes().iter().any(|change| {
        change.kind() == ChangeKind::Move
            && change.address().to_string() == "environment.production"
    }));
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn redis_direct_and_collection_endpoints_must_agree() {
    let cases = [
        (
            r#"{"items":[],"total":0}"#,
            r#"{"redisId":"redis-1","environmentId":"environment-1","name":"cache","appName":"redis-cache","dockerImage":"redis:8"}"#,
            "omitted",
        ),
        (
            r#"{"items":[{"redisId":"redis-1","environmentId":"environment-1","name":"cache"}],"total":1}"#,
            r#"{"redisId":"different-2","environmentId":"environment-1","name":"cache","appName":"redis-cache","dockerImage":"redis:8"}"#,
            "identity",
        ),
    ];

    for (search, direct, expected) in cases {
        let server = TestServer::respond_in_sequence(vec![
            ("200 OK", PROJECT),
            ("200 OK", ENVIRONMENTS),
            ("200 OK", ENVIRONMENT),
            ("200 OK", search),
            ("200 OK", direct),
        ]);
        let mut state = base_state(&server);
        insert(
            &mut state,
            "redis.cache",
            ResourceKind::Redis,
            "redis-1",
            serde_json::json!({}),
            &["environment.production"],
        );
        let desired = compile(&config("        {}"));

        let error = discover_remote(&server.client(), &desired, &state, authoritative())
            .await
            .expect_err("conflicting Redis endpoints must fail closed");
        assert!(match expected {
            "omitted" => matches!(error, DiscoverRemoteError::RedisTopologyConflict),
            "identity" => matches!(error, DiscoverRemoteError::InvalidRedisId),
            _ => unreachable!(),
        });
        server.finish();
    }
}

#[tokio::test]
async fn redis_direct_404_cannot_override_positive_collection_identity() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"items":[{"redisId":"redis-1","environmentId":"environment-1","name":"cache"}],"total":1}"#,
        ),
        (
            "404 Not Found",
            r#"{"code":"NOT_FOUND","message":"missing"}"#,
        ),
    ]);
    let mut state = base_state(&server);
    insert(
        &mut state,
        "redis.cache",
        ResourceKind::Redis,
        "redis-1",
        serde_json::json!({}),
        &["environment.production"],
    );
    let desired = compile(&config("        {}"));

    let remote = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect("the endpoint contradiction is represented conservatively");

    assert!(matches!(
        remote.observation(&"redis.cache".parse().unwrap()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );
    assert!(plan.changes().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteUnavailable
    );
    server.finish();
}

#[tokio::test]
async fn duplicate_redis_identity_across_environments_is_rejected() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        (
            "200 OK",
            r#"[{"environmentId":"environment-1","name":"production"},{"environmentId":"environment-2","name":"staging"}]"#,
        ),
        ("200 OK", ENVIRONMENT),
        (
            "200 OK",
            r#"{"environmentId":"environment-2","name":"staging","projectId":"project-1"}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"redisId":"duplicate-1","environmentId":"environment-1","name":"cache"}],"total":1}"#,
        ),
        (
            "200 OK",
            r#"{"items":[{"redisId":"duplicate-1","environmentId":"environment-2","name":"sessions"}],"total":1}"#,
        ),
    ]);
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
project: { name: platform }
environments:
  production:
    redis:
      cache: {}
  staging:
    redis:
      sessions: {}
"#,
    );

    let error = discover_remote(&server.client(), &desired, &state, authoritative())
        .await
        .expect_err("one Redis identity cannot occupy two environments");

    assert!(matches!(error, DiscoverRemoteError::DuplicateRedisId));
    assert_eq!(server.finish().len(), 6);
}

#[tokio::test]
async fn every_combined_discovery_invocation_reads_fresh_redis_state() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
        ("200 OK", PROJECT),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", r#"{"items":[],"total":0}"#),
    ]);
    let state = base_state(&server);
    let desired = compile(&config("        {}"));

    for _ in 0..2 {
        let remote = discover_remote(&server.client(), &desired, &state, authoritative())
            .await
            .expect("each invocation performs fresh reads");
        assert!(matches!(
            remote.observation(&"redis.cache".parse().unwrap()),
            Some(RemoteObservation::Missing)
        ));
    }

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /api/redis.search?"))
            .count(),
        2
    );
}
