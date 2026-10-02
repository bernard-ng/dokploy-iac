//! Instance settings: tag import, apply, state scope, and project association.

mod support;

use std::fs;
use std::path::Path;

use clap::Parser;
use dokploy_cli::cli::Cli;
use dokploy_cli::config::ConfigRepository;
use dokploy_cli::credentials::{ApiKey, CredentialStore, CredentialStoreError};
use dokploy_cli::executor::{apply_workspace, destroy_workspace_with_approval};
use dokploy_cli::import::{ImportError, SettingsImportRequest, import_settings};
use dokploy_cli::planning::plan_workspace;
use dokploy_cli::{execute, execute_with_input};
use dokploy_state::{InstanceIdentity, ResourceAddress, StateFile, StateScope, StateStore};
use support::world::World;
use support::{Reply, Router, ok, status, switch_after};

const SETTINGS: &str = "dokploy.settings.yaml";

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("test address is valid")
}

fn tag(id: &str, name: &str, color: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "tagId": id, "name": name, "color": color })
}

fn list(tags: &[serde_json::Value]) -> String {
    serde_json::Value::Array(tags.to_vec()).to_string()
}

fn settings_state(router: &Router, workspace: &Path) -> StateFile {
    StateStore::with_scope(
        workspace,
        InstanceIdentity::parse(&router.url).unwrap(),
        StateScope::Settings,
    )
    .unwrap()
    .inspect()
    .unwrap()
    .expect("settings state was written")
}

fn written(workspace: &Path) -> String {
    fs::read_to_string(workspace.join(SETTINGS)).expect("settings document was written")
}

async fn import(router: &Router, workspace: &Path) -> Result<usize, ImportError> {
    import_settings(
        &router.client(),
        SettingsImportRequest {
            config_file: workspace.join(SETTINGS),
        },
    )
    .await
    .map(|report| report.resource_count())
}

fn only_reads(router: &Router) {
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
    assert!(
        router
            .requests()
            .iter()
            .all(|request| request.starts_with("GET ")),
        "import must never mutate"
    );
}

