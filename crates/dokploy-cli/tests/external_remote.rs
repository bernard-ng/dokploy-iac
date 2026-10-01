use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{DiscoveryAuthority, discover_remote};
use dokploy_cli::saved_plan::{SavedPlan, SavedPlanError};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, ExternalResolution, ExternalSelectorFailure, Plan,
    PlanDiagnosticCode, PropertyPath, RemoteState, StoredState, plan,
};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateFile,
};

const SECRET_CANARY: &str = "external-secret-canary-4f9a";
const NAME_CANARY: &str = "selector-name-canary-91bc";

const PROJECT: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
const NO_APPLICATIONS: &str = r#"{"items":[],"total":0}"#;
const ONE_APPLICATION: &str = r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_in_sequence(responses: Vec<(&'static str, String)>) -> Self {
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

    fn instance(&self) -> InstanceIdentity {
        InstanceIdentity::parse(&self.url).expect("server URL is an instance")
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

fn config(application: &str) -> String {
    format!(
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      api:\n{application}\n"
    )
}

fn state(server: &TestServer, application: Option<serde_json::Value>) -> StateFile {
    let mut state = StateFile::new(
        "0.1.0".parse().expect("version is valid"),
        server.instance(),
    );
    let project: ResourceAddress = "project.platform".parse().unwrap();
    let environment: ResourceAddress = "environment.production".parse().unwrap();
    state
        .upsert_resource(
            project.clone(),
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new("project-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                None,
                Vec::new(),
            ),
        )
        .unwrap();
    state
        .upsert_resource(
            environment.clone(),
            ResourceState::new(
                ResourceKind::Environment,
                RemoteId::new("environment-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                Some(project.clone()),
                vec![],
            ),
        )
        .unwrap();
    if let Some(inputs) = application {
        state
            .upsert_resource(
                "application.api".parse().unwrap(),
                ResourceState::new(
                    ResourceKind::Application,
                    RemoteId::new("application-1").unwrap(),
                    false,
                    ManagedInputs::try_from_json(inputs).unwrap(),
                    Some(environment),
                    vec![],
                ),
            )
            .unwrap();
    }
    state
}

fn servers(rows: &[(&str, &str)]) -> String {
    let rows: Vec<_> = rows
        .iter()
        .map(|(id, name)| {
            format!(
                r#"{{"serverId":"{id}","name":"{name}","serverType":"deploy","command":"curl {SECRET_CANARY}","metricsConfig":{{"token":"{SECRET_CANARY}"}}}}"#
            )
        })
        .collect();
    format!("[{}]", rows.join(","))
}

fn registries(rows: &[(&str, &str)]) -> String {
    let rows: Vec<_> = rows
        .iter()
        .map(|(id, name)| {
            format!(
                r#"{{"registryId":"{id}","registryName":"{name}","username":"u","password":"{SECRET_CANARY}","registryUrl":"https://registry.example.test"}}"#
            )
        })
        .collect();
    format!("[{}]", rows.join(","))
}

fn application_one(extra: &str) -> String {
    format!(
        r#"{{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api"{extra}}}"#
    )
}

struct Discovery {
    remote: RemoteState,
    plan: Plan,
    requests: Vec<String>,
    instance: InstanceIdentity,
}

async fn discover(
    yaml: &str,
    stored: Option<serde_json::Value>,
    tail: Vec<(&'static str, String)>,
) -> Discovery {
    discover_many(yaml, stored, vec![tail])
        .await
        .pop()
        .expect("one discovery was requested")
}

/// Runs consecutive discoveries against one server, state, and lineage.
async fn discover_many(
    yaml: &str,
    stored: Option<serde_json::Value>,
    tails: Vec<Vec<(&'static str, String)>>,
) -> Vec<Discovery> {
    let mut responses = Vec::new();
    for tail in &tails {
        responses.extend([
            ("200 OK", PROJECT.to_owned()),
            ("200 OK", ENVIRONMENTS.to_owned()),
            ("200 OK", ENVIRONMENT.to_owned()),
        ]);
        responses.extend(tail.iter().cloned());
    }
    let server = TestServer::respond_in_sequence(responses);
    let client = server.client();
    let state = state(&server, stored);
    let compiled = compile(yaml);
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let instance = server.instance();
    let mut discoveries = Vec::new();
    for _ in &tails {
        let remote = discover_remote(
            &client,
            &compiled,
            &state,
            DiscoveryAuthority::reconciliation(),
        )
        .await
        .expect("discovery succeeds");
        let plan = plan(compiled.desired_state(), &stored, &remote);
        discoveries.push(Discovery {
            remote,
            plan,
            requests: Vec::new(),
            instance: instance.clone(),
        });
    }
    let requests = server.finish();
    if let Some(last) = discoveries.last_mut() {
        last.requests = requests;
    }

    discoveries
}

fn api() -> ResourceAddress {
    "application.api".parse().unwrap()
}

fn assert_no_leaks(text: &str) {
    for canary in [
        SECRET_CANARY,
        NAME_CANARY,
        "server-1",
        "server-2",
        "registry-1",
        "registry-2",
        "registry-old",
    ] {
        assert!(!text.contains(canary), "{canary} leaked");
    }
}

#[tokio::test]
async fn named_associations_resolve_and_a_stable_plan_has_no_changes() {
    let discovery = discover(
        &config("        server:\n          name: edge-1\n        registry:\n          name: main"),
        Some(serde_json::json!({
            "server": {"name": "edge-1"},
            "registry": {"name": "main"}
        })),
        vec![
            (
                "200 OK",
                servers(&[("server-1", "edge-1"), ("server-2", "edge-2")]),
            ),
            ("200 OK", registries(&[("registry-1", "main")])),
            ("200 OK", ONE_APPLICATION.to_owned()),
            (
                "200 OK",
                application_one(r#","serverId":"server-1","registryId":"registry-1""#),
            ),
        ],
    )
    .await;

    assert_eq!(
        discovery
            .remote
            .external_resolution(&api(), &PropertyPath::Server),
        Some(&ExternalResolution::Resolved(
            RemoteId::new("server-1").unwrap()
        ))
    );
    assert_eq!(
        discovery
            .remote
            .external_resolution(&api(), &PropertyPath::Registry),
        Some(&ExternalResolution::Resolved(
            RemoteId::new("registry-1").unwrap()
        ))
    );
    assert!(
        discovery.plan.applyable(),
        "{:?}",
        discovery.plan.diagnostics()
    );
    assert!(discovery.plan.changes().is_empty());
    assert!(discovery.plan.drift().is_empty());

    assert!(discovery.requests[3].starts_with("GET /api/server.all HTTP/1.1\r\n"));
    assert!(discovery.requests[4].starts_with("GET /api/registry.all HTTP/1.1\r\n"));
    assert!(discovery.requests[5].starts_with("GET /api/application.search?"));
    assert!(discovery.requests[6].starts_with("GET /api/application.one?"));
    assert_eq!(discovery.requests.len(), 7);

    let debug = format!("{:?}", discovery.remote);
    assert_no_leaks(&debug);
    assert_no_leaks(&String::from_utf8(discovery.plan.to_json_bytes()).unwrap());
}

#[tokio::test]
async fn local_selector_resolves_without_an_identity_and_matches_a_null_server() {
    let discovery = discover(
        &config("        server:\n          local: true"),
        Some(serde_json::json!({"server": {"local": true}})),
        vec![
            ("200 OK", servers(&[("server-1", "edge-1")])),
            ("200 OK", ONE_APPLICATION.to_owned()),
            ("200 OK", application_one(r#","serverId":null"#)),
        ],
    )
    .await;

    assert_eq!(
        discovery
            .remote
            .external_resolution(&api(), &PropertyPath::Server),
        Some(&ExternalResolution::Local)
    );
    assert!(
        discovery.plan.applyable(),
        "{:?}",
        discovery.plan.diagnostics()
    );
    assert!(discovery.plan.changes().is_empty());
}

#[tokio::test]
async fn zero_matches_block_a_create_with_a_value_free_typed_diagnostic() {
    let discovery = discover(
        &config(&format!("        registry:\n          name: {NAME_CANARY}")),
        None,
        vec![
            ("200 OK", registries(&[("registry-1", "main")])),
            ("200 OK", NO_APPLICATIONS.to_owned()),
        ],
    )
    .await;

    assert_eq!(
        discovery
            .remote
            .external_resolution(&api(), &PropertyPath::Registry),
        Some(&ExternalResolution::Unmatched)
    );
    assert!(!discovery.plan.applyable());
    assert!(discovery.plan.changes().is_empty());
    let diagnostic = &discovery.plan.diagnostics()[0];
    assert_eq!(
        diagnostic.code(),
        PlanDiagnosticCode::UnresolvedExternalSelector
    );
    assert_eq!(
        diagnostic.selector_failure(),
        Some(ExternalSelectorFailure::Unmatched)
    );
    assert_eq!(diagnostic.property(), Some(&PropertyPath::Registry));
    assert_no_leaks(&String::from_utf8(discovery.plan.to_json_bytes()).unwrap());
}

#[tokio::test]
async fn duplicate_names_are_ambiguous_and_never_selected_by_response_order() {
    for rows in [
        vec![("registry-1", "main"), ("registry-2", "main")],
        vec![("registry-2", "main"), ("registry-1", "main")],
    ] {
        let discovery = discover(
            &config("        registry:\n          name: main"),
            None,
            vec![
                ("200 OK", registries(&rows)),
                ("200 OK", NO_APPLICATIONS.to_owned()),
            ],
        )
        .await;

        assert_eq!(
            discovery
                .remote
                .external_resolution(&api(), &PropertyPath::Registry),
            Some(&ExternalResolution::Ambiguous)
        );
        assert!(!discovery.plan.applyable());
        assert_eq!(
            discovery.plan.diagnostics()[0].selector_failure(),
            Some(ExternalSelectorFailure::Ambiguous)
        );
    }
}

#[tokio::test]
async fn matching_is_exact_and_case_sensitive() {
    let discovery = discover(
        &config("        registry:\n          name: Main"),
        None,
        vec![
            (
                "200 OK",
                registries(&[("registry-1", "main"), ("registry-2", "MAIN")]),
            ),
            ("200 OK", NO_APPLICATIONS.to_owned()),
        ],
    )
    .await;

    assert_eq!(
        discovery
            .remote
            .external_resolution(&api(), &PropertyPath::Registry),
        Some(&ExternalResolution::Unmatched)
    );
}

#[tokio::test]
async fn an_unreadable_collection_blocks_without_leaking_its_error_body() {
    for (status, expected) in [
        ("403 Forbidden", ExternalSelectorFailure::Unavailable),
        (
            "500 Internal Server Error",
            ExternalSelectorFailure::Unavailable,
        ),
    ] {
        let discovery = discover(
            &config("        registry:\n          name: main"),
            None,
            vec![
                (status, format!(r#"{{"message":"{SECRET_CANARY}"}}"#)),
                ("200 OK", NO_APPLICATIONS.to_owned()),
            ],
        )
        .await;

        assert!(!discovery.plan.applyable());
        assert_eq!(
            discovery.plan.diagnostics()[0].selector_failure(),
            Some(expected)
        );
        assert_no_leaks(&format!("{:?}", discovery.remote));
        assert_no_leaks(&String::from_utf8(discovery.plan.to_json_bytes()).unwrap());
    }
}

#[tokio::test]
async fn duplicate_physical_ids_make_the_collection_unusable() {
    let discovery = discover(
        &config("        registry:\n          name: main"),
        None,
        vec![
            (
                "200 OK",
                registries(&[("registry-1", "main"), ("registry-1", "other")]),
            ),
            ("200 OK", NO_APPLICATIONS.to_owned()),
        ],
    )
    .await;

    assert_eq!(
        discovery
            .remote
            .external_resolution(&api(), &PropertyPath::Registry),
        Some(&ExternalResolution::Unavailable(
            dokploy_core::RemoteFailureKind::InvalidResponse
        ))
    );
    assert!(!discovery.plan.applyable());
}

#[tokio::test]
async fn changed_registry_updates_in_place_and_changed_server_replaces() {
    let discovery = discover(
        &config("        server:\n          name: edge-2\n        registry:\n          name: main"),
        Some(serde_json::json!({
            "server": {"name": "edge-1"},
            "registry": {"name": "registry-old"}
        })),
        vec![
            (
                "200 OK",
                servers(&[("server-1", "edge-1"), ("server-2", "edge-2")]),
            ),
            (
                "200 OK",
                registries(&[("registry-1", "main"), ("registry-old", "registry-old")]),
            ),
            ("200 OK", ONE_APPLICATION.to_owned()),
            (
                "200 OK",
                application_one(r#","serverId":"server-1","registryId":"registry-old""#),
            ),
        ],
    )
    .await;

    assert!(
        discovery.plan.applyable(),
        "{:?}",
        discovery.plan.diagnostics()
    );
    assert_eq!(discovery.plan.changes().len(), 1);
    assert_eq!(discovery.plan.changes()[0].kind(), ChangeKind::Replace);

    let registry_only = discover(
        &config("        server:\n          name: edge-1\n        registry:\n          name: main"),
        Some(serde_json::json!({
            "server": {"name": "edge-1"},
            "registry": {"name": "registry-old"}
        })),
        vec![
            ("200 OK", servers(&[("server-1", "edge-1")])),
            (
                "200 OK",
                registries(&[("registry-1", "main"), ("registry-old", "registry-old")]),
            ),
            ("200 OK", ONE_APPLICATION.to_owned()),
            (
                "200 OK",
                application_one(r#","serverId":"server-1","registryId":"registry-old""#),
            ),
        ],
    )
    .await;
    assert!(registry_only.plan.applyable());
    assert_eq!(registry_only.plan.changes()[0].kind(), ChangeKind::Update);
}

#[tokio::test]
async fn cleared_associations_observe_absence_and_need_no_resolution() {
    let discovery = discover(
        &config("        build_registry: null\n        build_server: null"),
        Some(serde_json::json!({"build_registry": null, "build_server": null})),
        vec![
            ("200 OK", ONE_APPLICATION.to_owned()),
            (
                "200 OK",
                application_one(r#","buildServerId":null,"buildRegistryId":null"#),
            ),
        ],
    )
    .await;

    assert!(
        discovery.plan.applyable(),
        "{:?}",
        discovery.plan.diagnostics()
    );
    assert!(discovery.plan.changes().is_empty());
    assert!(
        discovery
            .remote
            .external_resolution(&api(), &PropertyPath::BuildRegistry)
            .is_none()
    );
}

#[tokio::test]
async fn an_attached_identity_missing_from_the_collection_blocks_planning() {
    let discovery = discover(
        &config("        registry:\n          name: main"),
        Some(serde_json::json!({"registry": {"name": "main"}})),
        vec![
            ("200 OK", registries(&[("registry-1", "main")])),
            ("200 OK", ONE_APPLICATION.to_owned()),
            (
                "200 OK",
                application_one(r#","registryId":"registry-gone""#),
            ),
        ],
    )
    .await;

    assert!(!discovery.plan.applyable());
    assert_eq!(
        discovery.plan.diagnostics()[0].code(),
        PlanDiagnosticCode::UnknownPropertyObservation
    );
}

#[tokio::test]
async fn ignored_selectors_perform_no_external_reads() {
    let discovery = discover(
        &config(
            "        registry:\n          name: main\n        lifecycle:\n          ignore_changes: [registry]",
        ),
        Some(serde_json::json!({"registry": {"name": "main"}})),
        vec![
            ("200 OK", ONE_APPLICATION.to_owned()),
            ("200 OK", application_one(r#","registryId":"registry-1""#)),
        ],
    )
    .await;

    assert!(
        discovery.plan.applyable(),
        "{:?}",
        discovery.plan.diagnostics()
    );
    assert_eq!(discovery.requests.len(), 5);
    assert!(
        discovery
            .remote
            .external_resolution(&api(), &PropertyPath::Registry)
            .is_none()
    );
}

fn create_tail(rows: String) -> Vec<(&'static str, String)> {
    vec![("200 OK", rows), ("200 OK", NO_APPLICATIONS.to_owned())]
}

#[tokio::test]
async fn saved_plans_bind_resolved_identities_and_refuse_a_changed_resolution() {
    let mut discoveries = discover_many(
        &config("        registry:\n          name: main"),
        None,
        vec![
            create_tail(registries(&[("registry-1", "main")])),
            create_tail(registries(&[("registry-1", "main")])),
            create_tail(registries(&[("registry-2", "main")])),
        ],
    )
    .await;
    let recreated = discoveries.pop().unwrap();
    let same = discoveries.pop().unwrap();
    let first = discoveries.pop().unwrap();
    assert!(first.plan.applyable());
    assert_eq!(first.plan.to_json_bytes(), recreated.plan.to_json_bytes());

    let key = [9_u8; 32];
    let receipt = first.remote.binding_receipt(&key);
    let saved = SavedPlan::from_fresh_plan(first.instance.clone(), &first.plan, receipt)
        .expect("applyable plans can be saved");

    saved
        .verify_fresh(
            &same.instance,
            &same.plan,
            same.remote.binding_receipt(&key),
        )
        .expect("an unchanged resolution still verifies");
    let refused = saved
        .verify_fresh(
            &recreated.instance,
            &recreated.plan,
            recreated.remote.binding_receipt(&key),
        )
        .expect_err("a re-created external record must invalidate the saved plan");
    assert!(matches!(refused, SavedPlanError::StaleEvidence));

    let workspace = tempfile::tempdir().expect("temporary directory");
    let path = workspace.path().join("saved.plan.json");
    dokploy_cli::saved_plan::write_new(&path, &saved).expect("saved plan is written");
    let envelope = std::fs::read_to_string(&path).expect("saved plan is readable");
    assert_no_leaks(&envelope);
    assert_no_leaks(&format!("{saved:?}"));
}

#[tokio::test]
async fn removed_renamed_or_duplicated_records_make_a_saved_plan_unusable() {
    let key = [3_u8; 32];
    let mut discoveries = discover_many(
        &config("        registry:\n          name: main"),
        None,
        vec![
            create_tail(registries(&[("registry-1", "main")])),
            create_tail(registries(&[])),
            create_tail(registries(&[("registry-1", "renamed")])),
            create_tail(registries(&[
                ("registry-1", "main"),
                ("registry-2", "main"),
            ])),
        ],
    )
    .await;
    let baseline = discoveries.remove(0);
    let saved = SavedPlan::from_fresh_plan(
        baseline.instance.clone(),
        &baseline.plan,
        baseline.remote.binding_receipt(&key),
    )
    .expect("saved");

    assert_eq!(discoveries.len(), 3);
    for fresh in discoveries {
        assert!(!fresh.plan.applyable());
        assert!(matches!(
            SavedPlan::from_fresh_plan(
                fresh.instance.clone(),
                &fresh.plan,
                fresh.remote.binding_receipt(&key)
            ),
            Err(SavedPlanError::PlanBlocked)
        ));
        assert!(matches!(
            saved.verify_fresh(
                &fresh.instance,
                &fresh.plan,
                fresh.remote.binding_receipt(&key)
            ),
            Err(SavedPlanError::StaleEvidence)
        ));
    }
}
