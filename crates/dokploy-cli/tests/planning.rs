use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::planning::plan_workspace;
use dokploy_sdk::Dokploy;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn empty_project_topology(request_count: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received = Vec::new();
            for _ in 0..request_count {
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
                received.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n[]"
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

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("test server exits cleanly");
        requests
    }
}

#[tokio::test]
async fn absent_workspace_plan_is_deterministic_and_read_only() {
    let server = TestServer::empty_project_topology(2);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    let config = directory.path().join("dokploy.yaml");
    fs::write(
        &config,
        "version: 1\nproject: { name: platform }\nenvironments:\n  production: {}\n",
    )
    .expect("configuration fixture is writable");

    let first = plan_workspace(&server.client(), &config)
        .await
        .expect("initial plan succeeds");
    let second = plan_workspace(&server.client(), &config)
        .await
        .expect("repeated plan succeeds");

    assert!(first.complete());
    assert!(first.applyable());
    assert_eq!(first.changes().len(), 2);
    assert_eq!(first.to_json_bytes(), second.to_json_bytes());
    assert!(!directory.path().join(".dokploy").exists());
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| { request.starts_with("GET /api/project.all HTTP/1.1\r\n") })
    );
}

#[test]
fn public_plan_json_uses_detailed_exit_code_two_for_changes() {
    let server = TestServer::empty_project_topology(1);
    let directory = tempfile::tempdir().expect("temporary workspace is available");
    fs::write(
        directory.path().join("dokploy.yaml"),
        "version: 1\nproject: { name: platform }\nenvironments: {}\n",
    )
    .expect("configuration fixture is writable");

    let output = Command::new(env!("CARGO_BIN_EXE_dokploy"))
        .current_dir(directory.path())
        .env(
            "DOKPLOY_FINGERPRINT_KEY",
            format!("0199a0c8-2351-7c31-8899-2c8f81983ea5:{}", "07".repeat(32)),
        )
        .args([
            "--url",
            &server.url,
            "--api-key",
            "test-api-key",
            "plan",
            "--json",
            "--detailed-exitcode",
            "--out",
            "saved-plan.json",
        ])
        .output()
        .expect("dokploy plan runs");

    assert_eq!(output.status.code(), Some(2));
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is a JSON plan");
    assert_eq!(document["changes"][0]["kind"], "create");
    let saved = directory.path().join("saved-plan.json");
    let saved_document: serde_json::Value =
        serde_json::from_slice(&fs::read(saved).expect("saved plan artifact is readable"))
            .expect("saved plan artifact is JSON");
    assert_eq!(saved_document["formatVersion"], 1);
    assert_eq!(saved_document["plan"], document);
    assert_eq!(
        saved_document["remoteReceipt"]
            .as_str()
            .expect("receipt is a string")
            .len(),
        64
    );
    assert!(!directory.path().join(".dokploy").exists());
    assert_eq!(server.finish().len(), 1);
}