#[tokio::test]
async fn settings_import_adopts_tags_into_scoped_state_and_plans_clean() {
    let router = Router::start(vec![(
        "GET /api/tag.all",
        vec![ok(list(&[
            tag("tag-2", "Needs Review", None),
            tag("tag-1", "prod", Some("#e11d48")),
        ]))],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    let count = import(&router, workspace.path())
        .await
        .expect("settings import");

    assert_eq!(count, 2);
    only_reads(&router);
    let source = written(workspace.path());
    assert!(source.contains("settings:"), "{source}");
    assert!(!source.contains("project:"), "{source}");
    assert!(source.contains("#e11d48"), "{source}");
    assert!(
        source.contains("Needs Review"),
        "a name outside the grammar is written explicitly\n{source}"
    );
    let state = settings_state(&router, workspace.path());
    assert_eq!(state.scope(), StateScope::Settings);
    assert!(
        workspace.path().join(".dokploy/settings").is_dir(),
        "settings state lives apart from project state"
    );
    assert!(!workspace.path().join(".dokploy/state.json").exists());
    let stored = state.resource(&address("tag.prod")).unwrap();
    assert!(stored.is_protected());
    assert_eq!(stored.remote_id().as_str(), "tag-1");

    let plan = plan_workspace(&router.client(), &workspace.path().join(SETTINGS))
        .await
        .expect("a fresh plan succeeds");
    assert!(plan.complete() && plan.applyable(), "{plan:?}");
    assert!(plan.changes().is_empty(), "{plan:?}");
}

#[tokio::test]
async fn settings_import_fails_closed_on_shared_names_and_existing_workspaces() {
    let router = Router::start(vec![(
        "GET /api/tag.all",
        vec![ok(list(&[
            tag("tag-1", "prod", None),
            tag("tag-2", "prod", None),
        ]))],
    )]);
    let workspace = tempfile::tempdir().unwrap();
    // The SDK rejects a contradictory collection before the importer sees it.
    import(&router, workspace.path())
        .await
        .expect_err("shared names are never guessed");
    assert!(!workspace.path().join(SETTINGS).exists());
    assert!(!workspace.path().join(".dokploy").exists());

    let router = Router::start(vec![("GET /api/tag.all", vec![ok("[]")])]);
    let workspace = tempfile::tempdir().unwrap();
    import(&router, workspace.path())
        .await
        .expect("an empty instance imports");
    assert!(matches!(
        import(&router, workspace.path()).await,
        Err(ImportError::ConfigExists)
    ));
}

#[tokio::test]
async fn a_tag_collection_that_changes_during_import_writes_nothing() {
    let router = Router::start(vec![(
        "GET /api/tag.all",
        vec![
            ok(list(&[tag("tag-1", "prod", None)])),
            ok(list(&[
                tag("tag-1", "prod", None),
                tag("tag-2", "new", None),
            ])),
        ],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("a moving collection is refused");

    assert!(matches!(error, ImportError::RemoteChanged), "{error}");
    assert!(!workspace.path().join(SETTINGS).exists());
}

fn workspace_with(source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join(SETTINGS);
    fs::write(&file, source).unwrap();
    (directory, file)
}

const ONE_TAG: &str =
    "version: 1\nsettings:\n  server:\n    tags:\n      prod:\n        color: \"#e11d48\"\n";

#[tokio::test]
async fn apply_creates_a_tag_then_converges_and_destroy_removes_it() {
    let created = tag("tag-1", "prod", Some("#e11d48"));
    let router = Router::start(vec![
        (
            "GET /api/tag.all",
            vec![Reply::Dynamic(std::sync::Arc::new({
                let present = ok(list(std::slice::from_ref(&created)));
                move |log| {
                    let seen = |prefix: &str| log.iter().any(|request| request.starts_with(prefix));
                    if seen("POST /api/tag.remove") || !seen("POST /api/tag.create") {
                        ok("[]")
                    } else {
                        present.clone()
                    }
                }
            }))],
        ),
        ("POST /api/tag.create", vec![ok(created.to_string())]),
        ("POST /api/tag.remove", vec![ok("{}")]),
    ]);
    let (directory, file) = workspace_with(ONE_TAG);
    let client = router.client();

    let summary = apply_workspace(&client, &file).await.expect("apply");

    assert_eq!(summary.applied(), 1);
    let create = router.matching("POST /api/tag.create");
    assert_eq!(create.len(), 1);
    assert!(create[0].contains(r#""name":"prod""#), "{}", create[0]);
    assert!(create[0].contains("#e11d48"), "{}", create[0]);
    let state = settings_state(&router, directory.path());
    assert_eq!(
        state
            .resource(&address("tag.prod"))
            .unwrap()
            .remote_id()
            .as_str(),
        "tag-1"
    );

    let again = apply_workspace(&client, &file)
        .await
        .expect("converged apply");
    assert_eq!(again.applied(), 0);
    assert_eq!(router.matching("POST /api/tag.create").len(), 1);

    let destroyed = destroy_workspace_with_approval(&client, &file, |_| Ok(true))
        .await
        .expect("destroy");
    assert_eq!(destroyed.applied(), 1);
    let removed = router.matching("POST /api/tag.remove");
    assert_eq!(removed.len(), 1);
    assert!(removed[0].contains(r#""tagId":"tag-1""#), "{}", removed[0]);
}

#[tokio::test]
async fn apply_renames_and_recolors_a_tag_in_place() {
    let before = tag("tag-1", "prod", Some("#e11d48"));
    let after = tag("tag-1", "prod", Some("#16a34a"));
    let router = Router::start(vec![
        (
            "GET /api/tag.all",
            vec![switch_after(
                "POST /api/tag.update",
                ok(list(std::slice::from_ref(&before))),
                ok(list(std::slice::from_ref(&after))),
            )],
        ),
        ("POST /api/tag.update", vec![ok("{}")]),
    ]);
    let (directory, file) = workspace_with(ONE_TAG);
    // Track the existing tag first, as an import would.
    let imported = tempfile::tempdir().unwrap();
    import(&router, imported.path()).await.expect("import");
    fs::copy(imported.path().join(SETTINGS), &file).unwrap();
    copy_dir(
        &imported.path().join(".dokploy"),
        &directory.path().join(".dokploy"),
    );
    fs::write(
        &file,
        written(imported.path()).replace("#e11d48", "#16a34a"),
    )
    .unwrap();

    let summary = apply_workspace(&router.client(), &file)
        .await
        .expect("update applies");

    assert_eq!(summary.applied(), 1);
    let update = router.matching("POST /api/tag.update");
    assert_eq!(update.len(), 1);
    assert!(update[0].contains(r#""tagId":"tag-1""#), "{}", update[0]);
    assert!(update[0].contains("#16a34a"), "{}", update[0]);
    assert!(router.matching("POST /api/tag.create").is_empty());
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn an_unreadable_tag_collection_blocks_the_plan_and_mutates_nothing() {
    let router = Router::start(vec![(
        "GET /api/tag.all",
        vec![status("500 Internal Server Error", r#"{"message":"boom"}"#)],
    )]);
    let (_directory, file) = workspace_with(ONE_TAG);

    let result = apply_workspace(&router.client(), &file).await;

    assert!(result.is_err());
    assert!(
        router
            .requests()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

struct NoCredentials;

impl CredentialStore for NoCredentials {
    fn get(&self, _context: &str) -> Result<Option<ApiKey>, CredentialStoreError> {
        Ok(None)
    }

    fn set(&self, _context: &str, _api_key: &ApiKey) -> Result<(), CredentialStoreError> {
        Ok(())
    }

    fn delete(&self, _context: &str) -> Result<(), CredentialStoreError> {
        Ok(())
    }
}

async fn run(
    router: &Router,
    workspace: &Path,
    arguments: &[&str],
) -> (Result<(), String>, String) {
    let mut command = vec!["dokploy", "--url", &router.url, "--api-key", "test-key"];
    command.extend_from_slice(arguments);
    let cli = Cli::try_parse_from(command).expect("arguments are valid");
    let repository = ConfigRepository::new(workspace.join("config.toml"));
    let mut input = std::io::empty();
    let mut output = Vec::new();
    let result = execute_with_input(cli, &repository, &NoCredentials, &mut input, &mut output)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string());

    (result, String::from_utf8(output).unwrap())
}

#[tokio::test]
async fn the_cli_imports_settings_and_validates_the_result() {
    let router = Router::start(vec![(
        "GET /api/tag.all",
        vec![ok(list(&[tag("tag-1", "prod", None)]))],
    )]);
    let workspace = tempfile::tempdir().unwrap();
    let file = workspace.path().join(SETTINGS);

    let (result, output) = run(
        &router,
        workspace.path(),
        &["import", "settings", "--file", file.to_str().unwrap()],
    )
    .await;

    result.expect("the CLI import succeeds");
    assert!(
        output.contains("Import complete: 1 resource(s) tracked."),
        "{output}"
    );
    assert!(output.contains("Instance settings"), "{output}");
    let (result, output) = run(
        &router,
        workspace.path(),
        &["validate", "--file", file.to_str().unwrap()],
    )
    .await;
    result.expect("the imported document validates");
    assert!(output.contains("valid"), "{output}");
}

#[tokio::test]
async fn project_tags_are_assigned_and_removed_by_resolved_identity() {
    let mut before = World::new("project-1", "Platform");
    before.environment("env-prod", "production");
    before
        .tag("tag-keep", "keep", None, true)
        .tag("tag-drop", "drop", None, true)
        .tag("tag-add", "add", None, false);
    let mut after = World::new("project-1", "Platform");
    after.environment("env-prod", "production");
    after
        .tag("tag-keep", "keep", None, true)
        .tag("tag-drop", "drop", None, false)
        .tag("tag-add", "add", None, true);
    let mutated = "POST /api/tag.";
    let staged = |before: serde_json::Value, after: serde_json::Value| {
        switch_after(mutated, ok(before.to_string()), ok(after.to_string()))
    };
    let router = before.router_with(vec![
        (
            "GET /api/project.all".to_owned(),
            vec![staged(
                serde_json::json!([before.project_record()]),
                serde_json::json!([after.project_record()]),
            )],
        ),
        (
            World::key("project.one", "projectId", "project-1"),
            vec![staged(before.project_record(), after.project_record())],
        ),
        ("POST /api/tag.assignToProject".to_owned(), vec![ok("{}")]),
        ("POST /api/tag.removeFromProject".to_owned(), vec![ok("{}")]),
    ]);
    let workspace = tempfile::tempdir().unwrap();
    let file = workspace.path().join("dokploy.yaml");
    dokploy_cli::import::import_project(
        &router.client(),
        dokploy_cli::import::ImportRequest {
            project_id: "project-1".to_owned(),
            config_file: file.clone(),
        },
    )
    .await
    .expect("the project is tracked with its current tags");
    let source = fs::read_to_string(&file).unwrap();
    assert!(
        source.contains("drop") && source.contains("keep"),
        "{source}"
    );
    fs::write(&file, source.replace("drop", "add")).unwrap();

    let summary = apply_workspace(&router.client(), &file)
        .await
        .expect("the association converges");

    assert_eq!(summary.applied(), 1);
    let assigned = router.matching("POST /api/tag.assignToProject");
    let removed = router.matching("POST /api/tag.removeFromProject");
    assert_eq!(assigned.len(), 1, "{assigned:?}");
    assert_eq!(removed.len(), 1, "{removed:?}");
    assert!(
        assigned[0].contains(r#""tagId":"tag-add""#),
        "{}",
        assigned[0]
    );
    assert!(
        removed[0].contains(r#""tagId":"tag-drop""#),
        "{}",
        removed[0]
    );
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
}

#[tokio::test]
async fn an_unknown_desired_project_tag_blocks_the_plan_before_any_mutation() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world.tag("tag-keep", "keep", None, true);
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();
    let file = workspace.path().join("dokploy.yaml");
    dokploy_cli::import::import_project(
        &router.client(),
        dokploy_cli::import::ImportRequest {
            project_id: "project-1".to_owned(),
            config_file: file.clone(),
        },
    )
    .await
    .expect("import");
    let source = fs::read_to_string(&file).unwrap();
    fs::write(&file, source.replace("keep", "missing")).unwrap();

    let plan = plan_workspace(&router.client(), &file)
        .await
        .expect("plans");

    assert!(!plan.applyable(), "{plan:?}");
    assert!(
        router
            .requests()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

#[tokio::test]
async fn state_commands_follow_the_document_scope() {
    let router = Router::start(vec![(
        "GET /api/tag.all",
        vec![ok(list(&[tag("tag-1", "prod", None)]))],
    )]);
    let workspace = tempfile::tempdir().unwrap();
    import(&router, workspace.path()).await.expect("import");
    let file = workspace.path().join(SETTINGS);
    let repository = ConfigRepository::new(workspace.path().join("config.toml"));

    let state_command = |arguments: Vec<&str>| {
        let mut command = vec![
            "dokploy",
            "--url",
            router.url.as_str(),
            "state",
            "--file",
            file.to_str().unwrap(),
        ];
        command.extend(arguments);
        Cli::try_parse_from(command).expect("arguments are valid")
    };
    let mut output = Vec::new();
    execute(
        state_command(vec!["list"]),
        &repository,
        &NoCredentials,
        &mut output,
    )
    .await
    .expect("state list succeeds");
    assert_eq!(String::from_utf8(output).unwrap().trim(), "tag.prod");

    let mut output = Vec::new();
    execute(
        state_command(vec!["protect", "tag.prod"]),
        &repository,
        &NoCredentials,
        &mut output,
    )
    .await
    .expect("protect succeeds in the settings lineage");
    assert!(
        settings_state(&router, workspace.path())
            .resource(&address("tag.prod"))
            .unwrap()
            .is_protected()
    );

    // A project address does not exist in the settings lineage.
    let mut output = Vec::new();
    execute(
        state_command(vec!["show", "project.platform"]),
        &repository,
        &NoCredentials,
        &mut output,
    )
    .await
    .expect_err("a project resource is not tracked in settings state");
}
