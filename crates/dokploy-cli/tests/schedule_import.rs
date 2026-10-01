mod support;

use dokploy_cli::cli::ImportKind;
use dokploy_cli::import::{ImportRequest, import_resource};
use dokploy_cli::planning::plan_workspace;
use dokploy_config::{DokployConfig, Field};
use dokploy_state::{InstanceIdentity, ResourceAddress, StateStore};
use support::{Reply, Router, ScheduleTargetFixture, list, ok, schedule_record, status};

const COMMAND_CANARY: &str = "import-command-canary-never-leak";
const SCRIPT_CANARY: &str = "import-script-canary-never-leak";

const APPLICATION: &str = r#"{"applicationId":"application-1","name":"API Service","appName":"api","environmentId":"environment-1","description":"API","replicas":1}"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"Production West","projectId":"project-1"}"#;
const PROJECT: &str = r#"{"projectId":"project-1","name":"IaC Contract Test","environments":[]}"#;
const TOPOLOGY: &str = r#"[{"projectId":"project-1","name":"IaC Contract Test","environments":[{"environmentId":"environment-1","name":"Production West","isDefault":true,"applications":[{"applicationId":"application-1","name":"API Service"}],"postgres":[],"redis":[],"compose":[{"composeId":"compose-1","name":"Stack"}]}]}]"#;
const COMPOSE: &str = r#"{"composeId":"compose-1","name":"Stack","appName":"stack","environmentId":"environment-1","sourceType":"raw","composeType":"docker-compose","autoDeploy":false,"composePath":"./docker-compose.yml","composeStatus":"idle"}"#;

fn address(value: &str) -> ResourceAddress {
    value.parse().unwrap()
}

fn base_routes(
    schedule_routes: Vec<(&'static str, Vec<Reply>)>,
) -> Vec<(&'static str, Vec<Reply>)> {
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
        ("GET /api/compose.one", vec![ok(COMPOSE)]),
        (
            "GET /api/compose.search",
            vec![ok(
                r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"Stack","appName":"stack","sourceType":"raw"}],"total":1}"#,
            )],
        ),
    ];
    routes.extend(schedule_routes);
    routes
}

