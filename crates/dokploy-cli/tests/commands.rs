//! Commands that do not touch declared state: contexts, completions, offline behavior, and the
//! generated `api` commands.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use clap::Parser;
use dokploy_cli::cli::Cli;
use dokploy_cli::config::ConfigRepository;
use dokploy_cli::credentials::{ApiKey, CredentialStore, CredentialStoreError};
use dokploy_cli::execute;

#[derive(Default)]
struct MemoryCredentialStore {
    values: BTreeMap<String, ApiKey>,
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, context: &str) -> Result<Option<ApiKey>, CredentialStoreError> {
        Ok(self.values.get(context).cloned())
    }

    fn set(&self, _context: &str, _api_key: &ApiKey) -> Result<(), CredentialStoreError> {
        Ok(())
    }

    fn delete(&self, _context: &str) -> Result<(), CredentialStoreError> {
        Ok(())
    }
}

struct PanicCredentialStore;

impl CredentialStore for PanicCredentialStore {
    fn get(&self, _context: &str) -> Result<Option<ApiKey>, CredentialStoreError> {
        panic!("offline commands must not read credentials")
    }

    fn set(&self, _context: &str, _api_key: &ApiKey) -> Result<(), CredentialStoreError> {
        panic!("offline commands must not write credentials")
    }

    fn delete(&self, _context: &str) -> Result<(), CredentialStoreError> {
        panic!("offline commands must not delete credentials")
    }
}

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_with_json(body: &'static str) -> Self {
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
                if count == 0 || request_is_complete(&bytes) {
                    break;
                }
            }

            sender
                .send(vec![String::from_utf8(bytes).expect("request is UTF-8")])
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
            requests,
            thread,
        }
    }

    fn finish(self) -> String {
        let mut requests = self.requests.recv().expect("test receives the request");
        self.thread.join().expect("test server exits cleanly");
        assert_eq!(requests.len(), 1, "test expected exactly one request");
        requests.remove(0)
    }
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

#[tokio::test]
async fn context_list_marks_the_selected_context() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
    let config_path = temporary_directory.path().join("config.toml");
    fs::write(
        &config_path,
        r#"current_context = "production"

[contexts.production]
url = "https://deploy.example.com"

[contexts.staging]
url = "https://staging.example.com"
"#,
    )
    .expect("configuration fixture is writable");
    let repository = ConfigRepository::new(config_path);
    let credentials = MemoryCredentialStore::default();
    let cli = Cli::try_parse_from(["dokploy", "context", "list"]).expect("command line is valid");
    let mut output = Vec::new();

    execute(cli, &repository, &credentials, &mut output)
        .await
        .expect("command succeeds");

    assert_eq!(
        String::from_utf8(output).expect("output is UTF-8"),
        "* production\thttps://deploy.example.com\n  staging\thttps://staging.example.com\n"
    );
}

#[tokio::test]
async fn context_show_reports_credential_presence_without_printing_it() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
    let config_path = temporary_directory.path().join("config.toml");
    fs::write(
        &config_path,
        r#"current_context = "production"

[contexts.production]
url = "https://deploy.example.com"
"#,
    )
    .expect("configuration fixture is writable");
    let repository = ConfigRepository::new(config_path);
    let secret = "secret-that-must-not-be-rendered";
    let credentials = MemoryCredentialStore {
        values: BTreeMap::from([("production".to_owned(), ApiKey::new(secret))]),
    };
    let cli = Cli::try_parse_from(["dokploy", "context", "show"]).expect("command line is valid");
    let mut output = Vec::new();

    execute(cli, &repository, &credentials, &mut output)
        .await
        .expect("command succeeds");

    let output = String::from_utf8(output).expect("output is UTF-8");
    assert_eq!(
        output,
        "name: production\nurl: https://deploy.example.com\napi key: stored\n"
    );
    assert!(!output.contains(secret));
}

