use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::executor::{ApplyWorkspaceError, apply_workspace};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, RecoveryStatus, RemoteId, ResourceAddress,
    ResourceKind, ResourceState, StateFile, StateStore,
};

const SECRET_CANARY: &str = "external-secret-canary-77de";

const PROJECT: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
const NO_APPLICATIONS: &str = r#"{"items":[],"total":0}"#;
const API_SEARCH: &str = r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#;
const OK: &str = r#"{"ok":true}"#;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_in_sequence(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received = Vec::new();
            for body in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 1024];
                let mut expected = None;
                loop {
                    let count = stream.read(&mut buffer).expect("request is readable");
                    bytes.extend_from_slice(&buffer[..count]);
                    if count == 0 {
                        break;
                    }
                    if expected.is_none()
                        && let Some(position) = bytes.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        let head = String::from_utf8_lossy(&bytes[..position]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        expected = Some(position + 4 + length);
                    }
                    if expected.is_some_and(|total| bytes.len() >= total) {
                        break;
                    }
                }
                received.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }
            sender.send(received).expect("test receives requests");
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

fn reads() -> Vec<String> {
    vec![
        PROJECT.to_owned(),
        ENVIRONMENTS.to_owned(),
        ENVIRONMENT.to_owned(),
    ]
}

fn servers(rows: &[(&str, &str)]) -> String {
    let rows: Vec<_> = rows
        .iter()
        .map(|(id, name)| {
            format!(
                r#"{{"serverId":"{id}","name":"{name}","serverType":"deploy","command":"curl {SECRET_CANARY}"}}"#
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
                r#"{{"registryId":"{id}","registryName":"{name}","password":"{SECRET_CANARY}"}}"#
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

/// Writes the configuration and a durable state with project, environment, and optional resources.
fn workspace(
    server: &TestServer,
    yaml: &str,
    resources: Vec<(&str, ResourceKind, &str, serde_json::Value, Vec<&str>)>,
) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config = directory.path().join("dokploy.yaml");
    fs::write(&config, yaml).expect("configuration is writable");
    let instance = server.instance();
    let store = StateStore::new(directory.path(), instance.clone()).expect("state store");
    let mut session = store.begin_write().expect("state lock");
    let project: ResourceAddress = "project.platform".parse().unwrap();
    let environment: ResourceAddress = "environment.production".parse().unwrap();
    let mut states = std::collections::BTreeMap::new();
    states.insert(
        project.clone(),
        resource(
            ResourceKind::Project,
            "project-1",
            serde_json::json!({}),
            None,
            vec![],
        ),
    );
    states.insert(
        environment.clone(),
        resource(
            ResourceKind::Environment,
            "environment-1",
            serde_json::json!({}),
            Some(project),
            vec![],
        ),
    );
    for (address, kind, remote_id, inputs, dependencies) in resources {
        states.insert(
            address.parse().unwrap(),
            resource(
                kind,
                remote_id,
                inputs,
                Some(environment.clone()),
                dependencies.iter().map(|d| d.parse().unwrap()).collect(),
            ),
        );
    }
    let state = StateFile::new_with_resources("0.1.0".parse().unwrap(), instance, states)
        .expect("state is valid");
    session
        .checkpoint(ExpectedState::absent(), &state)
        .expect("state is persisted");
    drop(session);

    (directory, config)
}

fn resource(
    kind: ResourceKind,
    remote_id: &str,
    inputs: serde_json::Value,
    containment: Option<ResourceAddress>,
    dependencies: Vec<ResourceAddress>,
) -> ResourceState {
    ResourceState::new(
        kind,
        RemoteId::new(remote_id).unwrap(),
        false,
        ManagedInputs::try_from_json(inputs).unwrap(),
        containment,
        dependencies,
    )
}

fn config(application: &str) -> String {
    let application = format!("  {}", application.replace('\n', "\n  "));
    format!(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api:\n{application}\n"
    )
}

fn durable_text(directory: &Path) -> String {
    let mut text = String::new();
    for entry in walk(&directory.join(".dokploy")) {
        if let Ok(contents) = fs::read_to_string(&entry) {
            text.push_str(&contents);
        }
    }
    text
}

fn walk(path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}

fn assert_no_external_material(text: &str) {
    for canary in [
        SECRET_CANARY,
        "server-1",
        "server-2",
        "registry-1",
        "registry-2",
        "registry-old",
        "builder-1",
    ] {
        assert!(!text.contains(canary), "{canary} leaked");
    }
}

#[tokio::test]
async fn create_sends_the_resolved_server_at_create_and_associations_by_update_without_deploying() {
    let mut responses = reads();
    responses.extend([
        servers(&[("server-1", "edge-1"), ("server-2", "builder")]),
        registries(&[("registry-1", "main")]),
        NO_APPLICATIONS.to_owned(),
        r#"{"applicationId":"application-9","serverId":"server-1"}"#.to_owned(),
        OK.to_owned(),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let (directory, config_file) = workspace(
        &server,
        &config(
            "        server:\n          name: edge-1\n        build_server:\n          name: builder\n        registry:\n          name: main",
        ),
        vec![],
    );

    let summary = apply_workspace(&server.client(), &config_file)
        .await
        .expect("application with associations is created");

    assert_eq!(summary.applied(), 1);
    let instance = server.instance();
    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert!(requests[3].starts_with("GET /api/server.all HTTP/1.1\r\n"));
    assert!(requests[4].starts_with("GET /api/registry.all HTTP/1.1\r\n"));
    assert!(requests[6].starts_with("POST /api/application.create HTTP/1.1\r\n"));
    assert!(requests[6].contains(r#""serverId":"server-1""#));
    assert!(requests[7].starts_with("POST /api/application.update HTTP/1.1\r\n"));
    assert!(requests[7].contains(r#""registryId":"registry-1""#));
    assert!(requests[7].contains(r#""buildServerId":"server-2""#));
    assert!(!requests[7].contains(r#""serverId""#));
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("application.deploy")),
        "associations never trigger a deployment"
    );

    let durable = durable_text(directory.path());
    assert!(durable.contains(r#""name": "edge-1""#) || durable.contains(r#""name":"edge-1""#));
    assert_no_external_material(&durable);
    let store = StateStore::new(directory.path(), instance).expect("state store");
    assert_eq!(
        store.recovery_status().expect("journal scan"),
        RecoveryStatus::Clean
    );
}

#[tokio::test]
async fn a_local_server_selector_is_created_with_an_explicit_null_placement() {
    let mut responses = reads();
    responses.extend([
        servers(&[("server-1", "edge-1")]),
        NO_APPLICATIONS.to_owned(),
        r#"{"applicationId":"application-9","serverId":null}"#.to_owned(),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let (directory, config_file) = workspace(
        &server,
        &config("        server:\n          local: true"),
        vec![],
    );

    apply_workspace(&server.client(), &config_file)
        .await
        .expect("local placement is created");

    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert!(requests[5].starts_with("POST /api/application.create HTTP/1.1\r\n"));
    assert!(requests[5].contains(r#""serverId":null"#));
    assert_no_external_material(&durable_text(directory.path()));
}

#[tokio::test]
async fn changing_a_registry_updates_in_place_by_resolved_identity() {
    let mut responses = reads();
    responses.extend([
        registries(&[("registry-1", "main"), ("registry-old", "legacy")]),
        API_SEARCH.to_owned(),
        application_one(r#","registryId":"registry-old""#),
        OK.to_owned(),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let (directory, config_file) = workspace(
        &server,
        &config("        registry:\n          name: main"),
        vec![(
            "application.api",
            ResourceKind::Application,
            "application-1",
            serde_json::json!({"registry": {"name": "legacy"}}),
            vec![],
        )],
    );

    let summary = apply_workspace(&server.client(), &config_file)
        .await
        .expect("registry association updates in place");

    assert_eq!(summary.applied(), 1);
    let requests = server.finish();
    assert_eq!(requests.len(), 7);
    assert!(requests[6].starts_with("POST /api/application.update HTTP/1.1\r\n"));
    assert!(requests[6].contains(r#""registryId":"registry-1""#));
    assert!(requests[6].contains(r#""applicationId":"application-1""#));
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("application.deploy")
                && !request.contains("application.remove"))
    );
    let durable = durable_text(directory.path());
    assert!(durable.contains("main"));
    assert_no_external_material(&durable);
}

#[tokio::test]
async fn clearing_a_registry_sends_an_explicit_null() {
    let mut responses = reads();
    responses.extend([
        registries(&[("registry-1", "main")]),
        API_SEARCH.to_owned(),
        application_one(r#","registryId":"registry-1","buildRegistryId":null"#),
        OK.to_owned(),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let (_directory, config_file) = workspace(
        &server,
        &config("        registry: null\n        build_registry:\n          name: main"),
        vec![(
            "application.api",
            ResourceKind::Application,
            "application-1",
            serde_json::json!({"registry": {"name": "main"}, "build_registry": null}),
            vec![],
        )],
    );

    apply_workspace(&server.client(), &config_file)
        .await
        .expect("registry clears in place");

    let requests = server.finish();
    assert_eq!(requests.len(), 7);
    assert!(requests[6].starts_with("POST /api/application.update HTTP/1.1\r\n"));
    assert!(requests[6].contains(r#""registryId":null"#));
    assert!(requests[6].contains(r#""buildRegistryId":"registry-1""#));
}

#[tokio::test]
async fn changing_the_create_only_server_replaces_the_application_delete_before_create() {
    let mut responses = reads();
    responses.extend([
        servers(&[("server-1", "edge-1"), ("server-2", "edge-2")]),
        API_SEARCH.to_owned(),
        application_one(r#","serverId":"server-1""#),
        OK.to_owned(),
        r#"{"applicationId":"application-2","serverId":"server-2"}"#.to_owned(),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let (directory, config_file) = workspace(
        &server,
        &config("        server:\n          name: edge-2"),
        vec![(
            "application.api",
            ResourceKind::Application,
            "application-1",
            serde_json::json!({"server": {"name": "edge-1"}}),
            vec![],
        )],
    );

    let summary = apply_workspace(&server.client(), &config_file)
        .await
        .expect("placement change replaces the application");

    assert_eq!(summary.applied(), 1);
    let instance = server.instance();
    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert!(requests[6].starts_with("POST /api/application.delete HTTP/1.1\r\n"));
    assert!(requests[6].contains(r#""applicationId":"application-1""#));
    assert!(requests[7].starts_with("POST /api/application.create HTTP/1.1\r\n"));
    assert!(requests[7].contains(r#""serverId":"server-2""#));
    assert!(requests.iter().all(|request| !request.contains("deploy")));

    let store = StateStore::new(directory.path(), instance).expect("state store");
    let state = store.inspect().expect("state reads").expect("state exists");
    let address: ResourceAddress = "application.api".parse().unwrap();
    assert_eq!(
        state
            .resource(&address)
            .expect("application")
            .remote_id()
            .as_str(),
        "application-2"
    );
    assert_eq!(
        store.recovery_status().expect("journal scan"),
        RecoveryStatus::Clean
    );
    assert_no_external_material(&durable_text(directory.path()));
}

#[tokio::test]
async fn replacement_with_durable_dependents_is_refused_before_any_mutation() {
    let mut responses = reads();
    responses.extend([
        servers(&[("server-1", "edge-1"), ("server-2", "edge-2")]),
        r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"},{"applicationId":"application-3","environmentId":"environment-1","name":"worker"}],"total":2}"#
            .to_owned(),
        application_one(r#","serverId":"server-1""#),
        r#"{"applicationId":"application-3","environmentId":"environment-1","name":"worker","appName":"worker"}"#.to_owned(),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let (directory, config_file) = workspace(
        &server,
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api:\n          server:\n            name: edge-2\n        worker:\n          depends_on: [application.api]\n",
        vec![
            (
                "application.api",
                ResourceKind::Application,
                "application-1",
                serde_json::json!({"server": {"name": "edge-1"}}),
                vec![],
            ),
            (
                "application.worker",
                ResourceKind::Application,
                "application-3",
                serde_json::json!({}),
                vec!["application.api"],
            ),
        ],
    );

    let error = apply_workspace(&server.client(), &config_file)
        .await
        .expect_err("dependents block the replacement");

    assert!(matches!(
        error,
        ApplyWorkspaceError::ReplacementBlockedByDependents
    ));
    let instance = server.instance();
    let requests = server.finish();
    assert_eq!(requests.len(), 7);
    assert!(
        requests
            .iter()
            .all(|request| request.starts_with("GET /api/")),
        "no mutation precedes the refusal"
    );
    let store = StateStore::new(directory.path(), instance).expect("state store");
    assert_eq!(
        store.recovery_status().expect("journal scan"),
        RecoveryStatus::Clean
    );
}

#[tokio::test]
async fn an_unresolved_selector_blocks_apply_before_any_mutation() {
    for rows in [
        registries(&[]),
        registries(&[("registry-1", "main"), ("registry-2", "main")]),
    ] {
        let mut responses = reads();
        responses.extend([rows, NO_APPLICATIONS.to_owned()]);
        let server = TestServer::respond_in_sequence(responses);
        let (directory, config_file) = workspace(
            &server,
            &config("        registry:\n          name: main"),
            vec![],
        );

        let error = apply_workspace(&server.client(), &config_file)
            .await
            .expect_err("unresolved selector blocks apply");

        assert!(matches!(error, ApplyWorkspaceError::PlanBlocked));
        let requests = server.finish();
        assert_eq!(requests.len(), 5);
        assert!(
            requests
                .iter()
                .all(|request| request.starts_with("GET /api/"))
        );
        assert_no_external_material(&durable_text(directory.path()));
    }
}

fn fingerprint_key() -> String {
    format!("0199a0c8-2351-7c31-8899-2c8f81983ea5:{}", "09".repeat(32))
}

fn run(directory: &Path, url: &str, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dokploy"))
        .current_dir(directory)
        .env("DOKPLOY_FINGERPRINT_KEY", fingerprint_key())
        .args(["--url", url, "--api-key", "test-api-key"])
        .args(arguments)
        .output()
        .expect("dokploy runs")
}

fn create_reads(registry_rows: String) -> Vec<String> {
    let mut responses = reads();
    responses.extend([registry_rows, NO_APPLICATIONS.to_owned()]);
    responses
}

#[test]
fn a_saved_plan_applies_while_its_external_resolution_is_unchanged() {
    let mut responses = create_reads(registries(&[("registry-1", "main")]));
    responses.extend(create_reads(registries(&[("registry-1", "main")])));
    responses.extend([
        r#"{"applicationId":"application-9"}"#.to_owned(),
        OK.to_owned(),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let (directory, _config_file) = workspace(
        &server,
        &config("        registry:\n          name: main"),
        vec![],
    );
    let url = server.url.clone();

    let planned = run(directory.path(), &url, &["plan", "--out", "saved.json"]);
    assert!(planned.status.success(), "{planned:?}");
    let envelope = fs::read_to_string(directory.path().join("saved.json")).expect("envelope");
    assert_no_external_material(&envelope);
    assert!(
        !envelope.contains("\"main\""),
        "selector names stay out of the envelope"
    );
    let applied = run(
        directory.path(),
        &url,
        &["apply", "saved.json", "--auto-approve"],
    );

    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(String::from_utf8_lossy(&applied.stdout).contains("Apply complete: 1 change(s)."));
    let requests = server.finish();
    assert_eq!(requests.len(), 12);
    assert!(requests[10].starts_with("POST /api/application.create HTTP/1.1\r\n"));
    assert!(requests[11].contains(r#""registryId":"registry-1""#));
}

#[test]
fn a_saved_plan_is_refused_when_the_external_record_is_recreated_before_apply() {
    let mut responses = create_reads(registries(&[("registry-1", "main")]));
    // The identical plan is rebuilt, but "main" now names a different record.
    responses.extend(create_reads(registries(&[("registry-2", "main")])));
    let server = TestServer::respond_in_sequence(responses);
    let (directory, _config_file) = workspace(
        &server,
        &config("        registry:\n          name: main"),
        vec![],
    );
    let url = server.url.clone();

    let planned = run(directory.path(), &url, &["plan", "--out", "saved.json"]);
    assert!(planned.status.success(), "{planned:?}");
    let applied = run(
        directory.path(),
        &url,
        &["apply", "saved.json", "--auto-approve"],
    );

    assert!(!applied.status.success());
    let diagnostics = String::from_utf8_lossy(&applied.stderr).into_owned();
    assert_no_external_material(&diagnostics);
    assert_no_external_material(&String::from_utf8_lossy(&applied.stdout));
    let requests = server.finish();
    assert_eq!(requests.len(), 10);
    assert!(
        requests
            .iter()
            .all(|request| request.starts_with("GET /api/")),
        "a stale saved plan performs no mutation"
    );
    assert_no_external_material(&durable_text(directory.path()));
}
