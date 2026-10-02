use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{
    DiscoverRemoteError, DiscoveryAuthority, RedirectTopologyAuthority, discover_remote,
};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, PlanDiagnosticCode, PropertyObservation, PropertyPath,
    RemoteFailureKind, RemoteObservation, StoredState, plan,
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
const REDIRECT: &str = r#"{"redirectId":"redirect-1","applicationId":"application-1","regex":"^/old","replacement":"/new","permanent":true}"#;
const REDIRECTS: &str = r#"{"applicationId":"application-1","redirects":[{"redirectId":"redirect-1","applicationId":"application-1","regex":"^/old","replacement":"/new","permanent":true}]}"#;
const EMPTY_REDIRECTS: &str = r#"{"applicationId":"application-1","redirects":[]}"#;
const NOT_FOUND: &str = r#"{"message":"Redirect not found","code":"NOT_FOUND"}"#;

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

fn base_state(server: &TestServer, include_redirect: bool) -> StateFile {
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
    if include_redirect {
        insert(
            &mut state,
            "redirect.www",
            ResourceKind::Redirect,
            "redirect-1",
            Some("application.api"),
            serde_json::json!({
                "regex": "^/old",
                "replacement": "/new",
                "permanent": true
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

fn desired(replacement: &str) -> dokploy_cli::desired::CompiledDesired {
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
          redirects:
            www:
              regex: "^/old"
              replacement: "{replacement}"
              permanent: true
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

/// A managed direct read is `redirects.one` followed by the parent collection.
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
    "redirect.www".parse().unwrap()
}

#[tokio::test]
async fn authoritative_parent_absence_plans_redirect_creation() {
    let server = TestServer::respond_in_sequence(discovery_responses(EMPTY_REDIRECTS));
    let state = base_state(&server, false);
    let desired = desired("/new");

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
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
    assert_eq!(server.finish().len(), 6);
}

#[tokio::test]
async fn partial_authority_downgrades_absence_to_an_incomplete_plan() {
    let server = TestServer::respond_in_sequence(discovery_responses(EMPTY_REDIRECTS));
    let state = base_state(&server, false);
    let desired = desired("/new");
    let mut authority = DiscoveryAuthority::reconciliation();
    authority.redirects = RedirectTopologyAuthority::Partial;

    let remote = discover_remote(&server.client(), &desired, &state, authority)
        .await
        .unwrap();
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(matches!(
        remote.observation(&address()),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    assert!(!plan.complete());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|issue| issue.code() == PlanDiagnosticCode::RemoteUnavailable)
    );
    server.finish();
}

#[tokio::test]
async fn unmanaged_redirect_with_the_same_regex_is_matched_by_collision_key() {
    let server = TestServer::respond_in_sequence(discovery_responses(REDIRECTS));
    let state = base_state(&server, false);
    let desired = desired("/new");

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

    let Some(RemoteObservation::Present(redirect)) = remote.observation(&address()) else {
        panic!("the unmanaged Redirect should be observed")
    };
    assert_eq!(redirect.remote_id().as_str(), "redirect-1");
    assert!(
        !plan.applyable(),
        "an unmanaged collision must block creation"
    );
    server.finish();
}

#[tokio::test]
async fn managed_redirect_requires_direct_and_parent_collection_agreement() {
    let server = TestServer::respond_in_sequence(direct_read(
        discovery_responses(REDIRECTS),
        REDIRECT,
        REDIRECTS,
    ));
    let state = base_state(&server, true);
    let desired = desired("/new");

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
    let Some(RemoteObservation::Present(redirect)) = remote.observation(&address()) else {
        panic!("Redirect should be present")
    };

    for path in [
        PropertyPath::Regex,
        PropertyPath::Replacement,
        PropertyPath::Permanent,
    ] {
        assert!(matches!(
            redirect.property(&path),
            Some(PropertyObservation::Known(_))
        ));
    }
    assert!(plan.changes().is_empty());
    assert_eq!(server.finish().len(), 8);
}

#[tokio::test]
async fn redirect_one_404_proves_absence_only_with_authoritative_parent_absence() {
    let mut responses = discovery_responses(EMPTY_REDIRECTS);
    responses.push(("404 Not Found", NOT_FOUND));
    let server = TestServer::respond_in_sequence(responses);
    let state = base_state(&server, true);
    let desired = desired("/new");

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

    let mut responses = discovery_responses(EMPTY_REDIRECTS);
    responses.push(("404 Not Found", NOT_FOUND));
    let server = TestServer::respond_in_sequence(responses);
    let state = base_state(&server, true);
    let mut authority = DiscoveryAuthority::reconciliation();
    authority.redirects = RedirectTopologyAuthority::Partial;

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
async fn redirect_one_404_while_the_collection_lists_the_identity_is_not_absence() {
    let mut responses = discovery_responses(REDIRECTS);
    responses.push(("404 Not Found", NOT_FOUND));
    let server = TestServer::respond_in_sequence(responses);
    let state = base_state(&server, true);
    let desired = desired("/new");

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
async fn complete_field_change_plans_one_in_place_update() {
    let server = TestServer::respond_in_sequence(direct_read(
        discovery_responses(REDIRECTS),
        REDIRECT,
        REDIRECTS,
    ));
    let state = base_state(&server, true);
    let desired = desired("/newer");

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
async fn duplicate_parent_regular_expressions_fail_closed() {
    let duplicate = Box::leak(
        format!(
            r#"{{"applicationId":"application-1","redirects":[{REDIRECT},{}]}}"#,
            REDIRECT.replace("redirect-1", "redirect-2")
        )
        .into_boxed_str(),
    );
    let server = TestServer::respond_in_sequence(discovery_responses(duplicate));
    let state = base_state(&server, false);
    let desired = desired("/new");

    let error = discover_remote(
        &server.client(),
        &desired,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("duplicate collision key must fail closed");

    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateRedirectCollision
    ));
    server.finish();
}

#[tokio::test]
async fn direct_identity_under_another_application_fails_closed() {
    const FOREIGN: &str = r#"{"redirectId":"redirect-1","applicationId":"application-2","regex":"^/old","replacement":"/new","permanent":true}"#;
    const FOREIGN_COLLECTION: &str = r#"{"applicationId":"application-2","redirects":[{"redirectId":"redirect-1","applicationId":"application-2","regex":"^/old","replacement":"/new","permanent":true}]}"#;
    let server = TestServer::respond_in_sequence(direct_read(
        discovery_responses(REDIRECTS),
        FOREIGN,
        FOREIGN_COLLECTION,
    ));
    let state = base_state(&server, true);
    let desired = desired("/new");

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
        DiscoverRemoteError::RedirectTopologyConflict
    ));
    server.finish();
}

#[tokio::test]
async fn collection_naming_another_application_is_unavailable_not_authoritative() {
    const WRONG_ROOT: &str = r#"{"applicationId":"application-2","redirects":[]}"#;
    let server = TestServer::respond_in_sequence(discovery_responses(WRONG_ROOT));
    let state = base_state(&server, false);
    let desired = desired("/new");

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
async fn direct_and_collection_records_that_disagree_are_unavailable() {
    const DRIFTED_COLLECTION: &str = r#"{"applicationId":"application-1","redirects":[{"redirectId":"redirect-1","applicationId":"application-1","regex":"^/old","replacement":"/different","permanent":true}]}"#;
    let server = TestServer::respond_in_sequence(direct_read(
        discovery_responses(DRIFTED_COLLECTION),
        REDIRECT,
        DRIFTED_COLLECTION,
    ));
    let state = base_state(&server, true);
    let desired = desired("/new");

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