#[tokio::test]
async fn completions_are_generated_offline_from_the_public_command_tree() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
    let repository = ConfigRepository::new(temporary_directory.path().join("missing-config.toml"));
    let mut output = Vec::new();

    execute(
        Cli::try_parse_from(["dokploy", "completions", "bash"]).expect("command line is valid"),
        &repository,
        &PanicCredentialStore,
        &mut output,
    )
    .await
    .expect("completion generation succeeds without connection dependencies");

    let output = String::from_utf8(output).expect("completion output is UTF-8");
    assert!(output.contains("_dokploy"));
    for command in [
        "apply",
        "completions",
        "context",
        "init",
        "plan",
        "schema",
        "state",
        "validate",
    ] {
        assert!(output.contains(command), "missing `{command}` completion");
    }
    assert!(output.contains("--auto-approve"));
    assert!(output.contains("--detailed-exitcode"));
}

#[tokio::test]
async fn offline_commands_ignore_connection_overrides_and_poisoned_dependencies() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
    let context_path = temporary_directory.path().join("invalid-context.toml");
    fs::write(&context_path, "this is not valid TOML = [").expect("poison context is writable");
    let repository = ConfigRepository::new(context_path);
    let api_key_canary = "offline-api-key-canary";
    let cli = Cli::try_parse_from([
        "dokploy",
        "--url",
        "not-a-valid-url",
        "--api-key",
        api_key_canary,
        "schema",
    ])
    .expect("command line is valid");
    let mut output = Vec::new();

    execute(cli, &repository, &PanicCredentialStore, &mut output)
        .await
        .expect("schema bypasses all connection dependencies");

    serde_json::from_slice::<serde_json::Value>(&output).expect("schema output is JSON");
    assert!(!String::from_utf8_lossy(&output).contains(api_key_canary));
}

