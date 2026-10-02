//! Whole-project import: one recursive, read-only pass that must plan clean.

mod support;

use std::path::Path;

use clap::Parser;
use dokploy_cli::cli::Cli;
use dokploy_cli::config::ConfigRepository;
use dokploy_cli::credentials::{ApiKey, CredentialStore, CredentialStoreError};
use dokploy_cli::execute_with_input;
use dokploy_cli::import::{
    ImportError, ImportPrompter, ImportReport, ImportRequest, import_project, select_with_prompter,
};
use dokploy_cli::planning::plan_workspace;
use dokploy_config::DokployConfig;
use dokploy_state::{InstanceIdentity, ResourceAddress, StateFile, StateStore};
use support::world::{Kind, SECRET_CANARY, World};
use support::{Router, ok, status};

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("test address is valid")
}

fn request(workspace: &Path) -> ImportRequest {
    ImportRequest {
        project_id: "project-1".to_owned(),
        config_file: workspace.join("dokploy.yaml"),
    }
}

async fn import(router: &Router, workspace: &Path) -> Result<ImportReport, ImportError> {
    import_project(&router.client(), request(workspace)).await
}

fn config_text(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join("dokploy.yaml")).expect("configuration was written")
}

fn read_state(router: &Router, workspace: &Path) -> StateFile {
    StateStore::new(workspace, InstanceIdentity::parse(&router.url).unwrap())
        .unwrap()
        .inspect()
        .unwrap()
        .expect("state was written")
}

fn assert_nothing_written(workspace: &Path) {
    assert!(
        !workspace.join("dokploy.yaml").exists(),
        "config was written"
    );
    assert!(
        !workspace.join(".dokploy/state.json").exists(),
        "state was written"
    );
}

/// Import is read-only: every request is a GET and every read was routed.
fn assert_read_only(router: &Router) {
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
    assert!(
        router
            .requests()
            .iter()
            .all(|request| request.starts_with("GET ")),
        "import must never mutate"
    );
}

/// The first fresh plan after an import has nothing to change.
async fn assert_plans_clean(router: &Router, workspace: &Path) {
    let plan = plan_workspace(&router.client(), &workspace.join("dokploy.yaml"))
        .await
        .expect("a fresh plan succeeds");
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
    assert!(plan.complete(), "{plan:?}");
    assert!(plan.applyable(), "{plan:?}");
    assert!(plan.changes().is_empty(), "{plan:?}");
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
}

fn assert_secret_free(workspace: &Path) {
    let state = std::fs::read_to_string(workspace.join(".dokploy/state.json")).unwrap();
    for text in [config_text(workspace), state] {
        assert!(!text.contains(SECRET_CANARY), "a secret reached disk");
    }
}

/// One environment holding every service kind and every leaf kind.
fn platform() -> World {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world.destination("dest-1", "offsite");
    {
        let api = world.service("env-prod", Kind::Application, "app-api", "API");
        api.domain("dom-api", "api.example.test")
            .port("port-http", 8080, 80)
            .redirect("red-old", "^/old", "/new")
            .security("sec-admin", "admin")
            .volume_mount("mnt-data", "/data", "api-data")
            .schedule("sch-clean", "cleanup", "0 * * * *", None);
    }
    {
        let stack = world.service("env-prod", Kind::Compose, "cmp-stack", "Stack");
        stack
            .file_mount("mnt-config", "/etc/app.conf", "app.conf")
            .schedule("sch-web", "web-task", "0 1 * * *", Some("web"))
            .schedule("sch-web-late", "web-late", "0 2 * * *", Some("web"));
    }
    world
        .service("env-prod", Kind::Postgres, "pg-main", "Main DB")
        .volume_mount("mnt-pg", "/var/lib/postgresql", "pg-data")
        .backup("bk-pg", "dest-1", "nightly");
    world.service("env-prod", Kind::MySql, "my-legacy", "Legacy");
    world.service("env-prod", Kind::MariaDb, "maria-one", "Maria");
    world.service("env-prod", Kind::Mongo, "mongo-docs", "Docs");
    world.service("env-prod", Kind::LibSql, "lib-edge", "Edge");
    world.service("env-prod", Kind::Redis, "redis-cache", "Cache");
    world
}

