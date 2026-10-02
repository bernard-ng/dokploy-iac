use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, OperationJournal, PlanDigest, RecoveryStatus,
    RemoteId, ResourceAddress, ResourceKind, ResourceState, StateFile, StateStore,
};
use semver::Version;

struct OneShotServer {
    url: String,
    request: Receiver<String>,
    thread: JoinHandle<()>,
}

impl OneShotServer {
    fn project_topology(body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, request) = mpsc::channel();
        let thread = thread::spawn(move || {
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

            sender
                .send(String::from_utf8(bytes).expect("request is UTF-8"))
                .expect("test receives the request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response is writable");
        });

        Self {
            url: format!("http://{address}"),
            request,
            thread,
        }
    }

    fn finish(self) -> String {
        let request = self.request.recv().expect("test receives the request");
        self.thread.join().expect("test server exits cleanly");
        request
    }
}

#[test]
fn non_interactive_destroy_requires_auto_approve_before_delete() {
    let server = OneShotServer::project_topology(
        r#"[{"projectId":"project-1","name":"platform","description":"Managed","environments":[]}]"#,
    );
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config = workspace.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\n  description: Managed\n  environments: {}\n",
    )
    .expect("configuration fixture is writable");
    let instance = InstanceIdentity::parse(&server.url).expect("server URL is valid");
    let store = persist_project(workspace.path(), instance);

    let destroyed = Command::new(env!("CARGO_BIN_EXE_dokploy"))
        .current_dir(workspace.path())
        .args(["--url", &server.url, "--api-key", "test-api-key", "destroy"])
        .output()
        .expect("dokploy destroy runs");

    assert_eq!(destroyed.status.code(), Some(1));
    assert!(destroyed.stdout.is_empty());
    let diagnostics = String::from_utf8_lossy(&destroyed.stderr);
    assert!(diagnostics.contains("delete project.platform"));
    assert!(diagnostics.contains("destroy approval requires a terminal; use --auto-approve"));
    assert!(
        store
            .inspect()
            .expect("state remains readable")
            .expect("state remains present")
            .resource(&address("project.platform"))
            .is_some()
    );
    let request = server.finish();
    assert!(request.starts_with("GET /api/project.all HTTP/1.1\r\n"));
}

#[test]
fn non_interactive_recovery_requires_auto_approve_before_state_change() {
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config = workspace.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject:\n  name: platform\n  environments: {}\n",
    )
    .expect("configuration fixture is writable");
    let url = "http://127.0.0.1:1";
    let instance = InstanceIdentity::parse(url).expect("instance URL is valid");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store is valid");
    let state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .expect("writer starts")
        .checkpoint(ExpectedState::absent(), &state)
        .expect("initial state is persisted");
    let mut write = store.begin_write().expect("writer starts");
    let journal = OperationJournal::begin(
        &mut write,
        PlanDigest::parse("a".repeat(64)).expect("plan digest is valid"),
    )
    .expect("journal begins");
    drop(journal);
    drop(write);
    assert!(matches!(
        store
            .recovery_status()
            .expect("recovery status is readable"),
        RecoveryStatus::RecoveryRequired(_)
    ));

    let recovered = Command::new(env!("CARGO_BIN_EXE_dokploy"))
        .current_dir(workspace.path())
        .args(["--url", url, "--api-key", "test-api-key", "recover"])
        .output()
        .expect("dokploy recover runs");

    assert_eq!(recovered.status.code(), Some(1));
    assert!(recovered.stdout.is_empty());
    let diagnostics = String::from_utf8_lossy(&recovered.stderr);
    assert!(diagnostics.contains("Recovery action: ResolveOperation"));
    assert!(diagnostics.contains("recovery approval requires a terminal; use --auto-approve"));
    assert!(matches!(
        store
            .recovery_status()
            .expect("recovery status is readable"),
        RecoveryStatus::RecoveryRequired(_)
    ));
}

fn persist_project(workspace: &std::path::Path, instance: InstanceIdentity) -> StateStore {
    let store = StateStore::new(workspace, instance.clone()).expect("state store is valid");
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .expect("writer starts")
        .checkpoint(ExpectedState::absent(), &state)
        .expect("initial state is persisted");
    let initial = state.clone();
    state
        .upsert_resource(
            address("project.platform"),
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new("project-1").expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(serde_json::json!({"description": "Managed"}))
                    .expect("managed inputs are valid"),
                None,
                Vec::new(),
            ),
        )
        .expect("project state is valid");
    store
        .begin_write()
        .expect("writer starts")
        .checkpoint(ExpectedState::from_state(&initial), &state)
        .expect("project state is persisted");

    store
}

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("resource address is valid")
}