#[tokio::test]
async fn generated_imperative_read_outputs_the_raw_json_response() {
    let server = TestServer::respond_with_json(r#"{"projectId":"project-1","extra":true}"#);
    let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
    let repository = ConfigRepository::new(temporary_directory.path().join("missing-config.toml"));
    let credentials = MemoryCredentialStore::default();
    let secret = "test-api-key";
    let cli = Cli::try_parse_from([
        "dokploy",
        "--url",
        &server.url,
        "--api-key",
        secret,
        "api",
        "project",
        "one",
        "--query-project-id",
        "project-1",
    ])
    .expect("generated command line is valid");
    let mut output = Vec::new();

    execute(cli, &repository, &credentials, &mut output)
        .await
        .expect("imperative command succeeds");

    let output = String::from_utf8(output).expect("output is UTF-8");
    assert_eq!(
        output,
        "{\n  \"projectId\": \"project-1\",\n  \"extra\": true\n}\n"
    );
    assert!(!output.contains(secret));
    let request = server.finish();
    assert!(request.starts_with("GET /api/project.one?projectId=project-1 HTTP/1.1\r\n"));
    assert!(request.contains("\r\nx-api-key: test-api-key\r\n"));
}

#[tokio::test]
async fn generated_imperative_read_redacts_database_passwords() {
    let secret = "database-secret-that-must-not-be-rendered";
    let server = TestServer::respond_with_json(
        r#"{"postgresId":"postgres-1","databasePassword":"database-secret-that-must-not-be-rendered"}"#,
    );
    let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
    let repository = ConfigRepository::new(temporary_directory.path().join("missing-config.toml"));
    let credentials = MemoryCredentialStore::default();
    let cli = Cli::try_parse_from([
        "dokploy",
        "--url",
        &server.url,
        "--api-key",
        "test-api-key",
        "api",
        "postgres",
        "one",
        "--query-postgres-id",
        "postgres-1",
    ])
    .expect("generated command line is valid");
    let mut output = Vec::new();

    execute(cli, &repository, &credentials, &mut output)
        .await
        .expect("imperative command succeeds");

    let output = String::from_utf8(output).expect("output is UTF-8");
    assert_eq!(
        output,
        "{\n  \"postgresId\": \"postgres-1\",\n  \"databasePassword\": \"[REDACTED]\"\n}\n"
    );
    assert!(!output.contains(secret));
    server.finish();
}

#[tokio::test]
async fn generated_imperative_read_recursively_redacts_secret_key_families() {
    let server = TestServer::respond_with_json(
        r#"{
            "environmentId":"environment-1",
            "passwordPolicy":"strict",
            "nested":{
                "password":"password-value",
                "databasePassword":"database-password-value",
                "apiKey":"api-key-value",
                "providerAccessKey":"access-key-value",
                "sshPrivateKey":"private-key-value",
                "clientSecret":"secret-value",
                "buildSecrets":"build-secrets-value",
                "sessionToken":"token-value",
                "refreshToken":"refresh-token-value",
                "runtimeEnv":"env-value",
                "previewEnv":"preview-env-value",
                "dockerBuildArgs":"build-args-value",
                "previewBuildArgs":"preview-build-args-value",
                "secretary":"preserved-secretary",
                "tokenizer":"preserved-tokenizer"
            },
            "items":[
                {
                    "backupPassword":"array-password-value",
                    "applicationId":"application-1"
                }
            ]
        }"#,
    );
    let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
    let repository = ConfigRepository::new(temporary_directory.path().join("missing-config.toml"));
    let credentials = MemoryCredentialStore::default();
    let cli = Cli::try_parse_from([
        "dokploy",
        "--url",
        &server.url,
        "--api-key",
        "test-api-key",
        "api",
        "project",
        "one",
        "--query-project-id",
        "project-1",
    ])
    .expect("generated command line is valid");
    let mut output = Vec::new();

    execute(cli, &repository, &credentials, &mut output)
        .await
        .expect("imperative command succeeds");

    let output = String::from_utf8(output).expect("output is UTF-8");
    let response: serde_json::Value =
        serde_json::from_str(&output).expect("output remains valid JSON");
    let redacted = serde_json::Value::String("[REDACTED]".to_owned());

    assert_eq!(response["environmentId"], "environment-1");
    assert_eq!(response["passwordPolicy"], "strict");
    assert_eq!(response["nested"]["secretary"], "preserved-secretary");
    assert_eq!(response["nested"]["tokenizer"], "preserved-tokenizer");
    assert_eq!(response["items"][0]["applicationId"], "application-1");

    for key in [
        "password",
        "databasePassword",
        "apiKey",
        "providerAccessKey",
        "sshPrivateKey",
        "clientSecret",
        "buildSecrets",
        "sessionToken",
        "refreshToken",
        "runtimeEnv",
        "previewEnv",
        "dockerBuildArgs",
        "previewBuildArgs",
    ] {
        assert_eq!(response["nested"][key], redacted, "key `{key}` leaked");
    }
    assert_eq!(response["items"][0]["backupPassword"], redacted);

    for secret in [
        "password-value",
        "database-password-value",
        "api-key-value",
        "access-key-value",
        "private-key-value",
        "secret-value",
        "build-secrets-value",
        "token-value",
        "refresh-token-value",
        "env-value",
        "preview-env-value",
        "build-args-value",
        "preview-build-args-value",
        "array-password-value",
    ] {
        assert!(!output.contains(secret), "secret value was rendered");
    }

    server.finish();
}

#[tokio::test]
async fn generated_body_fields_form_the_wire_json_object() {
    let server = TestServer::respond_with_json(r#"{"projectId":"project-1"}"#);
    let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
    let repository = ConfigRepository::new(temporary_directory.path().join("missing-config.toml"));
    let credentials = MemoryCredentialStore::default();
    let cli = Cli::try_parse_from([
        "dokploy",
        "--url",
        &server.url,
        "--api-key",
        "test-api-key",
        "api",
        "project",
        "create",
        "--body-name",
        "IaC Project",
    ])
    .expect("generated command line is valid");
    let mut output = Vec::new();

    execute(cli, &repository, &credentials, &mut output)
        .await
        .expect("imperative command succeeds");

    let request = server.finish();
    assert!(request.starts_with("POST /api/project.create HTTP/1.1\r\n"));
    let (_, body) = request
        .split_once("\r\n\r\n")
        .expect("request includes the JSON body");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).expect("request body is JSON"),
        serde_json::json!({"name": "IaC Project"})
    );
}
