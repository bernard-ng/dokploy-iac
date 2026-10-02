use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{
    DiscoverRemoteError, DiscoveryAuthority, SecurityTopologyAuthority, discover_remote,
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

const PROJECTS: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
const APPLICATIONS: &str = r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#;
const APPLICATION: &str = r#"{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api"}"#;
const ENTRY: &str = r#"{"securityId":"security-1","applicationId":"application-1","username":"admin","password":"remote-password-canary"}"#;
const ENTRIES: &str = r#"{"applicationId":"application-1","security":[{"securityId":"security-1","applicationId":"application-1","username":"admin","password":"remote-password-canary"}]}"#;
const EMPTY_ENTRIES: &str = r#"{"applicationId":"application-1","security":[]}"#;
const NOT_FOUND: &str = r#"{"message":"Security not found","code":"NOT_FOUND"}"#;

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

fn base_state(server: &TestServer, include_security: bool) -> StateFile {
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
    if include_security {
        insert(
            &mut state,
            "security.admin",
            ResourceKind::Security,
            "security-1",
            Some("application.api"),
            serde_json::json!({"username": "admin"}),
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

/// The password stays unmanaged so these offline tests need no key storage.
fn desired(username: &str) -> dokploy_cli::desired::CompiledDesired {
    compile_desired(
        &DokployConfig::parse(&format!(
            r#"
version: 1
project:
  name: platform
  environments:
    production:
      applications:
        api:
          security:
            admin:
              username: "{username}"
"#
        ))
        .unwrap(),
        ConfigDigest::parse("a".repeat(64)).unwrap(),
    )
    .unwrap()
}

fn discovery_responses(collection: &'static str) -> Vec<(&'static str, &'static str)> {
    vec![
        ("200 OK", PROJECTS),
        ("200 OK", ENVIRONMENTS),
        ("200 OK", ENVIRONMENT),
        ("200 OK", APPLICATIONS),
        ("200 OK", APPLICATION),
        ("200 OK", collection),
    ]
}

/// A managed direct read is `security.one` followed by the parent collection.
fn direct_read(
    mut responses: Vec<(&'static str, &'static str)>,
    direct: &'static str,
    collection: &'static str,
) -> Vec<(&'static str, &'static str)> {
    responses.push(("200 OK", direct));
    responses.push(("200 OK", collection));
    responses
}

fn address() -> ResourceAddress {
    "security.admin".parse().unwrap()
}

#[tokio::test]
async fn authoritative_parent_absence_plans_security_creation_only_when_a_password_is_declared() {
    let server = TestServer::respond_in_sequence(discovery_responses(EMPTY_ENTRIES));
    let state = base_state(&server, false);
    let desired = desired("admin");

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
        remote.observation(&address()),
        Some(RemoteObservation::Missing)
    ));
    // A basic-auth entry cannot be created without its password, so an
    // unmanaged password blocks creation instead of guessing one.
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert_eq!(server.finish().len(), 6);
}

#[tokio::test]
async fn partial_authority_downgrades_absence_to_an_incomplete_plan() {
    let server = TestServer::respond_in_sequence(discovery_responses(EMPTY_ENTRIES));
    let state = base_state(&server, false);
    let desired = desired("admin");
    let mut authority = DiscoveryAuthority::reconciliation();
    authority.security = SecurityTopologyAuthority::Partial;

    let remote = discover_remote(&server.client(), &desired, &state, authority)
        .await
        .unwrap();

    assert!(matches!(
        remote.observation(&address()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn unmanaged_entry_with_the_same_username_is_matched_by_collision_key() {
    let server = TestServer::respond_in_sequence(discovery_responses(ENTRIES));
    let state = base_state(&server, false);
    let desired = desired("admin");

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    let Some(RemoteObservation::Present(entry)) = remote.observation(&address()) else {
        panic!("the unmanaged entry should be observed")
    };
    assert_eq!(entry.remote_id().as_str(), "security-1");
    server.finish();
}

#[tokio::test]
async fn managed_entry_requires_agreement_and_never_retains_the_password() {
    let server =
        TestServer::respond_in_sequence(direct_read(discovery_responses(ENTRIES), ENTRY, ENTRIES));
    let state = base_state(&server, true);
    let desired = desired("admin");

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
    let Some(RemoteObservation::Present(entry)) = remote.observation(&address()) else {
        panic!("the entry should be present")
    };

    assert!(matches!(
        entry.property(&PropertyPath::Username),
        Some(PropertyObservation::Known(_))
    ));
    assert!(plan.changes().is_empty());
    let rendered = format!(
        "{remote:?} {plan:?} {}",
        String::from_utf8(plan.to_json_bytes()).unwrap()
    );
    assert!(!rendered.contains("remote-password-canary"));
    assert_eq!(server.finish().len(), 8);
}

#[tokio::test]
async fn security_one_404_proves_absence_only_with_authoritative_parent_absence() {
    let mut responses = discovery_responses(EMPTY_ENTRIES);
    responses.push(("404 Not Found", NOT_FOUND));
    let server = TestServer::respond_in_sequence(responses);
    let state = base_state(&server, true);
    let desired = desired("admin");

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        remote.observation(&address()),
        Some(RemoteObservation::Missing)
    ));
    server.finish();

    let mut responses = discovery_responses(EMPTY_ENTRIES);
    responses.push(("404 Not Found", NOT_FOUND));
    let server = TestServer::respond_in_sequence(responses);
    let state = base_state(&server, true);
    let mut authority = DiscoveryAuthority::reconciliation();
    authority.security = SecurityTopologyAuthority::Partial;

    let remote = discover_remote(&server.client(), &desired, &state, authority)
        .await
        .unwrap();

    assert!(matches!(
        remote.observation(&address()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn security_one_404_while_the_collection_lists_the_identity_is_not_absence() {
    let mut responses = discovery_responses(ENTRIES);
    responses.push(("404 Not Found", NOT_FOUND));
    let server = TestServer::respond_in_sequence(responses);
    let state = base_state(&server, true);
    let desired = desired("admin");

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        remote.observation(&address()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn username_change_plans_one_in_place_update() {
    let server =
        TestServer::respond_in_sequence(direct_read(discovery_responses(ENTRIES), ENTRY, ENTRIES));
    let state = base_state(&server, true);
    let desired = desired("root");

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
async fn duplicate_usernames_make_the_parent_collection_unavailable() {
    let duplicate = Box::leak(
        format!(
            r#"{{"applicationId":"application-1","security":[{ENTRY},{}]}}"#,
            ENTRY.replace("security-1", "security-2")
        )
        .into_boxed_str(),
    );
    let server = TestServer::respond_in_sequence(discovery_responses(duplicate));
    let state = base_state(&server, false);
    let desired = desired("admin");

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        remote.observation(&address()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}

#[tokio::test]
async fn direct_identity_under_another_application_fails_closed() {
    const FOREIGN: &str = r#"{"securityId":"security-1","applicationId":"application-2","username":"admin","password":"remote-password-canary"}"#;
    const FOREIGN_COLLECTION: &str = r#"{"applicationId":"application-2","security":[{"securityId":"security-1","applicationId":"application-2","username":"admin","password":"remote-password-canary"}]}"#;
    let server = TestServer::respond_in_sequence(direct_read(
        discovery_responses(ENTRIES),
        FOREIGN,
        FOREIGN_COLLECTION,
    ));
    let state = base_state(&server, true);
    let desired = desired("admin");

    let error = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("a direct identity under another application is contradictory");

    assert!(matches!(
        error,
        DiscoverRemoteError::SecurityTopologyConflict
    ));
    assert!(!format!("{error:?} {error}").contains("remote-password-canary"));
    server.finish();
}

#[tokio::test]
async fn collection_naming_another_application_is_unavailable_not_authoritative() {
    const WRONG_ROOT: &str = r#"{"applicationId":"application-2","security":[]}"#;
    let server = TestServer::respond_in_sequence(discovery_responses(WRONG_ROOT));
    let state = base_state(&server, false);
    let desired = desired("admin");

    let remote = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        remote.observation(&address()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    server.finish();
}