#[tokio::test]
async fn a_whole_project_imports_read_only_secret_free_and_plans_clean() {
    let router = platform().router();
    let workspace = tempfile::tempdir().unwrap();

    let report = import(&router, workspace.path())
        .await
        .expect("project imports");
    assert_read_only(&router);
    assert_secret_free(workspace.path());

    // project + environment + 8 services + 1 domain + 1 port + 1 redirect + 1 security
    // + 3 mounts + 3 schedules + 1 backup.
    assert_eq!(report.resource_count(), 1 + 1 + 8 + 4 + 3 + 3 + 1);
    let source = config_text(workspace.path());
    let config = DokployConfig::parse(&source).expect("the written document is canonical");
    for expected in [
        "project.platform",
        "environment.production",
        "application.api",
        "compose.stack",
        "postgres.main-db",
        "mysql.legacy",
        "mariadb.maria",
        "mongo.docs",
        "libsql.edge",
        "redis.cache",
        "domain.api-example-test",
        "port.imported-8080-tcp",
        "redirect.old",
        "security.admin",
        "schedule.cleanup",
        "schedule.web-task",
        "schedule.web-late",
        "backup.main-db-nightly",
    ] {
        assert!(
            config.resource(&address(expected)).is_some(),
            "missing {expected}\n{source}"
        );
    }
    let state = read_state(&router, workspace.path());
    assert_eq!(
        state.serial(),
        0,
        "import is one absent-to-populated checkpoint"
    );
    assert_eq!(state.resources().len(), report.resource_count());

    assert_plans_clean(&router, workspace.path()).await;
}

#[tokio::test]
async fn services_are_protected_and_leaves_depend_on_their_target() {
    let router = platform().router();
    let workspace = tempfile::tempdir().unwrap();
    import(&router, workspace.path()).await.unwrap();
    let state = read_state(&router, workspace.path());

    for protected in [
        "compose.stack",
        "postgres.main-db",
        "mysql.legacy",
        "mariadb.maria",
        "mongo.docs",
        "libsql.edge",
        "redis.cache",
        "port.imported-8080-tcp",
        "redirect.old",
        "security.admin",
        "schedule.cleanup",
        "backup.main-db-nightly",
    ] {
        assert!(
            state.resource(&address(protected)).unwrap().is_protected(),
            "{protected} must be imported protected"
        );
    }
    let domain = state.resource(&address("domain.api-example-test")).unwrap();
    assert_eq!(domain.dependencies(), [address("application.api")]);
    let backup = state.resource(&address("backup.main-db-nightly")).unwrap();
    assert_eq!(backup.dependencies(), [address("postgres.main-db")]);
    let schedule = state.resource(&address("schedule.web-task")).unwrap();
    assert_eq!(schedule.dependencies(), [address("compose.stack")]);
}

#[tokio::test]
async fn the_report_lists_environments_unmanaged_fields_and_no_secret() {
    let router = platform().router();
    let workspace = tempfile::tempdir().unwrap();

    let report = import(&router, workspace.path()).await.unwrap();
    let text = report.to_string();

    assert!(text.contains("Project project.platform"), "{text}");
    assert!(text.contains("environment.production:"), "{text}");
    assert!(text.contains("1 application"), "{text}");
    assert!(text.contains("security passwords"), "{text}");
    assert!(text.contains("compose documents"), "{text}");
    assert!(!text.contains("Renamed"), "no address was renamed\n{text}");
    assert!(!text.contains(SECRET_CANARY));
}

#[tokio::test]
async fn the_same_name_in_two_environments_prefixes_every_member() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world.environment("env-stage", "staging");
    world.service("env-prod", Kind::Application, "app-prod", "api");
    world.service("env-stage", Kind::Application, "app-stage", "api");
    world.service("env-prod", Kind::Postgres, "pg-main", "api");
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    let report = import(&router, workspace.path()).await.unwrap();

    let config = DokployConfig::parse(&config_text(workspace.path())).unwrap();
    assert!(
        config
            .resource(&address("application.production-api"))
            .is_some()
    );
    assert!(
        config
            .resource(&address("application.staging-api"))
            .is_some()
    );
    assert!(
        config.resource(&address("application.api")).is_none(),
        "no member of a collision keeps the bare name"
    );
    assert!(
        config.resource(&address("postgres.api")).is_some(),
        "a different kind never collides"
    );
    let text = report.to_string();
    assert!(
        text.contains("application.production-api (remote: api)"),
        "{text}"
    );
    assert!(
        text.contains("application.staging-api (remote: api)"),
        "{text}"
    );
    assert!(!text.contains("postgres.api (remote"), "{text}");
    assert_plans_clean(&router, workspace.path()).await;
}

