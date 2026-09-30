use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dokploy_cli::recovery::{RecoveryAction, recover_workspace_with_approval};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    ExpectedCheckpoint, ExpectedState, InstanceIdentity, JournalAction, ManagedInputs,
    OperationJournal, PlanDigest, RecoveryStatus, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, StateFile, StateStore,
};
use semver::Version;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

struct CrashServer {
    url: String,
    create_observed: Receiver<()>,
    release_create: mpsc::Sender<()>,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn project_topology(body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
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
                .send(vec![String::from_utf8(bytes).expect("request is UTF-8")])
                .expect("test receives request");
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

impl CrashServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("crash server binds");
        let address = listener.local_addr().expect("crash server has an address");
        let (create_sender, create_observed) = mpsc::channel();
        let (release_create, release_receiver) = mpsc::channel();
        let (request_sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut captured = Vec::new();

            let (mut plan_stream, _) = listener.accept().expect("plan request is accepted");
            captured.push(read_request(&mut plan_stream));
            write_response(&mut plan_stream, "[]");

            let (mut create_stream, _) = listener.accept().expect("create request is accepted");
            captured.push(read_request(&mut create_stream));
            create_sender
                .send(())
                .expect("test observes the create request");
            release_receiver
                .recv()
                .expect("test releases the interrupted response");
            drop(create_stream);

            let (mut recovery_stream, _) = listener.accept().expect("recovery read is accepted");
            captured.push(read_request(&mut recovery_stream));
            write_response(
                &mut recovery_stream,
                r#"[{"projectId":"project-1","name":"platform","description":"Stable","environments":[]}]"#,
            );

            request_sender
                .send(captured)
                .expect("test receives captured requests");
        });

        Self {
            url: format!("http://{address}"),
            create_observed,
            release_create,
            requests,
            thread,
        }
    }

    fn wait_for_create(&self) {
        self.create_observed
            .recv_timeout(Duration::from_secs(15))
            .expect("apply reaches the remote create");
    }

    fn release_interrupted_response(&self) {
        self.release_create
            .send(())
            .expect("interrupted response is released");
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("crash server exits cleanly");
        requests
    }
}

#[tokio::test]
async fn uncertain_create_adopts_one_exact_fresh_resource_without_retrying() {
    let server = TestServer::project_topology(
        r#"[{"projectId":"project-1","name":"platform","description":"Stable","environments":[]}]"#,
    );
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        "version: 1\nproject:\n  name: platform\n  description: Stable\n",
    )
    .expect("configuration fixture is writable");
    let instance = InstanceIdentity::parse(&server.url).expect("server URL is valid");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("state store is valid");
    let state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .expect("writer starts")
        .checkpoint(ExpectedState::absent(), &state)
        .expect("initial state is persisted");
    let target = ResourceState::new(
        ResourceKind::Project,
        RemoteId::new("recovery-pending").expect("placeholder ID is valid"),
        false,
        ManagedInputs::try_from_json(serde_json::json!({"description": "Stable"}))
            .expect("managed inputs are valid"),
        None,
        Vec::new(),
    );
    let mut write = store.begin_write().expect("writer starts");
    let mut journal = OperationJournal::begin(
        &mut write,
        PlanDigest::parse("a".repeat(64)).expect("digest is valid"),
    )
    .expect("journal begins");
    journal
        .start_recoverable_step(
            address("project.platform"),
            JournalAction::Create,
            ExpectedCheckpoint::create(target).expect("checkpoint is valid"),
        )
        .expect("recoverable step starts");
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&server.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&address("project.platform")));
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .expect("recovery succeeds");

    assert_eq!(result.recovered_steps(), 1);
    assert!(matches!(
        store
            .recovery_status()
            .expect("recovery status is readable"),
        RecoveryStatus::Clean
    ));
    let recovered = store
        .inspect()
        .expect("state is readable")
        .expect("state exists");
    assert_eq!(
        recovered
            .resource(&address("project.platform"))
            .expect("project is adopted")
            .remote_id()
            .as_str(),
        "project-1"
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /api/project.all HTTP/1.1\r\n"));
    assert!(!requests[0].contains("project.create"));
}

