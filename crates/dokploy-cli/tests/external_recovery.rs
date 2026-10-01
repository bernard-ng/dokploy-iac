use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::recovery::{RecoveryAction, recover_workspace_with_approval};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    ExpectedCheckpoint, ExpectedState, InstanceIdentity, JournalAction, ManagedInputs,
    OperationJournal, PlanDigest, RecoveryStatus, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, StateFile, StateStore,
};
use semver::Version;

const SECRET_CANARY: &str = "recovery-secret-canary-31aa";
const PROJECT: &str = r#"[{"projectId":"project-1","name":"platform","environments":[]}]"#;
const ENVIRONMENTS: &str = r#"[{"environmentId":"environment-1","name":"production"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_in_sequence(bodies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut captured = Vec::new();
            for body in bodies {
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
                captured.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }
            sender.send(captured).expect("test receives requests");
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

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("test server exits cleanly");
        requests
    }
}

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
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

fn application_search(ids: &[&str]) -> String {
    let rows: Vec<_> = ids
        .iter()
        .map(|id| {
            format!(r#"{{"applicationId":"{id}","environmentId":"environment-1","name":"api"}}"#)
        })
        .collect();
    format!(r#"{{"items":[{}],"total":{}}}"#, rows.join(","), ids.len())
}

fn application_one(id: &str, extra: &str) -> String {
    format!(
        r#"{{"applicationId":"{id}","environmentId":"environment-1","name":"api","appName":"api"{extra}}}"#
    )
}

fn application_state(remote_id: &str, inputs: serde_json::Value) -> ResourceState {
    ResourceState::new(
        ResourceKind::Application,
        RemoteId::new(remote_id).unwrap(),
        false,
        ManagedInputs::try_from_json(inputs).unwrap(),
        Some(address("environment.production")),
        Vec::new(),
    )
}

/// Persists project and environment state plus an optional application.
fn seed(
    store: &StateStore,
    instance: InstanceIdentity,
    application: Option<ResourceState>,
) -> StateFile {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::absent(), &state)
        .unwrap();
    let mut resources = vec![
        (
            "project.platform",
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new("project-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                None,
                Vec::new(),
            ),
        ),
        (
            "environment.production",
            ResourceState::new(
                ResourceKind::Environment,
                RemoteId::new("environment-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                Some(address("project.platform")),
                Vec::new(),
            ),
        ),
    ];
    if let Some(application) = application {
        resources.push(("application.api", application));
    }
    for (address_value, resource) in resources {
        let before = state.clone();
        state
            .upsert_resource(address(address_value), resource)
            .unwrap();
        store
            .begin_write()
            .unwrap()
            .checkpoint(ExpectedState::from_state(&before), &state)
            .unwrap();
    }

    state
}

fn assert_no_external_material(text: &str) {
    for canary in [SECRET_CANARY, "server-2", "registry-new", "registry-old"] {
        assert!(!text.contains(canary), "{canary} leaked");
    }
}

fn durable_text(directory: &Path) -> String {
    let mut text = String::new();
    let mut pending = vec![directory.join(".dokploy")];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(
                fs::read_dir(&path)
                    .expect("directory is readable")
                    .flatten()
                    .map(|entry| entry.path()),
            );
        } else if let Ok(contents) = fs::read_to_string(&path) {
            text.push_str(&contents);
        }
    }
    text
}

fn config(application: &str) -> String {
    format!(
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      api:\n{application}\n"
    )
}

fn read_prefix() -> Vec<String> {
    vec![
        PROJECT.to_owned(),
        ENVIRONMENTS.to_owned(),
        ENVIRONMENT.to_owned(),
    ]
}

