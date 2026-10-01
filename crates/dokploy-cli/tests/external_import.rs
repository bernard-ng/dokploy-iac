use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_cli::cli::ImportKind;
use dokploy_cli::import::{ImportError, ImportRequest, import_resource};
use dokploy_cli::planning::plan_workspace;
use dokploy_config::{DokployConfig, ExternalSelector, Field};
use dokploy_sdk::Dokploy;
use dokploy_state::{InstanceIdentity, StateStore};

const SECRET_CANARY: &str = "import-secret-canary-2b61";

const ENVIRONMENT: &str = r#"{"environmentId":"environment-1","name":"production","description":"Production","projectId":"project-1"}"#;
const PROJECT: &str =
    r#"{"projectId":"project-1","name":"platform","description":"Platform","environments":[]}"#;
const TOPOLOGY: &str = r#"[{"projectId":"project-1","name":"platform","description":"Platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[{"applicationId":"application-1","name":"api"}],"postgres":[],"redis":[]}]}]"#;
const ENVIRONMENT_COLLECTION: &str =
    r#"[{"environmentId":"environment-1","name":"production","description":"Production"}]"#;
const APPLICATION_COLLECTION: &str = r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"api"}],"total":1}"#;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_in_sequence(responses: Vec<(&'static str, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut captured = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 2048];
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
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }
            sender.send(captured).expect("requests are returned");
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
            .expect("client is valid")
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("requests are received");
        self.thread.join().expect("server exits");
        requests
    }
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

fn application(extra: &str) -> String {
    format!(
        r#"{{"applicationId":"application-1","environmentId":"environment-1","name":"api","appName":"api","description":"API"{extra}}}"#
    )
}

fn request(config_file: std::path::PathBuf) -> ImportRequest {
    ImportRequest {
        kind: ImportKind::Application,
        remote_id: "application-1".to_owned(),
        address: "application.api".parse().unwrap(),
        config_file,
    }
}

fn import_prefix(application: String) -> Vec<(&'static str, String)> {
    vec![
        ("200 OK", application),
        ("200 OK", ENVIRONMENT.to_owned()),
        ("200 OK", PROJECT.to_owned()),
    ]
}

#[tokio::test]
async fn application_import_emits_name_selectors_and_converges_without_identities() {
    let attached = application(
        r#","serverId":"server-1","buildServerId":null,"registryId":"registry-1","rollbackRegistryId":"registry-2""#,
    );
    let mut responses = import_prefix(attached.clone());
    responses.extend([
        (
            "200 OK",
            servers(&[("server-1", "edge-1"), ("server-9", "other")]),
        ),
        (
            "200 OK",
            registries(&[("registry-1", "main"), ("registry-2", "archive")]),
        ),
        // The convergence plan after import.
        ("200 OK", TOPOLOGY.to_owned()),
        ("200 OK", ENVIRONMENT_COLLECTION.to_owned()),
        ("200 OK", ENVIRONMENT.to_owned()),
        (
            "200 OK",
            servers(&[("server-1", "edge-1"), ("server-9", "other")]),
        ),
        (
            "200 OK",
            registries(&[("registry-1", "main"), ("registry-2", "archive")]),
        ),
        ("200 OK", APPLICATION_COLLECTION.to_owned()),
        ("200 OK", attached),
    ]);
    let server = TestServer::respond_in_sequence(responses);
    let client = server.client();
    let workspace = tempfile::tempdir().expect("workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");

    let count = import_resource(&client, request(config_file.clone()))
        .await
        .expect("application with associations imports");
    let plan = plan_workspace(&client, &config_file)
        .await
        .expect("fresh plan succeeds");

    assert_eq!(count, 3);
    assert!(plan.complete());
    assert!(plan.applyable(), "{:?}", plan.diagnostics());
    assert!(plan.changes().is_empty());
    let source = std::fs::read_to_string(&config_file).expect("config is readable");
    for identity in [
        "server-1",
        "server-9",
        "registry-1",
        "registry-2",
        SECRET_CANARY,
    ] {
        assert!(!source.contains(identity), "{identity} leaked into config");
    }
    let config = DokployConfig::parse(&source).expect("config is canonical and valid");
    let resource = config
        .resource(&"application.api".parse().unwrap())
        .unwrap();
    let api = resource.as_application().unwrap();
    assert_eq!(api.server(), &Field::Set(ExternalSelector::named("edge-1")));
    assert_eq!(api.registry(), &Field::Set(ExternalSelector::named("main")));
    assert_eq!(
        api.rollback_registry(),
        &Field::Set(ExternalSelector::named("archive"))
    );
    assert_eq!(api.build_server(), &Field::Unmanaged);
    assert_eq!(api.build_registry(), &Field::Unmanaged);

    let instance = InstanceIdentity::parse(&server.url).unwrap();
    let state = StateStore::new(workspace.path(), instance)
        .unwrap()
        .inspect()
        .unwrap()
        .unwrap();
    let inputs = state
        .resource(&"application.api".parse().unwrap())
        .unwrap()
        .last_applied()
        .as_json()
        .clone();
    assert_eq!(inputs["server"], serde_json::json!({"name": "edge-1"}));
    assert_eq!(inputs["registry"], serde_json::json!({"name": "main"}));
    assert_eq!(
        inputs["rollback_registry"],
        serde_json::json!({"name": "archive"})
    );
    let state_text = inputs.to_string();
    for identity in ["server-1", "registry-1", "registry-2", SECRET_CANARY] {
        assert!(!state_text.contains(identity));
    }

    let requests = server.finish();
    assert_eq!(requests.len(), 12);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[tokio::test]
async fn application_without_associations_reads_no_external_collections() {
    let responses = import_prefix(application(r#","serverId":null,"registryId":null"#));
    let server = TestServer::respond_in_sequence(responses);
    let client = server.client();
    let workspace = tempfile::tempdir().expect("workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");

    import_resource(&client, request(config_file.clone()))
        .await
        .expect("plain application imports");

    let source = std::fs::read_to_string(&config_file).expect("config is readable");
    assert!(!source.contains("server"));
    assert!(!source.contains("registry"));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("server.all") && !request.contains("registry.all"))
    );
}

#[tokio::test]
async fn ambiguous_unknown_or_unreadable_associations_fail_closed_without_writing() {
    let attached = application(r#","registryId":"registry-1""#);
    for (status, body) in [
        (
            "200 OK",
            registries(&[("registry-1", "main"), ("registry-2", "main")]),
        ),
        ("200 OK", registries(&[("registry-2", "main")])),
        ("200 OK", registries(&[("registry-1", " padded ")])),
        (
            "500 Internal Server Error",
            format!(r#"{{"message":"{SECRET_CANARY}"}}"#),
        ),
        (
            "403 Forbidden",
            format!(r#"{{"message":"{SECRET_CANARY}"}}"#),
        ),
    ] {
        let mut responses = import_prefix(attached.clone());
        responses.push((status, body));
        let server = TestServer::respond_in_sequence(responses);
        let workspace = tempfile::tempdir().expect("workspace is available");
        let config_file = workspace.path().join("dokploy.yaml");

        let error = import_resource(&server.client(), request(config_file.clone()))
            .await
            .expect_err("an association without a unique name cannot be imported");

        assert!(matches!(error, ImportError::ExternalAssociation));
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains(SECRET_CANARY));
        assert!(!rendered.contains("registry-1"));
        assert!(!config_file.exists());
        assert!(!workspace.path().join(".dokploy/state.json").exists());
        let requests = server.finish();
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
    }
}
