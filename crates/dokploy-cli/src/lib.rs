//! Presentation and local configuration for the `dokploy` command-line interface.

pub mod cli;
pub mod config;
pub mod credentials;
mod imperative;
mod imperative_generated;
mod redaction;
pub mod settings;
pub mod telemetry;

use std::io::Write;

use cli::{Cli, Command, ContextCommand};
use config::ConfigRepository;
use credentials::{ApiKey, CredentialStore};
use dokploy_sdk::Dokploy;
use miette::{IntoDiagnostic, Result};
use settings::{ConnectionOptions, ProcessEnvironment, resolve_connection};

/// Executes one parsed command against injected local configuration services.
///
/// Keeping dispatch independent from process globals makes command behavior
/// testable without accessing the user's configuration directory or keyring.
pub async fn execute(
    cli: Cli,
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
    output: &mut dyn Write,
) -> Result<()> {
    let Cli {
        url,
        api_key,
        command,
    } = cli;

    match command {
        Command::Context { command } => match command {
            ContextCommand::List => list_contexts(config, output),
            ContextCommand::Use { name } => use_context(config, &name, output),
            ContextCommand::Show { name } => {
                show_context(config, credentials, name.as_deref(), output)
            }
        },
        Command::Imperative(command) => {
            let invocation = command.into_invocation()?;
            let configuration = config.load()?;
            let settings = resolve_connection(
                ConnectionOptions {
                    url,
                    api_key: api_key.map(ApiKey::new),
                },
                &ProcessEnvironment,
                &configuration,
                credentials,
            )?;
            let client = Dokploy::builder()
                .url(settings.url().as_str())
                .api_key(settings.api_key().expose())
                .build()
                .into_diagnostic()?;
            let response = client
                .imperative()
                .execute(invocation.into_request())
                .await
                .into_diagnostic()?;

            let response = redaction::redact_response(response);
            serde_json::to_writer_pretty(&mut *output, &response).into_diagnostic()?;
            writeln!(output).into_diagnostic()?;

            Ok(())
        }
    }
}

fn list_contexts(config: &ConfigRepository, output: &mut dyn Write) -> Result<()> {
    let configuration = config.load()?;

    if configuration.contexts().is_empty() {
        writeln!(output, "No contexts configured.").into_diagnostic()?;
        return Ok(());
    }

    for (name, context) in configuration.contexts() {
        let marker = if configuration.current_context() == Some(name.as_str()) {
            '*'
        } else {
            ' '
        };

        writeln!(output, "{marker} {name}\t{}", context.url()).into_diagnostic()?;
    }

    Ok(())
}

fn use_context(config: &ConfigRepository, name: &str, output: &mut dyn Write) -> Result<()> {
    config.select(name)?;
    writeln!(output, "Switched to context `{name}`.").into_diagnostic()?;

    Ok(())
}

fn show_context(
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
    requested_name: Option<&str>,
    output: &mut dyn Write,
) -> Result<()> {
    let configuration = config.load()?;
    let (name, context) = configuration.context(requested_name)?;
    let credential_status = if credentials.get(name)?.is_some() {
        "stored"
    } else {
        "not stored"
    };

    writeln!(output, "name: {name}").into_diagnostic()?;
    writeln!(output, "url: {}", context.url()).into_diagnostic()?;
    writeln!(output, "api key: {credential_status}").into_diagnostic()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    use clap::Parser;

    use super::execute;
    use crate::cli::Cli;
    use crate::config::ConfigRepository;
    use crate::credentials::{ApiKey, CredentialStore, CredentialStoreError};

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

    struct TestServer {
        url: String,
        requests: Receiver<String>,
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
                requests,
                thread,
            }
        }

        fn finish(self) -> String {
            let request = self.requests.recv().expect("test receives the request");
            self.thread.join().expect("test server exits cleanly");
            request
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
        let cli =
            Cli::try_parse_from(["dokploy", "context", "list"]).expect("command line is valid");
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
        let cli =
            Cli::try_parse_from(["dokploy", "context", "show"]).expect("command line is valid");
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
    async fn generated_imperative_read_outputs_the_raw_json_response() {
        let server = TestServer::respond_with_json(r#"{"projectId":"project-1","extra":true}"#);
        let temporary_directory = tempfile::tempdir().expect("temporary directory is available");
        let repository =
            ConfigRepository::new(temporary_directory.path().join("missing-config.toml"));
        let credentials = MemoryCredentialStore::default();
        let secret = "test-api-key";
        let cli = Cli::try_parse_from([
            "dokploy",
            "--url",
            &server.url,
            "--api-key",
            secret,
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
        let repository =
            ConfigRepository::new(temporary_directory.path().join("missing-config.toml"));
        let credentials = MemoryCredentialStore::default();
        let cli = Cli::try_parse_from([
            "dokploy",
            "--url",
            &server.url,
            "--api-key",
            "test-api-key",
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
        let repository =
            ConfigRepository::new(temporary_directory.path().join("missing-config.toml"));
        let credentials = MemoryCredentialStore::default();
        let cli = Cli::try_parse_from([
            "dokploy",
            "--url",
            &server.url,
            "--api-key",
            "test-api-key",
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
        let repository =
            ConfigRepository::new(temporary_directory.path().join("missing-config.toml"));
        let credentials = MemoryCredentialStore::default();
        let cli = Cli::try_parse_from([
            "dokploy",
            "--url",
            &server.url,
            "--api-key",
            "test-api-key",
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
}