#[tokio::test]
async fn leaf_collisions_are_separated_by_their_parent_service() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world
        .service("env-prod", Kind::Application, "app-web", "web")
        .port("port-web", 8080, 80);
    world
        .service("env-prod", Kind::Application, "app-api", "api")
        .port("port-api", 8080, 80);
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path()).await.unwrap();

    let config = DokployConfig::parse(&config_text(workspace.path())).unwrap();
    assert!(
        config
            .resource(&address("port.imported-8080-tcp"))
            .is_none()
    );
    assert!(
        config
            .resource(&address("port.web-imported-8080-tcp"))
            .is_some()
    );
    assert!(
        config
            .resource(&address("port.api-imported-8080-tcp"))
            .is_some()
    );
    assert_plans_clean(&router, workspace.path()).await;
}

#[tokio::test]
async fn a_leaf_name_that_still_collides_is_numbered_by_remote_id() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    // Different kinds may share a name, so these two parents are both `web`.
    world
        .service("env-prod", Kind::Compose, "cmp-web", "web")
        .volume_mount("mnt-b", "/data", "volume-b");
    world
        .service("env-prod", Kind::Application, "app-web", "web")
        .volume_mount("mnt-a", "/data", "volume-a");
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    let report = import(&router, workspace.path()).await.unwrap();

    let state = read_state(&router, workspace.path());
    assert_eq!(
        state
            .resource(&address("mount.web-data"))
            .unwrap()
            .remote_id()
            .as_str(),
        "mnt-a",
        "the lowest remote id keeps the unnumbered name"
    );
    assert_eq!(
        state
            .resource(&address("mount.web-data-2"))
            .unwrap()
            .remote_id()
            .as_str(),
        "mnt-b"
    );
    let text = report.to_string();
    assert!(text.contains("mount.web-data-2 (remote: /data)"), "{text}");
    assert_plans_clean(&router, workspace.path()).await;
}

#[tokio::test]
async fn compose_schedules_on_one_service_record_the_service_and_target() {
    let router = platform().router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path()).await.unwrap();

    let state = read_state(&router, workspace.path());
    for name in ["schedule.web-task", "schedule.web-late"] {
        let inputs = state
            .resource(&address(name))
            .unwrap()
            .last_applied()
            .as_json()
            .clone();
        assert_eq!(inputs["service_name"], "web", "{name}");
        assert_eq!(inputs["target"], "compose.stack", "{name}");
    }
}

#[tokio::test]
async fn compose_schedules_on_several_services_fail_closed_without_writing() {
    let mut world = platform();
    world
        .svc("cmp-stack")
        .schedule("sch-worker", "worker-task", "0 3 * * *", Some("worker"));
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("must not import");

    assert!(
        matches!(error, ImportError::ComposeSchedulesSpanServices),
        "{error}"
    );
    assert_nothing_written(workspace.path());
    assert_read_only(&router);
}

#[tokio::test]
async fn same_named_siblings_of_one_kind_fail_closed_without_writing() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world.service("env-prod", Kind::Application, "app-a", "api");
    world.service("env-prod", Kind::Application, "app-b", "api");
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("must not import");

    assert!(
        matches!(
            error,
            ImportError::DuplicateName {
                kind: "application"
            }
        ),
        "{error}"
    );
    assert_nothing_written(workspace.path());
}

#[tokio::test]
async fn a_project_name_shared_with_another_project_fails_closed() {
    let world = platform();
    let other = serde_json::json!({
        "projectId": "project-2", "name": "Platform", "environments": [],
    });
    let router = world.router_with(vec![(
        "GET /api/project.all".to_owned(),
        vec![ok(
            serde_json::json!([world.project_record(), other]).to_string()
        )],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("must not import");

    assert!(
        matches!(error, ImportError::DuplicateName { kind: "project" }),
        "{error}"
    );
    assert_nothing_written(workspace.path());
}

// ---------------------------------------------------------------------------
// External servers, registries, and destinations
// ---------------------------------------------------------------------------

fn with_externals() -> World {
    let mut world = platform();
    world
        .external_server("srv-edge", "edge-1")
        .external_server("srv-build", "builder")
        .registry("reg-hub", "hub")
        .registry("reg-cache", "cache")
        .registry("reg-roll", "rollback");
    world
        .svc("app-api")
        .server("srv-edge")
        .build_server("srv-build")
        .registries("reg-hub", "reg-cache", "reg-roll");
    world
}

#[tokio::test]
async fn external_associations_become_name_selectors_without_identities() {
    let router = with_externals().router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path())
        .await
        .expect("project imports");

    let source = config_text(workspace.path());
    let state = std::fs::read_to_string(workspace.path().join(".dokploy/state.json")).unwrap();
    for name in ["edge-1", "builder", "hub", "cache", "rollback", "offsite"] {
        assert!(source.contains(name), "{name} must be a selector\n{source}");
    }
    for identity in [
        "srv-edge",
        "srv-build",
        "reg-hub",
        "reg-cache",
        "reg-roll",
        "dest-1",
    ] {
        assert!(
            !source.contains(identity),
            "{identity} leaked into configuration"
        );
        assert!(!state.contains(identity), "{identity} leaked into state");
    }
    assert_secret_free(workspace.path());
    assert_plans_clean(&router, workspace.path()).await;
}

#[tokio::test]
async fn external_collections_are_read_once_per_crawl() {
    let router = with_externals().router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path()).await.unwrap();

    for collection in ["server.all", "registry.all", "destination.all"] {
        assert_eq!(
            router.matching(&format!("GET /api/{collection}")).len(),
            1,
            "{collection} is loaded once for the whole crawl"
        );
    }
}

#[tokio::test]
async fn a_project_without_associations_reads_no_external_collections() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world.service("env-prod", Kind::Application, "app-api", "api");
    world.service("env-prod", Kind::Postgres, "pg-main", "main");
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path()).await.unwrap();

    for collection in ["server.all", "registry.all", "destination.all"] {
        assert!(
            router
                .matching(&format!("GET /api/{collection}"))
                .is_empty(),
            "{collection} must not be read when nothing is attached"
        );
    }
}