async fn import(
    router: &Router,
    workspace: &std::path::Path,
) -> Result<usize, dokploy_cli::import::ImportError> {
    import_resource(
        &router.client(),
        ImportRequest {
            kind: ImportKind::Schedule,
            remote_id: "schedule-1".to_owned(),
            address: address("schedule.nightly"),
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

fn application_record(description: Option<&str>, timezone: Option<&str>) -> String {
    schedule_record(
        "schedule-1",
        ScheduleTargetFixture::Application("application-1"),
        "nightly",
        "0 3 * * *",
        "bash",
        false,
        COMMAND_CANARY,
        Some(SCRIPT_CANARY),
        description,
        timezone,
    )
}

fn assert_read_only(router: &Router) {
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
    assert!(
        router
            .requests()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

#[tokio::test]
async fn application_schedule_import_is_protected_executables_stay_unmanaged_and_it_converges() {
    let record = application_record(None, None);
    let router = Router::start(base_routes(vec![
        ("GET /api/schedule.one", vec![ok(&record)]),
        ("GET /api/schedule.list", vec![ok(list([&record]))]),
    ]));
    let workspace = tempfile::tempdir().unwrap();

    let count = import(&router, workspace.path()).await.unwrap();
    let plan = plan_workspace(&router.client(), &workspace.path().join("dokploy.yaml"))
        .await
        .unwrap();

    assert_eq!(
        count, 4,
        "project, environment, target application, and Schedule"
    );
    assert!(plan.complete() && plan.applyable() && plan.changes().is_empty());
    let source = std::fs::read_to_string(workspace.path().join("dokploy.yaml")).unwrap();
    for canary in [COMMAND_CANARY, SCRIPT_CANARY] {
        assert!(!source.contains(canary));
    }
    assert!(!source.contains("command:"));
    assert!(!source.contains("script:"));
    let config = DokployConfig::parse(&source).unwrap();
    let resource = config.resource(&address("schedule.nightly")).unwrap();
    assert_eq!(resource.lifecycle().protect(), &Field::Set(true));
    let schedule = resource.as_schedule().unwrap();
    assert_eq!(schedule.target(), &address("application.api-service"));
    assert_eq!(schedule.service_name(), None);
    assert_eq!(schedule.name(), "nightly");
    assert!(!schedule.enabled());
    assert!(matches!(schedule.command(), Field::Unmanaged));
    assert!(matches!(schedule.script(), Field::Unmanaged));
    assert_eq!(
        config.parent_of(&address("schedule.nightly")),
        Some(&address("environment.production-west"))
    );
    let state = read_state(&router, workspace.path());
    let imported = state.resource(&address("schedule.nightly")).unwrap();
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
            "name": "nightly",
            "cron_expression": "0 3 * * *",
            "shell_type": "bash",
            "enabled": false
        })
    );
    assert_eq!(imported.sensitive_inputs().paths().count(), 0);
    let state_text = std::fs::read_to_string(workspace.path().join(".dokploy/state.json")).unwrap();
    for canary in [COMMAND_CANARY, SCRIPT_CANARY] {
        assert!(!state_text.contains(canary));
    }
    assert_read_only(&router);
}

#[tokio::test]
async fn description_and_timezone_are_owned_only_when_present_and_still_converge() {
    let record = application_record(Some("Nightly cleanup"), Some("Africa/Lubumbashi"));
    let router = Router::start(base_routes(vec![
        ("GET /api/schedule.one", vec![ok(&record)]),
        ("GET /api/schedule.list", vec![ok(list([&record]))]),
    ]));
    let workspace = tempfile::tempdir().unwrap();

    import(&router, workspace.path()).await.unwrap();
    let plan = plan_workspace(&router.client(), &workspace.path().join("dokploy.yaml"))
        .await
        .unwrap();

    assert!(plan.applyable() && plan.changes().is_empty());
    let config = DokployConfig::parse(
        &std::fs::read_to_string(workspace.path().join("dokploy.yaml")).unwrap(),
    )
    .unwrap();
    let schedule = config
        .resource(&address("schedule.nightly"))
        .unwrap()
        .as_schedule()
        .unwrap();
    assert_eq!(
        schedule.description(),
        &Field::Set("Nightly cleanup".into())
    );
    assert_eq!(schedule.timezone(), &Field::Set("Africa/Lubumbashi".into()));
    let state = read_state(&router, workspace.path());
    let inputs = state
        .resource(&address("schedule.nightly"))
        .unwrap()
        .last_applied()
        .as_json()
        .clone();
    assert_eq!(inputs["description"], "Nightly cleanup");
    assert_eq!(inputs["timezone"], "Africa/Lubumbashi");
}

#[tokio::test]
async fn compose_service_schedule_import_records_the_service_and_target_dependency() {
    let record = schedule_record(
        "schedule-1",
        ScheduleTargetFixture::Compose("compose-1", "worker"),
        "nightly",
        "@daily",
        "sh",
        true,
        COMMAND_CANARY,
        None,
        None,
        None,
    );
    let router = Router::start(base_routes(vec![
        ("GET /api/schedule.one", vec![ok(&record)]),
        ("GET /api/schedule.list", vec![ok(list([&record]))]),
    ]));
    let workspace = tempfile::tempdir().unwrap();

    let count = import(&router, workspace.path()).await.unwrap();

    assert_eq!(count, 4);
    let state = read_state(&router, workspace.path());
    let imported = state.resource(&address("schedule.nightly")).unwrap();
    assert_eq!(imported.dependencies(), &[address("compose.stack")]);
    assert_eq!(imported.last_applied().as_json()["service_name"], "worker");
    assert_eq!(imported.last_applied().as_json()["enabled"], true);
    assert!(router.matching("GET /api/schedule.list")[0].contains("scheduleType=compose"));
    let source = std::fs::read_to_string(workspace.path().join("dokploy.yaml")).unwrap();
    assert!(source.contains("service_name: \"worker\""));
    assert!(!source.contains(COMMAND_CANARY));
    assert_read_only(&router);
}

#[tokio::test]
async fn import_fails_closed_on_identity_collection_or_shape_contradictions() {
    let record = application_record(None, None);

    // The direct read returns a different identity.
    let other = schedule_record(
        "schedule-2",
        ScheduleTargetFixture::Application("application-1"),
        "nightly",
        "0 3 * * *",
        "bash",
        false,
        COMMAND_CANARY,
        None,
        None,
        None,
    );
    let router = Router::start(base_routes(vec![
        ("GET /api/schedule.one", vec![ok(&other)]),
        ("GET /api/schedule.list", vec![ok(list([&other]))]),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&router, workspace.path()).await.is_err());
    assert!(!workspace.path().join("dokploy.yaml").exists());

    // The authoritative collection does not contain the direct record.
    let router = Router::start(base_routes(vec![
        ("GET /api/schedule.one", vec![ok(&record)]),
        ("GET /api/schedule.list", vec![ok("[]")]),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&router, workspace.path()).await.is_err());
    assert!(!workspace.path().join("dokploy.yaml").exists());

    // A host-scoped Schedule is not a supported target.
    let privileged = r#"{"scheduleId":"schedule-1","name":"host-job","description":null,"cronExpression":"0 3 * * *","shellType":"bash","scheduleType":"server","command":"x","script":null,"applicationId":null,"composeId":null,"serverId":"server-1","serviceName":null,"enabled":false,"timezone":null}"#;
    let router = Router::start(base_routes(vec![
        ("GET /api/schedule.one", vec![ok(privileged)]),
        (
            "GET /api/schedule.list",
            vec![ok(list([privileged.to_owned()]))],
        ),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&router, workspace.path()).await.is_err());
    assert!(!workspace.path().join("dokploy.yaml").exists());

    // A name outside the configuration grammar cannot be written safely.
    let unwritable = schedule_record(
        "schedule-1",
        ScheduleTargetFixture::Application("application-1"),
        "bad/name",
        "0 3 * * *",
        "bash",
        false,
        COMMAND_CANARY,
        None,
        None,
        None,
    );
    let router = Router::start(base_routes(vec![
        ("GET /api/schedule.one", vec![ok(&unwritable)]),
        ("GET /api/schedule.list", vec![ok(list([&unwritable]))]),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&router, workspace.path()).await.is_err());
    assert!(!workspace.path().join("dokploy.yaml").exists());

    // A missing Schedule surfaces as an error, not an empty import.
    let router = Router::start(base_routes(vec![(
        "GET /api/schedule.one",
        vec![status(
            "404 Not Found",
            r#"{"message":"Schedule not found"}"#,
        )],
    )]));
    let workspace = tempfile::tempdir().unwrap();
    let error = import(&router, workspace.path()).await.unwrap_err();
    assert!(!format!("{error:?}{error}").contains(COMMAND_CANARY));
    assert!(!workspace.path().join(".dokploy/state.json").exists());
}

#[test]
fn cli_accepts_the_schedule_import_kind() {
    use clap::Parser;
    let parsed = dokploy_cli::cli::Cli::try_parse_from([
        "dokploy",
        "import",
        "schedule",
        "schedule-1",
        "--as",
        "schedule.nightly",
    ])
    .expect("schedule import parses");
    assert!(matches!(
        parsed.command,
        dokploy_cli::cli::Command::Import {
            kind: Some(ImportKind::Schedule),
            ..
        }
    ));
}