#[tokio::test]
async fn interrupted_move_recovers_atomically_without_remote_requests() {
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        concat!(
            "version: 1\n",
            "project:\n  name: renamed\n",
            "environments: {}\n",
            "moves:\n",
            "  - from: project.platform\n",
            "    to: project.renamed\n",
        ),
    )
    .unwrap();
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .unwrap();
    let instance = InstanceIdentity::parse(client.base_url().as_str()).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    let empty = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::absent(), &empty)
        .unwrap();
    let source = address("project.platform");
    let target = address("project.renamed");
    let child = address("environment.production");
    let mut current = empty.clone();
    current
        .upsert_resource(
            source.clone(),
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new("project-1").unwrap(),
                true,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                None,
                Vec::new(),
            ),
        )
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&empty), &current)
        .unwrap();
    let with_project = current.clone();
    current
        .upsert_resource(
            child.clone(),
            ResourceState::new(
                ResourceKind::Environment,
                RemoteId::new("environment-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                Some(source.clone()),
                vec![source.clone()],
            ),
        )
        .unwrap();
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&with_project), &current)
        .unwrap();
    let before = current.resource(&source).unwrap().clone();
    let mut expected = current.clone();
    expected.move_resource(&source, target.clone()).unwrap();
    let after = expected.resource(&target).unwrap().clone();
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("b".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(
            target.clone(),
            JournalAction::Move,
            ExpectedCheckpoint::move_resource(source.clone(), before, after).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(write);

    let result = recover_workspace_with_approval(&client, &config_file, |preview| {
        assert_eq!(preview.address(), Some(&target));
        assert_eq!(preview.action(), RecoveryAction::CheckpointConfirmedSuccess);
        Ok(true)
    })
    .await
    .expect("state-only move recovery succeeds without discovery");

    assert_eq!(result.recovered_steps(), 1);
    let recovered = store.inspect().unwrap().unwrap();
    assert_eq!(recovered.resource(&source), None);
    assert!(recovered.resource(&target).unwrap().is_protected());
    assert_eq!(
        recovered.resource(&child).unwrap().containment(),
        Some(&target)
    );
    assert_eq!(
        recovered.resource(&child).unwrap().dependencies(),
        std::slice::from_ref(&target)
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
}

#[test]
fn killed_apply_is_recovered_from_fresh_remote_evidence_without_duplicate_create() {
    let server = CrashServer::start();
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    fs::write(
        workspace.path().join("dokploy.yaml"),
        "version: 1\nproject:\n  name: platform\n  description: Stable\n",
    )
    .expect("configuration fixture is writable");

    let mut apply = spawn_cli(workspace.path(), &server.url, &["apply", "--auto-approve"]);
    server.wait_for_create();

    let instance = InstanceIdentity::parse(&server.url).expect("server URL is valid");
    let store = StateStore::new(workspace.path(), instance).expect("state store is valid");
    assert!(matches!(
        store.recovery_status().expect("journal is readable"),
        RecoveryStatus::RecoveryRequired(_)
    ));
    apply.kill().expect("apply process is killed");
    apply.wait().expect("killed apply is reaped");
    server.release_interrupted_response();

    let recovery = Command::new(env!("CARGO_BIN_EXE_dokploy"))
        .current_dir(workspace.path())
        .args([
            "--url",
            &server.url,
            "--api-key",
            "test-api-key",
            "recover",
            "--auto-approve",
        ])
        .output()
        .expect("recovery process runs");
    assert!(
        recovery.status.success(),
        "recovery failed: {}",
        String::from_utf8_lossy(&recovery.stderr)
    );

    assert!(matches!(
        store.recovery_status().expect("journal is readable"),
        RecoveryStatus::Clean
    ));
    let recovered = store
        .inspect()
        .expect("state is readable")
        .expect("state exists");
    assert_eq!(
        recovered
            .resource(&address("project.platform"))
            .expect("project is adopted")
            .remote_id()
            .as_str(),
        "project-1"
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /api/project.all HTTP/1.1\r\n"));
    assert!(requests[1].starts_with("POST /api/project.create HTTP/1.1\r\n"));
    assert!(requests[2].starts_with("GET /api/project.all HTTP/1.1\r\n"));
}

fn spawn_cli(workspace: &std::path::Path, url: &str, arguments: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_dokploy"))
        .current_dir(workspace)
        .args(["--url", url, "--api-key", "test-api-key"])
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("dokploy process starts")
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = stream.read(&mut buffer).expect("request is readable");
        bytes.extend_from_slice(&buffer[..count]);
        if count == 0 || request_is_complete(&bytes) {
            break;
        }
    }

    String::from_utf8(bytes).expect("request is UTF-8")
}

fn request_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();

    bytes.len() >= header_end + 4 + content_length
}

fn write_response(stream: &mut std::net::TcpStream, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .expect("response is writable");
}

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
}
