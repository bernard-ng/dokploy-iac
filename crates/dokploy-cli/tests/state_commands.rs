use std::collections::BTreeMap;
use std::fs;

use clap::Parser;
use dokploy_cli::cli::Cli;
use dokploy_cli::config::ConfigRepository;
use dokploy_cli::credentials::{ApiKey, CredentialStore, CredentialStoreError};
use dokploy_cli::execute;
use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, StateFile, StateStore,
};
use semver::Version;

#[derive(Default)]
struct MemoryCredentialStore {
    values: BTreeMap<String, ApiKey>,
}

struct PanicCredentialStore;

impl CredentialStore for PanicCredentialStore {
    fn get(&self, _context: &str) -> Result<Option<ApiKey>, CredentialStoreError> {
        panic!("local state commands must not read API keys")
    }

    fn set(&self, _context: &str, _api_key: &ApiKey) -> Result<(), CredentialStoreError> {
        panic!("local state commands must not write API keys")
    }

    fn delete(&self, _context: &str) -> Result<(), CredentialStoreError> {
        panic!("local state commands must not delete API keys")
    }
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

#[tokio::test]
async fn state_list_and_show_inspect_one_workspace_without_exposing_managed_values() {
    const VALUE_CANARY: &str = "managed-value-must-not-be-rendered";

    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    let instance =
        InstanceIdentity::parse("https://deploy.example.com").expect("instance is valid");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("store is valid");
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .expect("writer starts")
        .checkpoint(ExpectedState::absent(), &state)
        .expect("state is initialized");
    let initial = state.clone();
    state
        .upsert_resource(
            address("project.platform"),
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new("project-1").expect("remote ID is valid"),
                true,
                ManagedInputs::try_from_json(serde_json::json!({"description": VALUE_CANARY}))
                    .expect("managed inputs are valid"),
                None,
                Vec::new(),
            ),
        )
        .expect("resource state is valid");
    store
        .begin_write()
        .expect("writer starts")
        .checkpoint(ExpectedState::from_state(&initial), &state)
        .expect("state is persisted");
    let repository = ConfigRepository::new(workspace.path().join("missing-config.toml"));
    let credentials = MemoryCredentialStore::default();

    let list = Cli::try_parse_from([
        "dokploy",
        "--url",
        "https://deploy.example.com",
        "--api-key",
        "test-api-key",
        "state",
        "--file",
        config_file.to_str().expect("fixture path is UTF-8"),
        "list",
    ])
    .expect("state list command line is valid");
    let mut list_output = Vec::new();
    execute(list, &repository, &credentials, &mut list_output)
        .await
        .expect("state list succeeds");
    assert_eq!(
        String::from_utf8(list_output).expect("output is UTF-8"),
        "project.platform\n"
    );

    let show = Cli::try_parse_from([
        "dokploy",
        "--url",
        "https://deploy.example.com",
        "--api-key",
        "test-api-key",
        "state",
        "--file",
        config_file.to_str().expect("fixture path is UTF-8"),
        "show",
        "project.platform",
    ])
    .expect("state show command line is valid");
    let mut show_output = Vec::new();
    execute(show, &repository, &credentials, &mut show_output)
        .await
        .expect("state show succeeds");

    let show_output = String::from_utf8(show_output).expect("output is UTF-8");
    assert!(show_output.contains("address: project.platform\n"));
    assert!(show_output.contains("remote id: project-1\n"));
    assert!(show_output.contains("protected: true\n"));
    assert!(show_output.contains("  - description\n"));
    assert!(!show_output.contains(VALUE_CANARY));
}

#[tokio::test]
async fn state_repair_commands_are_local_atomic_and_credential_free() {
    let workspace = tempfile::tempdir().expect("temporary workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");
    let context_file = workspace.path().join("config.toml");
    fs::write(
        &context_file,
        r#"current_context = "production"

[contexts.production]
url = "https://deploy.example.com"
"#,
    )
    .expect("context fixture is writable");
    let instance =
        InstanceIdentity::parse("https://deploy.example.com").expect("instance is valid");
    let store = StateStore::new(workspace.path(), instance.clone()).expect("store is valid");
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .expect("writer starts")
        .checkpoint(ExpectedState::absent(), &state)
        .expect("state is initialized");
    let initial = state.clone();
    state
        .upsert_resource(
            address("project.platform"),
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new("project-1").expect("remote ID is valid"),
                false,
                ManagedInputs::try_from_json(serde_json::json!({}))
                    .expect("managed inputs are valid"),
                None,
                Vec::new(),
            ),
        )
        .expect("resource state is valid");
    store
        .begin_write()
        .expect("writer starts")
        .checkpoint(ExpectedState::from_state(&initial), &state)
        .expect("state is persisted");
    let repository = ConfigRepository::new(context_file);

    let protect =
        run_state_command(&repository, &config_file, &["protect", "project.platform"]).await;
    assert_eq!(protect, "Resource project.platform is protected.\n");
    let moved = run_state_command(
        &repository,
        &config_file,
        &["mv", "project.platform", "project.control-plane"],
    )
    .await;
    assert_eq!(
        moved,
        "Moved state from project.platform to project.control-plane. Dokploy was not changed.\n"
    );
    let unprotected = run_state_command(
        &repository,
        &config_file,
        &["unprotect", "project.control-plane"],
    )
    .await;
    assert_eq!(
        unprotected,
        "Resource project.control-plane is unprotected.\n"
    );
    let removed =
        run_state_command(&repository, &config_file, &["rm", "project.control-plane"]).await;
    assert_eq!(
        removed,
        "Removed project.control-plane from local state. Dokploy was not changed.\n"
    );

    assert!(
        store
            .inspect()
            .expect("state is readable")
            .expect("state exists")
            .resources()
            .is_empty()
    );
}

async fn run_state_command(
    repository: &ConfigRepository,
    config_file: &std::path::Path,
    command: &[&str],
) -> String {
    let mut arguments = vec![
        "dokploy",
        "state",
        "--file",
        config_file.to_str().expect("fixture path is UTF-8"),
    ];
    arguments.extend_from_slice(command);
    let cli = Cli::try_parse_from(arguments).expect("state command line is valid");
    let mut output = Vec::new();

    execute(cli, repository, &PanicCredentialStore, &mut output)
        .await
        .expect("state command succeeds");

    String::from_utf8(output).expect("state output is UTF-8")
}

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
}