#[tokio::test]
async fn uncertain_replacement_create_adopts_the_unique_application_without_retrying() {
    let mut responses = read_prefix();
    responses.extend([
        servers(&[("server-2", "edge-2")]),
        application_search(&["application-2"]),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        config("        server:\n          name: edge-2"),
    )
    .expect("configuration is writable");
    let instance = InstanceIdentity::parse(&server.url).expect("instance");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store");
    seed(&store, instance, None);
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("e".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            address("application.api"),
            JournalAction::Create,
            ExpectedCheckpoint::create(ResourceState::new(
                ResourceKind::Application,
                RemoteId::new("recovery-pending").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                Some(address("environment.production")),
                Vec::new(),
            ))
            .unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .expect("one unique application adopts the uncertain create");

    assert_eq!(result.recovered_steps(), 1);
    let recovered = store.inspect().unwrap().unwrap();
    assert_eq!(
        recovered
            .resource(&address("application.api"))
            .unwrap()
            .remote_id()
            .as_str(),
        "application-2"
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("application.create")
                && !request.contains("application.update"))
    );
    assert_no_external_material(&durable_text(workspace.path()));
}

#[tokio::test]
async fn uncertain_placement_checkpoint_is_confirmed_through_the_name_selector() {
    let mut responses = read_prefix();
    responses.extend([
        servers(&[("server-2", "edge-2")]),
        application_search(&["application-2"]),
        application_one("application-2", r#","serverId":"server-2""#),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        config("        server:\n          name: edge-2"),
    )
    .expect("configuration is writable");
    let instance = InstanceIdentity::parse(&server.url).expect("instance");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store");
    let before = application_state("application-2", serde_json::json!({}));
    seed(&store, instance, Some(before.clone()));
    let after = application_state(
        "application-2",
        serde_json::json!({"server": {"name": "edge-2"}}),
    );
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("f".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            address("application.api"),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, after).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::CheckpointConfirmedSuccess);
        Ok(true)
    })
    .await
    .expect("the observed placement confirms the checkpoint");

    assert_eq!(result.recovered_steps(), 1);
    let recovered = store.inspect().unwrap().unwrap();
    assert_eq!(
        recovered
            .resource(&address("application.api"))
            .unwrap()
            .last_applied()
            .as_json(),
        &serde_json::json!({"server": {"name": "edge-2"}})
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    assert_eq!(server.finish().len(), 6);
    assert_no_external_material(&durable_text(workspace.path()));
}

#[tokio::test]
async fn uncertain_registry_update_that_did_not_apply_confirms_no_change() {
    let mut responses = read_prefix();
    responses.extend([
        registries(&[("registry-new", "main"), ("registry-old", "legacy")]),
        application_search(&["application-1"]),
        application_one("application-1", r#","registryId":"registry-old""#),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        config("        registry:\n          name: main"),
    )
    .expect("configuration is writable");
    let instance = InstanceIdentity::parse(&server.url).expect("instance");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store");
    let before = application_state(
        "application-1",
        serde_json::json!({"registry": {"name": "legacy"}}),
    );
    seed(&store, instance, Some(before.clone()));
    let after = application_state(
        "application-1",
        serde_json::json!({"registry": {"name": "main"}}),
    );
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("a".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            address("application.api"),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, after).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::ConfirmNoChange);
        Ok(true)
    })
    .await
    .expect("the unchanged association confirms the update did not apply");

    assert_eq!(result.recovered_steps(), 1);
    let recovered = store.inspect().unwrap().unwrap();
    assert_eq!(
        recovered
            .resource(&address("application.api"))
            .unwrap()
            .last_applied()
            .as_json(),
        &serde_json::json!({"registry": {"name": "legacy"}})
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    assert_no_external_material(&durable_text(workspace.path()));
}

#[tokio::test]
async fn ambiguous_registry_names_leave_an_uncertain_update_for_manual_intervention() {
    let mut responses = read_prefix();
    responses.extend([
        registries(&[("registry-new", "main"), ("registry-old", "main")]),
        application_search(&["application-1"]),
        application_one("application-1", r#","registryId":"registry-new""#),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        config("        registry:\n          name: main"),
    )
    .expect("configuration is writable");
    let instance = InstanceIdentity::parse(&server.url).expect("instance");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store");
    let before = application_state("application-1", serde_json::json!({"registry": null}));
    seed(&store, instance, Some(before.clone()));
    let after = application_state(
        "application-1",
        serde_json::json!({"registry": {"name": "main"}}),
    );
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("b".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            address("application.api"),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, after).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    recover_workspace_with_approval(&server.client(), &config_file, |_| Ok(true))
        .await
        .expect_err("a duplicated name cannot prove which record was selected");

    assert!(matches!(
        store.recovery_status().unwrap(),
        RecoveryStatus::RecoveryRequired(_)
    ));
    let _ = server.finish();
}
