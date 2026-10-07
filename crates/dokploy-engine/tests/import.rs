mod common;

use std::sync::Arc;

use dokploy_engine::{EnvironmentImportMode, ImportOptions, ImportSelection};
use dokploy_sim::Sim;
use serde_json::json;

#[tokio::test]
async fn import_all_builds_settings_and_nested_project_documents_that_converge() {
    let specs = Arc::new(common::specs());
    let sim = Sim::new(specs.clone());
    let organization_id = sim.objects("organization")[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sim.patch(
        "organization",
        &organization_id,
        &json!({ "name": "Legalter" }),
    );
    let web_server_id = sim.objects("web_server")[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sim.patch(
        "web_server",
        &web_server_id,
        &json!({
            "host": null,
            "https": false,
            "certificateType": "none",
            "letsEncryptEmail": null,
            "serverIp": "127.0.0.1",
            "buildsConcurrency": 2,
            "requestLogging": false,
            "enableDockerCleanup": true,
            "logCleanupCron": "0 0 * * *",
            "traefikDashboard": false,
            "additionalPorts": [],
            "traefikEnv": null,
            "traefikConfig": null,
            "webServerTraefikConfig": null,
            "middlewareTraefikConfig": null,
            "remoteServersOnly": true,
            "enforceSSO": true
        }),
    );
    sim.seed(
        "registry",
        &json!({
            "registryName": "Production Registry",
            "registryType": "cloud",
            "registryUrl": "ghcr.io",
            "username": "ci",
            "password": "registry-secret-canary"
        }),
    );
    let project = sim.seed(
        "project",
        &json!({
            "name": "Customer Portal",
            "description": "Imported project"
        }),
    );
    let environment = sim.seed(
        "environment",
        &json!({
            "projectId": project,
            "name": "Production",
            "description": "Primary environment",
            "env": "PUBLIC_ORIGIN=https://example.test\nAPI_TOKEN=environment-secret-canary"
        }),
    );
    sim.seed(
        "application",
        &json!({
            "environmentId": environment,
            "name": "Web API",
            "description": "Imported application",
            "replicas": 2,
            "env": "PUBLIC_MODE=enabled\nAPP_TOKEN=application-secret-canary"
        }),
    );

    let engine = common::engine(&sim);
    let bundle = engine
        .import(
            ImportSelection::All,
            &engine.fingerprinter(common::key()),
            &ImportOptions {
                environment: EnvironmentImportMode::Auto,
                ..ImportOptions::default()
            },
        )
        .await
        .expect("the remote instance imports and its first plans are empty");

    assert_eq!(
        bundle
            .documents()
            .iter()
            .map(|document| document.path())
            .collect::<Vec<_>>(),
        ["settings.yaml", "projects/customer-portal.yaml"]
    );
    let settings = bundle.documents()[0].render();
    let project = bundle.documents()[1].render();
    assert!(settings.contains("production-registry:"), "{settings}");
    assert!(settings.contains("password:\n        file:"), "{settings}");
    assert!(
        project.contains("environments:\n    production:"),
        "{project}"
    );
    assert!(
        project.contains("applications:\n        web-api:"),
        "{project}"
    );
    assert!(
        project.contains("PUBLIC_ORIGIN: \"https://example.test\""),
        "{project}"
    );
    assert!(
        project.contains("API_TOKEN:\n          secret:\n            file:"),
        "{project}"
    );
    assert!(
        project.contains("env_file:\n            file:"),
        "{project}"
    );
    assert!(project.contains("PUBLIC_MODE: enabled"), "{project}");
    assert!(!settings.contains("registry-secret-canary"));
    assert!(!project.contains("environment-secret-canary"));
    assert!(!project.contains("application-secret-canary"));
    assert!(
        bundle
            .files()
            .iter()
            .any(|file| file.path().starts_with("secrets/settings/"))
    );
    assert_eq!(
        bundle
            .files()
            .iter()
            .filter(|file| file.is_secret())
            .count(),
        3
    );
    assert_eq!(
        bundle.manifest(),
        "version: 1\ndocuments:\n  - settings.yaml\n  - projects/customer-portal.yaml\n"
    );
    assert!(
        bundle
            .unsupported()
            .iter()
            .any(|item| item.item.ends_with(":enforceSSO")),
        "excluded SSO fields are reported instead of silently dropped"
    );

    let destination = tempfile::tempdir().unwrap();
    let workspace = destination.path().join("workspace");
    bundle
        .persist(&workspace)
        .expect("the proved bundle persists");
    assert_eq!(
        std::fs::read_to_string(workspace.join("dokploy.workspace.yaml")).unwrap(),
        bundle.manifest()
    );
    assert!(workspace.join(".dokploy/settings/state.json").is_file());
    assert!(
        workspace
            .join(".dokploy/projects/customer-portal/state.json")
            .is_file()
    );
    assert!(
        std::fs::read_to_string(workspace.join("settings.yaml"))
            .unwrap()
            .contains("password:\n        file:")
    );
    assert!(
        bundle
            .persist(&workspace)
            .expect_err("an import never overwrites its workspace")
            .to_string()
            .contains("already exists")
    );
}

#[tokio::test]
async fn project_import_requires_the_selected_remote_identity() {
    let specs = Arc::new(common::specs());
    let sim = Sim::new(specs);
    let engine = common::engine(&sim);

    let error = engine
        .import(
            ImportSelection::Project("missing".to_owned()),
            &engine.fingerprinter(common::key()),
            &ImportOptions::default(),
        )
        .await
        .expect_err("a missing project is refused");

    assert!(error.to_string().contains("was not found"));
}

#[tokio::test]
async fn project_import_allocates_a_domain_key_from_its_host() {
    let specs = Arc::new(common::specs());
    let sim = Sim::new(specs);
    let project = sim.seed("project", &json!({ "name": "Shop" }));
    let environment = sim.seed(
        "environment",
        &json!({ "projectId": project, "name": "Production" }),
    );
    let application = sim.seed(
        "application",
        &json!({ "environmentId": environment, "name": "API" }),
    );
    sim.seed(
        "domain",
        &json!({
            "applicationId": application,
            "domainType": "application",
            "host": "api.example.test",
            "path": "/",
            "internalPath": "/",
            "stripPath": false,
            "port": 3000,
            "https": false,
            "certificateType": "none",
            "customCertResolver": null,
            "customEntrypoint": null,
            "middlewares": [],
            "forwardAuthEnabled": false,
            "enabled": true
        }),
    );
    let engine = common::engine(&sim);

    let bundle = engine
        .import(
            ImportSelection::Project(project),
            &engine.fingerprinter(common::key()),
            &ImportOptions::default(),
        )
        .await
        .expect("a keyless domain uses its natural collision value as its document key");

    let document = bundle.documents()[0].render();
    assert!(
        document.contains("domains:\n            api-example-test:"),
        "{document}"
    );
}

#[tokio::test]
async fn import_rejects_a_secret_directory_outside_the_workspace_before_inventory() {
    let specs = Arc::new(common::specs());
    let sim = Sim::new(specs);
    let engine = common::engine(&sim);

    let error = engine
        .import(
            ImportSelection::All,
            &engine.fingerprinter(common::key()),
            &ImportOptions {
                secrets_directory: "../secrets".to_owned(),
                ..ImportOptions::default()
            },
        )
        .await
        .expect_err("secret output stays below the workspace");

    assert!(error.to_string().contains("workspace-relative"));
    assert!(
        sim.requests().is_empty(),
        "validation runs before inventory"
    );
}