#[tokio::test]
async fn every_service_kind_imports_its_server_placement() {
    for (kind, id) in [
        (Kind::Compose, "svc-compose"),
        (Kind::Postgres, "svc-postgres"),
        (Kind::MySql, "svc-mysql"),
        (Kind::MariaDb, "svc-mariadb"),
        (Kind::Mongo, "svc-mongo"),
        (Kind::LibSql, "svc-libsql"),
        (Kind::Redis, "svc-redis"),
    ] {
        let mut world = World::new("project-1", "Platform");
        world.environment("env-prod", "production");
        world.external_server("srv-edge", "edge-1");
        world
            .service("env-prod", kind, id, "main")
            .server("srv-edge");
        let router = world.router();
        let workspace = tempfile::tempdir().unwrap();

        import(&router, workspace.path())
            .await
            .unwrap_or_else(|error| {
                panic!("{} placement must import: {error}", kind.key());
            });

        let state = read_state(&router, workspace.path());
        let inputs = state
            .resource(&address(&format!("{}.main", kind.key())))
            .unwrap()
            .last_applied()
            .as_json()
            .clone();
        assert_eq!(
            inputs["server"],
            serde_json::json!({"name": "edge-1"}),
            "{}",
            kind.key()
        );
        assert!(!config_text(workspace.path()).contains("srv-edge"));
        assert_plans_clean(&router, workspace.path()).await;
    }
}

