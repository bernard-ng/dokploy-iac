mod support;

use dokploy_cli::cli::ImportKind;
use dokploy_cli::import::{ImportRequest, import_resource};
use dokploy_cli::planning::plan_workspace;
use dokploy_config::{DokployConfig, Field, MountSourceConfig};
use dokploy_state::{InstanceIdentity, ResourceAddress, StateStore};
use support::{Reply, Router, file_mount, list, ok, status, volume_mount};

const APPLICATION: &str = r#"{"applicationId":"application-1","name":"API Service","appName":"api","environmentId":"environment-1","description":"API","replicas":1}"#;
const POSTGRES: &str = r#"{"postgresId":"postgres-1","name":"Main DB","appName":"main-db","environmentId":"environment-1","databaseName":"app","databaseUser":"app","dockerImage":"postgres:16"}"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"Production West","projectId":"project-1"}"#;
const PROJECT: &str = r#"{"projectId":"project-1","name":"IaC Contract Test","environments":[]}"#;
const TOPOLOGY: &str = r#"[{"projectId":"project-1","name":"IaC Contract Test","environments":[{"environmentId":"environment-1","name":"Production West","isDefault":true,"applications":[{"applicationId":"application-1","name":"API Service"}],"postgres":[],"redis":[]}]}]"#;

fn address(value: &str) -> ResourceAddress {
    value.parse().unwrap()
}

fn base_routes(mount_routes: Vec<(&'static str, Vec<Reply>)>) -> Vec<(&'static str, Vec<Reply>)> {
    let mut routes = vec![
        ("GET /api/project.one", vec![ok(PROJECT)]),
        ("GET /api/project.all", vec![ok(TOPOLOGY)]),
        ("GET /api/environment.one", vec![ok(ENVIRONMENT)]),
        (
            "GET /api/environment.byProjectId",
            vec![ok(
                r#"[{"environmentId":"environment-1","name":"Production West"}]"#,
            )],
        ),
        ("GET /api/application.one", vec![ok(APPLICATION)]),
        (
            "GET /api/application.search",
            vec![ok(
                r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"API Service"}],"total":1}"#,
            )],
        ),
        ("GET /api/postgres.one", vec![ok(POSTGRES)]),
    ];
    routes.extend(mount_routes);
    routes
}

async fn import(
    router: &Router,
    workspace: &std::path::Path,
) -> Result<usize, dokploy_cli::import::ImportError> {
    import_resource(
        &router.client(),
        ImportRequest {
            kind: ImportKind::Mount,
            remote_id: "mount-1".to_owned(),
            address: address("mount.data"),
            config_file: workspace.join("dokploy.yaml"),
        },
    )
    .await
}

fn read_state(router: &Router, workspace: &std::path::Path) -> dokploy_state::StateFile {
    StateStore::new(workspace, InstanceIdentity::parse(&router.url).unwrap())
        .unwrap()
        .inspect()
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn application_volume_mount_import_is_protected_and_immediately_convergent() {
    let mount = volume_mount("mount-1", "application-1", "/data", "api-data");
    let router = Router::start(base_routes(vec![
        ("GET /api/mounts.one", vec![ok(&mount)]),
        ("GET /api/mounts.listByServiceId", vec![ok(list([&mount]))]),
    ]));
    let workspace = tempfile::tempdir().unwrap();

    let count = import(&router, workspace.path()).await.unwrap();
    let plan = plan_workspace(&router.client(), &workspace.path().join("dokploy.yaml"))
        .await
        .unwrap();

    assert_eq!(
        count, 4,
        "project, environment, target application, and Mount"
    );
    assert!(plan.complete() && plan.applyable() && plan.changes().is_empty());
    let source = std::fs::read_to_string(workspace.path().join("dokploy.yaml")).unwrap();
    let config = DokployConfig::parse(&source).unwrap();
    let resource = config.resource(&address("mount.data")).unwrap();
    assert_eq!(resource.lifecycle().protect(), &Field::Set(true));
    let mount_config = resource.as_mount().unwrap();
    assert_eq!(mount_config.target(), &address("application.api-service"));
    assert_eq!(
        config.parent_of(&address("mount.data")),
        Some(&address("environment.production-west"))
    );
    let state = read_state(&router, workspace.path());
    let imported = state.resource(&address("mount.data")).unwrap();
    assert!(imported.is_protected());
    assert_eq!(
        imported.dependencies(),
        &[address("application.api-service")]
    );
    assert_eq!(
        imported.containment(),
        Some(&address("environment.production-west"))
    );
    assert_eq!(
        imported.last_applied().as_json(),
        &serde_json::json!({
            "target": "application.api-service",
            "mount_type": "volume",
            "mount_path": "/data",
            "volume_name": "api-data"
        })
    );
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
    assert!(
        router
            .requests()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

#[tokio::test]
async fn file_mount_import_leaves_content_unmanaged_and_never_reads_it() {
    let mount = file_mount("mount-1", "application-1", "/etc/app.conf", "app.conf");
    let router = Router::start(base_routes(vec![
        ("GET /api/mounts.one", vec![ok(&mount)]),
        ("GET /api/mounts.listByServiceId", vec![ok(list([&mount]))]),
    ]));
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path()).await.unwrap();
    let plan = plan_workspace(&router.client(), &workspace.path().join("dokploy.yaml"))
        .await
        .unwrap();

    assert!(plan.changes().is_empty() && plan.applyable());
    let source = std::fs::read_to_string(workspace.path().join("dokploy.yaml")).unwrap();
    assert!(!source.contains("content"));
    assert!(!source.contains("remote-content-is-never-modeled"));
    let config = DokployConfig::parse(&source).unwrap();
    assert!(matches!(
        config
            .resource(&address("mount.data"))
            .unwrap()
            .as_mount()
            .unwrap()
            .source(),
        MountSourceConfig::File {
            content: Field::Unmanaged,
            ..
        }
    ));
    let state_text = std::fs::read_to_string(workspace.path().join(".dokploy/state.json")).unwrap();
    assert!(!state_text.contains("remote-content-is-never-modeled"));
    assert!(!state_text.contains("\"content\""));
}

#[tokio::test]
async fn database_target_is_imported_with_its_ancestry_and_dependency() {
    let mount = r#"{"mountId":"mount-1","type":"bind","mountPath":"/backup","serviceType":"postgres","postgresId":"postgres-1","hostPath":"/srv/backup","volumeName":null,"filePath":null}"#;
    let router = Router::start(base_routes(vec![
        ("GET /api/mounts.one", vec![ok(mount)]),
        (
            "GET /api/mounts.listByServiceId",
            vec![ok(list([mount.to_owned()]))],
        ),
    ]));
    let workspace = tempfile::tempdir().unwrap();

    let count = import(&router, workspace.path()).await.unwrap();

    assert_eq!(count, 4);
    let state = read_state(&router, workspace.path());
    let imported = state.resource(&address("mount.data")).unwrap();
    assert_eq!(imported.dependencies(), &[address("postgres.main-db")]);
    assert!(state.resource(&address("postgres.main-db")).is_some());
    assert!(router.matching("GET /api/mounts.listByServiceId")[0].contains("serviceType=postgres"));
    assert!(
        router
            .requests()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

#[tokio::test]
async fn import_fails_closed_on_identity_collection_or_shape_contradictions() {
    let mount = volume_mount("mount-1", "application-1", "/data", "api-data");

    // The direct read returns a different identity.
    let other = volume_mount("mount-2", "application-1", "/data", "api-data");
    let router = Router::start(base_routes(vec![
        ("GET /api/mounts.one", vec![ok(&other)]),
        ("GET /api/mounts.listByServiceId", vec![ok(list([&other]))]),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&router, workspace.path()).await.is_err());
    assert!(!workspace.path().join("dokploy.yaml").exists());

    // The authoritative collection does not contain the direct record.
    let router = Router::start(base_routes(vec![
        ("GET /api/mounts.one", vec![ok(&mount)]),
        ("GET /api/mounts.listByServiceId", vec![ok("[]")]),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&router, workspace.path()).await.is_err());
    assert!(!workspace.path().join("dokploy.yaml").exists());

    // A bind mount without its host path is not representable.
    let broken = r#"{"mountId":"mount-1","type":"bind","mountPath":"/data","serviceType":"application","applicationId":"application-1","hostPath":null,"volumeName":null,"filePath":null}"#;
    let router = Router::start(base_routes(vec![
        ("GET /api/mounts.one", vec![ok(broken)]),
        (
            "GET /api/mounts.listByServiceId",
            vec![ok(list([broken.to_owned()]))],
        ),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&router, workspace.path()).await.is_err());

    // A missing mount surfaces as an error, not an empty import.
    let router = Router::start(base_routes(vec![(
        "GET /api/mounts.one",
        vec![status("404 Not Found", r#"{"message":"Mount not found"}"#)],
    )]));
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&router, workspace.path()).await.is_err());
    assert!(!workspace.path().join(".dokploy/state.json").exists());
}

#[test]
fn cli_accepts_the_mount_import_kind() {
    use clap::Parser;
    let parsed = dokploy_cli::cli::Cli::try_parse_from([
        "dokploy",
        "import",
        "mount",
        "mount-1",
        "--as",
        "mount.data",
    ])
    .expect("mount import parses");
    assert!(matches!(
        parsed.command,
        dokploy_cli::cli::Command::Import {
            kind: Some(ImportKind::Mount),
            ..
        }
    ));
}