#[tokio::test]
async fn unknown_shared_or_unreadable_externals_fail_closed_without_writing() {
    // An application attached to a server that the instance does not list.
    let mut unknown = with_externals();
    unknown.svc("app-api").server("srv-ghost");

    // Two servers share the attached server's name.
    let mut shared = with_externals();
    shared.external_server("srv-twin", "edge-1");

    // The server collection cannot be read at all.
    let unreadable = (
        with_externals(),
        vec![(
            "GET /api/server.all".to_owned(),
            vec![status("500 Internal Server Error", r#"{"message":"boom"}"#)],
        )],
    );

    // A Backup whose destination name is shared.
    let mut destination = with_externals();
    destination.destination("dest-twin", "offsite");

    for (name, world, overrides) in [
        ("unknown", unknown, Vec::new()),
        ("shared", shared, Vec::new()),
        ("unreadable", unreadable.0, unreadable.1),
        ("destination", destination, Vec::new()),
    ] {
        let router = world.router_with(overrides);
        let workspace = tempfile::tempdir().unwrap();

        let error = import(&router, workspace.path())
            .await
            .expect_err("an unresolvable association must not import");

        assert!(
            matches!(error, ImportError::ExternalAssociation),
            "{name}: {error}"
        );
        assert_nothing_written(workspace.path());
        assert_read_only(&router);
    }
}

// ---------------------------------------------------------------------------
// Failure at every stage leaves no configuration and no state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failed_read_during_the_crawl_leaves_nothing_behind() {
    let world = platform();
    let router = world.router_with(vec![(
        World::key("application.one", "applicationId", "app-api"),
        vec![status("500 Internal Server Error", r#"{"message":"boom"}"#)],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("the crawl fails");

    assert!(matches!(error, ImportError::Remote(_)), "{error}");
    assert_nothing_written(workspace.path());
    assert_read_only(&router);
}

#[tokio::test]
async fn an_inconsistent_topology_leaves_nothing_behind() {
    // The project-scoped collection lists an environment the project does not.
    let world = platform();
    let router = world.router_with(vec![(
        World::key("environment.byProjectId", "projectId", "project-1"),
        vec![ok(serde_json::json!([
            {"environmentId": "env-prod", "name": "production"},
            {"environmentId": "env-ghost", "name": "ghost"},
        ])
        .to_string())],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("validation fails");

    assert!(
        matches!(error, ImportError::InvalidRemoteTopology),
        "{error}"
    );
    assert_nothing_written(workspace.path());
}

#[tokio::test]
async fn a_compose_collection_that_disagrees_with_the_direct_read_fails_closed() {
    let world = platform();
    let router = world.router_with(vec![(
        World::key("compose.search", "environmentId", "env-prod"),
        vec![ok(serde_json::json!({
            "items": [{
                "composeId": "cmp-stack", "environmentId": "env-prod", "name": "Renamed",
                "appName": "stack", "description": "Stack", "sourceType": "raw",
            }],
            "total": 1,
        })
        .to_string())],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("agreement is required");

    assert!(
        matches!(error, ImportError::InvalidRemoteTopology),
        "{error}"
    );
    assert_nothing_written(workspace.path());
}

#[tokio::test]
async fn a_backup_with_an_unknown_enabled_flag_is_never_guessed() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world.destination("dest-1", "offsite");
    world
        .service("env-prod", Kind::Postgres, "pg-main", "main")
        .backup_enabled("bk-pg", "dest-1", "nightly", None);
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("the flag is required");

    assert!(
        matches!(error, ImportError::InvalidRemoteTopology),
        "{error}"
    );
    assert_nothing_written(workspace.path());
}

#[tokio::test]
async fn a_malformed_mount_record_fails_the_whole_import() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world
        .service("env-prod", Kind::Application, "app-api", "api")
        // A volume Mount that names no volume cannot be written back.
        .raw_mount(serde_json::json!({
            "mountId": "mnt-bad", "type": "volume", "mountPath": "/data",
            "serviceType": "application", "applicationId": "app-api",
            "volumeName": null, "hostPath": null, "filePath": null,
        }));
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("the Mount is invalid");

    assert!(
        matches!(error, ImportError::InvalidRemoteTopology),
        "{error}"
    );
    assert_nothing_written(workspace.path());
}

#[tokio::test]
async fn a_project_that_changes_during_the_crawl_writes_nothing() {
    let platform = platform();
    let mut grown = platform.project_record();
    grown["environments"][0]["applications"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"applicationId": "app-new", "name": "new"}));
    // The first read is what the crawl sees; every later read shows a new application.
    let router = platform.router_with(vec![(
        World::key("project.one", "projectId", "project-1"),
        vec![
            ok(platform.project_record().to_string()),
            ok(grown.to_string()),
        ],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("the race is detected");

    assert!(matches!(error, ImportError::RemoteChanged), "{error}");
    assert_nothing_written(workspace.path());
    assert_read_only(&router);
}

#[tokio::test]
async fn a_database_added_during_the_crawl_writes_nothing() {
    let platform = platform();
    let router = platform.router_with(vec![(
        World::key("mysql.search", "environmentId", "env-prod"),
        vec![
            ok(serde_json::json!({
                "items": [{"mysqlId": "my-legacy", "environmentId": "env-prod", "name": "Legacy"}],
                "total": 1,
            })
            .to_string()),
            ok(serde_json::json!({
                "items": [
                    {"mysqlId": "my-legacy", "environmentId": "env-prod", "name": "Legacy"},
                    {"mysqlId": "my-late", "environmentId": "env-prod", "name": "Late"},
                ],
                "total": 2,
            })
            .to_string()),
        ],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("the race is detected");

    assert!(matches!(error, ImportError::RemoteChanged), "{error}");
    assert_nothing_written(workspace.path());
}

#[tokio::test]
async fn a_project_larger_than_the_environment_bound_is_refused_before_reading_more() {
    let environments = (0..1_001)
        .map(|index| {
            serde_json::json!({
                "environmentId": format!("env-{index}"), "name": format!("env {index}"),
                "isDefault": index == 0,
            })
        })
        .collect::<Vec<_>>();
    let router = Router::start(vec![(
        "GET /api/project.one",
        vec![ok(serde_json::json!({
            "projectId": "project-1", "name": "Huge", "environments": environments,
        })
        .to_string())],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("the bound applies");

    assert!(matches!(error, ImportError::TooLarge), "{error}");
    assert_eq!(router.requests().len(), 1, "no further resource was read");
    assert_nothing_written(workspace.path());
}

// ---------------------------------------------------------------------------
// New workspace only
// ---------------------------------------------------------------------------

#[tokio::test]
async fn import_only_creates_a_new_workspace() {
    let router = platform().router();
    let workspace = tempfile::tempdir().unwrap();

    std::fs::write(workspace.path().join("dokploy.yaml"), "existing").unwrap();
    let error = import(&router, workspace.path())
        .await
        .expect_err("config exists");
    assert!(matches!(error, ImportError::ConfigExists), "{error}");
    assert!(
        router.requests().is_empty(),
        "nothing is read before the refusal"
    );
    std::fs::remove_file(workspace.path().join("dokploy.yaml")).unwrap();

    import(&router, workspace.path())
        .await
        .expect("first import");
    let second = ImportRequest {
        project_id: "project-1".to_owned(),
        config_file: workspace.path().join("other.yaml"),
    };
    let error = import_project(&router.client(), second)
        .await
        .expect_err("state exists");
    assert!(matches!(error, ImportError::StateExists), "{error}");
    assert!(!workspace.path().join("other.yaml").exists());
}

#[tokio::test]
async fn the_same_remote_tree_always_produces_the_same_workspace() {
    // The same project described with its services added in the opposite order.
    let forward = platform();
    let mut reverse = World::new("project-1", "Platform");
    reverse.environment("env-prod", "production");
    reverse.destination("dest-1", "offsite");
    for (kind, id, name) in [
        (Kind::Redis, "redis-cache", "Cache"),
        (Kind::LibSql, "lib-edge", "Edge"),
        (Kind::Mongo, "mongo-docs", "Docs"),
        (Kind::MariaDb, "maria-one", "Maria"),
        (Kind::MySql, "my-legacy", "Legacy"),
    ] {
        reverse.service("env-prod", kind, id, name);
    }
    reverse
        .service("env-prod", Kind::Postgres, "pg-main", "Main DB")
        .backup("bk-pg", "dest-1", "nightly")
        .volume_mount("mnt-pg", "/var/lib/postgresql", "pg-data");
    reverse
        .service("env-prod", Kind::Compose, "cmp-stack", "Stack")
        .schedule("sch-web-late", "web-late", "0 2 * * *", Some("web"))
        .schedule("sch-web", "web-task", "0 1 * * *", Some("web"))
        .file_mount("mnt-config", "/etc/app.conf", "app.conf");
    reverse
        .service("env-prod", Kind::Application, "app-api", "API")
        .schedule("sch-clean", "cleanup", "0 * * * *", None)
        .volume_mount("mnt-data", "/data", "api-data")
        .security("sec-admin", "admin")
        .redirect("red-old", "^/old", "/new")
        .port("port-http", 8080, 80)
        .domain("dom-api", "api.example.test");

    let mut outputs = Vec::new();
    for world in [forward, reverse] {
        let router = world.router();
        let workspace = tempfile::tempdir().unwrap();
        import(&router, workspace.path()).await.unwrap();
        let state = serde_json::to_value(read_state(&router, workspace.path())).unwrap();
        outputs.push((config_text(workspace.path()), state["resources"].clone()));
    }

    assert_eq!(
        outputs[0].0, outputs[1].0,
        "configuration differs by listing order"
    );
    assert_eq!(outputs[0].1, outputs[1].1, "state differs by listing order");
}

#[tokio::test]
async fn a_sparse_libsql_topology_entry_is_enriched_by_a_direct_read() {
    let world = platform();
    let mut sparse = world.project_record();
    sparse["environments"][0]["libsql"] = serde_json::json!([{"libsqlId": "lib-edge"}]);
    let router = world.router_with(vec![(
        World::key("project.one", "projectId", "project-1"),
        vec![ok(sparse.to_string())],
    )]);
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path())
        .await
        .expect("the direct read supplies the name");

    let config = DokployConfig::parse(&config_text(workspace.path())).unwrap();
    assert!(config.resource(&address("libsql.edge")).is_some());
    assert!(
        !router
            .matching("GET /api/libsql.one?libsqlId=lib-edge")
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Interactive selection and the public CLI
// ---------------------------------------------------------------------------

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

struct FakePrompter {
    selected: usize,
    choices: Vec<String>,
}

impl ImportPrompter for FakePrompter {
    fn select(&mut self, choices: &[String]) -> Result<usize, ImportError> {
        self.choices = choices.to_vec();
        Ok(self.selected)
    }
}

fn projects(rows: &[(&str, &str)]) -> Router {
    Router::start(vec![(
        "GET /api/project.all",
        vec![ok(serde_json::json!(
            rows.iter()
                .map(|(id, name)| serde_json::json!({
                    "projectId": id, "name": name, "environments": [],
                }))
                .collect::<Vec<_>>()
        )
        .to_string())],
    )])
}

#[tokio::test]
async fn the_picker_lists_projects_by_name_and_distinguishes_duplicates_by_identity() {
    let router = projects(&[
        ("project-2", "Platform"),
        ("project-1", "Platform"),
        ("project-3", "Apps"),
    ]);
    let mut prompter = FakePrompter {
        selected: 2,
        choices: Vec::new(),
    };

    let selection = select_with_prompter(
        &router.client(),
        std::path::PathBuf::from("dokploy.yaml"),
        &mut prompter,
    )
    .await
    .expect("a project is selected");

    assert_eq!(
        prompter.choices,
        [
            "Apps (project-3)",
            "Platform (project-1)",
            "Platform (project-2)"
        ]
    );
    assert_eq!(selection.project_id, "project-2");
    assert_eq!(router.requests().len(), 1, "the picker makes one call");
}

#[tokio::test]
async fn the_picker_refuses_an_empty_instance_and_an_out_of_range_choice() {
    let empty = projects(&[]);
    let mut prompter = FakePrompter {
        selected: 0,
        choices: Vec::new(),
    };
    let error = select_with_prompter(&empty.client(), "dokploy.yaml".into(), &mut prompter)
        .await
        .err()
        .expect("nothing to choose");
    assert!(matches!(error, ImportError::NoVisibleResources), "{error}");

    let one = projects(&[("project-1", "Platform")]);
    let mut prompter = FakePrompter {
        selected: 5,
        choices: Vec::new(),
    };
    let error = select_with_prompter(&one.client(), "dokploy.yaml".into(), &mut prompter)
        .await
        .err()
        .expect("the choice is out of range");
    assert!(matches!(error, ImportError::InvalidSelection), "{error}");
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
async fn the_cli_imports_a_project_and_prints_the_table() {
    let mut world = platform();
    world.environment("env-stage", "staging");
    world.service("env-stage", Kind::Application, "app-stage", "API");
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");

    let (result, output) = run(
        &router,
        workspace.path(),
        &[
            "import",
            "project",
            "project-1",
            "--file",
            config_file.to_str().unwrap(),
        ],
    )
    .await;

    result.expect("the CLI import succeeds");
    assert!(config_file.exists());
    assert!(
        output.contains("Import complete: 23 resource(s) tracked."),
        "{output}"
    );
    assert!(output.contains("Project project.platform"), "{output}");
    assert!(output.contains("environment.production:"), "{output}");
    assert!(
        output.contains("environment.staging: 1 application"),
        "{output}"
    );
    assert!(
        output.contains("application.production-api (remote: API)"),
        "{output}"
    );
    assert!(output.contains("Left unmanaged"), "{output}");
    assert_read_only(&router);
}

#[tokio::test]
async fn interactive_import_without_a_terminal_fails_before_reading_anything() {
    let router = platform().router();
    let workspace = tempfile::tempdir().unwrap();

    let (result, _) = run(&router, workspace.path(), &["import", "project"]).await;

    let error = result.expect_err("a terminal is required");
    assert!(error.contains("requires a terminal"), "{error}");
    assert!(router.requests().is_empty());
}

// ---------------------------------------------------------------------------
// Per-kind semantics carried over from the single-resource importers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn optional_values_are_owned_only_when_the_remote_returns_them_and_still_converge() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world.destination("dest-1", "offsite");
    world
        .service("env-prod", Kind::Application, "app-api", "api")
        .schedule("sch-plain", "plain", "0 * * * *", None)
        .schedule("sch-rich", "rich", "5 * * * *", None)
        .schedule_details(Some("Hourly sweep"), Some("Europe/Paris"))
        .bind_mount("mnt-bind", "/host", "/srv/data");
    world
        .service("env-prod", Kind::MySql, "my-main", "sql")
        .backup("bk-sql", "dest-1", "weekly")
        .backup_keep(None);
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path())
        .await
        .expect("project imports");

    let state = read_state(&router, workspace.path());
    let plain = state
        .resource(&address("schedule.plain"))
        .unwrap()
        .last_applied()
        .as_json()
        .clone();
    assert!(
        plain.get("description").is_none() && plain.get("timezone").is_none(),
        "{plain}"
    );
    let rich = state
        .resource(&address("schedule.rich"))
        .unwrap()
        .last_applied()
        .as_json()
        .clone();
    assert_eq!(rich["description"], "Hourly sweep");
    assert_eq!(rich["timezone"], "Europe/Paris");
    let backup = state
        .resource(&address("backup.sql-weekly"))
        .unwrap()
        .last_applied()
        .as_json()
        .clone();
    assert!(
        backup["keep_latest"].is_null(),
        "cleared retention is owned as null: {backup}"
    );
    assert_eq!(
        state
            .resource(&address("mount.host"))
            .unwrap()
            .last_applied()
            .as_json()["host_path"],
        "/srv/data"
    );
    assert_plans_clean(&router, workspace.path()).await;
}

#[tokio::test]
async fn a_file_mount_never_carries_its_content() {
    let router = platform().router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path()).await.unwrap();

    let state = read_state(&router, workspace.path());
    let mount = state.resource(&address("mount.etc-app-conf")).unwrap();
    assert_eq!(mount.last_applied().as_json()["file_path"], "app.conf");
    assert!(mount.last_applied().as_json().get("content").is_none());
    assert_secret_free(workspace.path());
}

#[tokio::test]
async fn database_specifics_round_trip_and_converge() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    world
        .service("env-prod", Kind::LibSql, "lib-replica", "Replica")
        .patch("sqldNode", serde_json::json!("replica"))
        .patch(
            "sqldPrimaryUrl",
            serde_json::json!("http://primary.internal:8080"),
        );
    world
        .service("env-prod", Kind::Mongo, "mongo-docs", "Docs")
        .patch("replicaSets", serde_json::json!(true));
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path())
        .await
        .expect("project imports");

    let state = read_state(&router, workspace.path());
    assert_eq!(
        state
            .resource(&address("libsql.replica"))
            .unwrap()
            .last_applied()
            .as_json()["node"],
        serde_json::json!({"type": "replica", "primary_url": "http://primary.internal:8080"})
    );
    assert_eq!(
        state
            .resource(&address("mongo.docs"))
            .unwrap()
            .last_applied()
            .as_json()["replica_sets"],
        true
    );
    assert_plans_clean(&router, workspace.path()).await;
}

#[tokio::test]
async fn an_external_name_outside_the_selector_grammar_fails_closed() {
    let mut world = World::new("project-1", "Platform");
    world.environment("env-prod", "production");
    // Names with surrounding whitespace or control characters cannot be selectors.
    world.external_server("srv-odd", " padded ");
    world
        .service("env-prod", Kind::Application, "app-api", "api")
        .server("srv-odd");
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    let error = import(&router, workspace.path())
        .await
        .expect_err("the name is unusable");

    assert!(matches!(error, ImportError::ExternalAssociation), "{error}");
    assert_nothing_written(workspace.path());
}

#[tokio::test]
async fn project_tags_import_as_sorted_name_selectors_and_plan_clean() {
    let mut world = platform();
    world
        .tag("tag-b", "prod", Some("#e11d48"), true)
        .tag("tag-a", "critical", None, true)
        .tag("tag-c", "unused", None, false);
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path())
        .await
        .expect("a tagged project imports");

    assert_read_only(&router);
    let source = config_text(workspace.path());
    assert!(
        source.contains("tags:") && source.contains("critical") && source.contains("prod"),
        "{source}"
    );
    assert!(!source.contains("unused"), "{source}");
    let state = read_state(&router, workspace.path());
    let project = state.resource(&address("project.platform")).unwrap();
    assert_eq!(
        project.last_applied().as_json()["tags"],
        serde_json::json!(["critical", "prod"])
    );
    assert_plans_clean(&router, workspace.path()).await;
}

#[tokio::test]
async fn an_untagged_project_never_reads_the_tag_collection() {
    let mut world = platform();
    world.tag("tag-c", "unused", None, false);
    let router = world.router();
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path())
        .await
        .expect("an untagged project imports");

    assert!(router.matching("GET /api/tag.all").is_empty());
    assert!(!config_text(workspace.path()).contains("tags:"));
}

#[tokio::test]
async fn project_tags_with_a_shared_or_unlisted_identity_fail_closed() {
    let mut shared = platform();
    shared
        .tag("tag-1", "prod", None, true)
        .tag("tag-2", "prod", None, false);
    let mut unlisted = platform();
    unlisted.tag("tag-1", "prod", None, true);

    for (world, hidden) in [(shared, None), (unlisted, Some("tag-1"))] {
        let overrides = hidden
            .map(|_| vec![("GET /api/tag.all".to_owned(), vec![ok("[]")])])
            .unwrap_or_default();
        let router = world.router_with(overrides);
        let workspace = tempfile::tempdir().unwrap();

        import(&router, workspace.path())
            .await
            .expect_err("an ambiguous tag identity is never guessed");

        assert_nothing_written(workspace.path());
    }
}
