mod support;

use std::fs;

use dokploy_cli::recovery::{
    RecoverWorkspaceError, RecoveryAction, recover_workspace_with_approval,
};
use dokploy_state::{
    ExpectedCheckpoint, ExpectedState, FingerprintKeyId, InstanceIdentity, JournalAction,
    ManagedInputs, OperationJournal, PlanDigest, RecoveryStatus, RemoteId, ResourceAddress,
    ResourceKind, ResourceState, SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath,
    StateFile, StateStore,
};
use semver::Version;
use support::{
    ENVIRONMENT, ENVIRONMENTS, Router, ScheduleTargetFixture, application_one, application_search,
    list, ok, project_topology, schedule_record, status,
};

const COMMAND_CANARY: &str = "recovery-schedule-command-canary-never-leak";
const SCRIPT_CANARY: &str = "recovery-schedule-script-canary-never-leak";

fn address(value: &str) -> ResourceAddress {
    value.parse().unwrap()
}

fn routes(
    schedule_routes: Vec<(&'static str, Vec<support::Reply>)>,
) -> Vec<(&'static str, Vec<support::Reply>)> {
    let mut routes = vec![
        (
            "GET /api/project.all",
            vec![ok(project_topology(&support::application_item(
                "application-1",
                "api",
            )))],
        ),
        ("GET /api/environment.byProjectId", vec![ok(ENVIRONMENTS)]),
        ("GET /api/environment.one", vec![ok(ENVIRONMENT)]),
        (
            "GET /api/application.search",
            vec![ok(application_search(&[("application-1", "api")]))],
        ),
        (
            "GET /api/application.one",
            vec![ok(application_one("application-1", "api"))],
        ),
    ];
    routes.extend(schedule_routes);
    routes
}

fn receipt(command: u8, script: Option<u8>) -> SensitiveInputs {
    let key_id = FingerprintKeyId::new(
        uuid::Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").unwrap(),
    )
    .unwrap();
    let mut entries = vec![(
        SensitivePropertyPath::parse("command").unwrap(),
        SensitiveFingerprint::new_v1(key_id.clone(), [command; 32]),
    )];
    if let Some(script) = script {
        entries.push((
            SensitivePropertyPath::parse("script").unwrap(),
            SensitiveFingerprint::new_v1(key_id, [script; 32]),
        ));
    }
    SensitiveInputs::try_from_entries(entries).unwrap()
}

fn schedule_state(
    id: &str,
    inputs: serde_json::Value,
    sensitive: SensitiveInputs,
) -> ResourceState {
    ResourceState::try_new(
        ResourceKind::Schedule,
        RemoteId::new(id).unwrap(),
        false,
        ManagedInputs::try_from_json(inputs).unwrap(),
        sensitive,
        Some(address("environment.production")),
        vec![address("application.api")],
    )
    .unwrap()
}

/// Seeds project, environment, and application state, optionally with a Schedule.
fn seed(store: &StateStore, instance: InstanceIdentity, schedule: Option<ResourceState>) {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::absent(), &state)
        .unwrap();
    let mut entries = vec![
        ("project.platform", ResourceKind::Project, "project-1", None),
        (
            "environment.production",
            ResourceKind::Environment,
            "environment-1",
            Some("project.platform"),
        ),
        (
            "application.api",
            ResourceKind::Application,
            "application-1",
            Some("environment.production"),
        ),
    ]
    .into_iter()
    .map(|(name, kind, id, containment)| {
        (
            address(name),
            ResourceState::new(
                kind,
                RemoteId::new(id).unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                containment.map(address),
                Vec::new(),
            ),
        )
    })
    .collect::<Vec<_>>();
    if let Some(schedule) = schedule {
        entries.push((address("schedule.nightly"), schedule));
    }
    for (address, resource) in entries {
        let before = state.clone();
        state.upsert_resource(address, resource).unwrap();
        store
            .begin_write()
            .unwrap()
            .checkpoint(ExpectedState::from_state(&before), &state)
            .unwrap();
    }
}

fn open_step(store: &StateStore, action: JournalAction, expected: ExpectedCheckpoint) {
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("e".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(address("schedule.nightly"), action, expected)
        .unwrap();
}

fn job_config(cron: &str, script: bool) -> String {
    let script = if script {
        "        script: { file: script.sh }\n"
    } else {
        ""
    };
    let script = support::nest_yaml(script);
    format!(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api: {{}}\n      schedules:\n        nightly:\n          name: nightly\n          target: application.api\n          cron_expression: \"{cron}\"\n          shell_type: bash\n          enabled: false\n          command: {{ file: command.sh }}\n{script}"
    )
}

fn inputs(cron: &str) -> serde_json::Value {
    serde_json::json!({
        "target": "application.api",
        "name": "nightly",
        "cron_expression": cron,
        "shell_type": "bash",
        "enabled": false
    })
}

fn record(cron: &str, script: Option<&str>) -> String {
    schedule_record(
        "schedule-1",
        ScheduleTargetFixture::Application("application-1"),
        "nightly",
        cron,
        "bash",
        false,
        "remote-command-is-never-modeled",
        script,
        None,
        None,
    )
}

fn workspace(config: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(workspace.path().join("command.sh"), COMMAND_CANARY).unwrap();
    fs::write(workspace.path().join("script.sh"), SCRIPT_CANARY).unwrap();
    fs::write(&config_file, config).unwrap();
    (workspace, config_file)
}

fn pending_create(inputs: serde_json::Value, sensitive: SensitiveInputs) -> ExpectedCheckpoint {
    ExpectedCheckpoint::create(schedule_state("recovery-pending", inputs, sensitive)).unwrap()
}

fn no_mutation(router: &Router) {
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
    assert!(
        router
            .requests()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

fn assert_no_executable_text(workspace: &std::path::Path) {
    let state_text = fs::read_to_string(workspace.join(".dokploy/state.json")).unwrap();
    for canary in [
        COMMAND_CANARY,
        SCRIPT_CANARY,
        "remote-command-is-never-modeled",
    ] {
        assert!(!state_text.contains(canary));
    }
}

#[tokio::test]
async fn uncertain_create_adopts_one_exact_collision_key_without_retrying_or_exposing_text() {
    let remote = record("0 3 * * *", None);
    let router = Router::start(routes(vec![(
        "GET /api/schedule.list",
        vec![ok(list([remote]))],
    )]));
    let (workspace, config_file) = workspace(&job_config("0 3 * * *", false));
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    seed(&store, instance, None);
    open_step(
        &store,
        JournalAction::Create,
        pending_create(inputs("0 3 * * *"), receipt(1, None)),
    );

    let result = recover_workspace_with_approval(&router.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&address("schedule.nightly")));
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    let state = store.inspect().unwrap().unwrap();
    let adopted = state.resource(&address("schedule.nightly")).unwrap();
    assert_eq!(adopted.remote_id().as_str(), "schedule-1");
    assert_eq!(adopted.last_applied().as_json(), &inputs("0 3 * * *"));
    assert_eq!(adopted.dependencies(), &[address("application.api")]);
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    no_mutation(&router);
    assert_no_executable_text(workspace.path());
}

#[tokio::test]
async fn uncertain_create_with_authoritative_absence_confirms_no_change() {
    let router = Router::start(routes(vec![("GET /api/schedule.list", vec![ok("[]")])]));
    let (workspace, config_file) = workspace(&job_config("0 3 * * *", false));
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    seed(&store, instance, None);
    open_step(
        &store,
        JournalAction::Create,
        pending_create(inputs("0 3 * * *"), receipt(1, None)),
    );

    let result = recover_workspace_with_approval(&router.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::ConfirmNoChange);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    assert!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&address("schedule.nightly"))
            .is_none()
    );
    no_mutation(&router);
}

#[tokio::test]
async fn uncertain_create_with_a_different_schedule_or_script_at_the_key_needs_manual_intervention()
{
    for (remote, expected_script) in [
        // A different cron at the exact key.
        (record("9 9 * * 9", None), None),
        // The proposal carried a script, but the remote has none.
        (record("0 3 * * *", None), Some(2)),
    ] {
        let router = Router::start(routes(vec![(
            "GET /api/schedule.list",
            vec![ok(list([remote]))],
        )]));
        let (workspace, config_file) =
            workspace(&job_config("0 3 * * *", expected_script.is_some()));
        let instance = InstanceIdentity::parse(&router.url).unwrap();
        let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
        seed(&store, instance, None);
        open_step(
            &store,
            JournalAction::Create,
            pending_create(inputs("0 3 * * *"), receipt(1, expected_script)),
        );

        let error = recover_workspace_with_approval(&router.client(), &config_file, |_| Ok(true))
            .await
            .expect_err("a non-matching Schedule is never adopted");

        assert!(matches!(error, RecoverWorkspaceError::ManualIntervention));
        assert!(
            store
                .inspect()
                .unwrap()
                .unwrap()
                .resource(&address("schedule.nightly"))
                .is_none()
        );
        assert_ne!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    }
}

#[tokio::test]
async fn uncertain_update_is_confirmed_only_from_authoritative_complete_state() {
    let updated = record("5 4 * * 1", None);
    let router = Router::start(routes(vec![
        ("GET /api/schedule.list", vec![ok(list([&updated]))]),
        ("GET /api/schedule.one", vec![ok(&updated)]),
    ]));
    let (workspace, config_file) = workspace(&job_config("5 4 * * 1", false));
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    let before = schedule_state("schedule-1", inputs("0 3 * * *"), receipt(1, None));
    seed(&store, instance, Some(before.clone()));
    open_step(
        &store,
        JournalAction::Update,
        ExpectedCheckpoint::update(
            before,
            schedule_state("schedule-1", inputs("5 4 * * 1"), receipt(1, None)),
        )
        .unwrap(),
    );

    let result = recover_workspace_with_approval(&router.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::CheckpointConfirmedSuccess);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    assert_eq!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&address("schedule.nightly"))
            .unwrap()
            .last_applied()
            .as_json(),
        &inputs("5 4 * * 1")
    );
    no_mutation(&router);
}

#[tokio::test]
async fn uncertain_update_of_a_schedule_with_executable_receipts_is_never_assumed_unapplied() {
    let original = record("0 3 * * *", None);
    let router = Router::start(routes(vec![
        ("GET /api/schedule.list", vec![ok(list([&original]))]),
        ("GET /api/schedule.one", vec![ok(&original)]),
    ]));
    let (workspace, config_file) = workspace(&job_config("5 4 * * 1", false));
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    let before = schedule_state("schedule-1", inputs("0 3 * * *"), receipt(1, None));
    seed(&store, instance, Some(before.clone()));
    open_step(
        &store,
        JournalAction::Update,
        ExpectedCheckpoint::update(
            before,
            schedule_state("schedule-1", inputs("5 4 * * 1"), receipt(1, None)),
        )
        .unwrap(),
    );

    // Executable text cannot be read back, so a remote that still matches the old
    // safe fields does not prove that the step changed nothing.
    let error = recover_workspace_with_approval(&router.client(), &config_file, |_| Ok(true))
        .await
        .expect_err("an unapplied-looking update with write-only text needs an operator");

    assert!(matches!(error, RecoverWorkspaceError::ManualIntervention));
    assert_eq!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&address("schedule.nightly"))
            .unwrap()
            .last_applied()
            .as_json(),
        &inputs("0 3 * * *")
    );
    assert_ne!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
}

#[tokio::test]
async fn uncertain_command_or_script_rotation_requires_manual_intervention() {
    for (before_receipt, after_receipt, script) in [
        (receipt(1, None), receipt(2, None), false),
        (receipt(1, Some(5)), receipt(1, Some(6)), true),
    ] {
        let remote = record("0 3 * * *", script.then_some("remote-script"));
        let router = Router::start(routes(vec![
            ("GET /api/schedule.list", vec![ok(list([&remote]))]),
            ("GET /api/schedule.one", vec![ok(&remote)]),
        ]));
        let (workspace, config_file) = workspace(&job_config("0 3 * * *", script));
        let instance = InstanceIdentity::parse(&router.url).unwrap();
        let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
        let before = schedule_state("schedule-1", inputs("0 3 * * *"), before_receipt);
        seed(&store, instance, Some(before.clone()));
        open_step(
            &store,
            JournalAction::Update,
            ExpectedCheckpoint::update(
                before,
                schedule_state("schedule-1", inputs("0 3 * * *"), after_receipt),
            )
            .unwrap(),
        );

        let error = recover_workspace_with_approval(&router.client(), &config_file, |_| Ok(true))
            .await
            .expect_err("write-only executable text cannot be proven applied");

        assert!(matches!(error, RecoverWorkspaceError::ManualIntervention));
        let rendered = format!("{error:?}{error}");
        assert!(!rendered.contains(COMMAND_CANARY));
        assert!(!rendered.contains(SCRIPT_CANARY));
        assert_ne!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    }
}

#[tokio::test]
async fn uncertain_delete_is_confirmed_by_authoritative_absence_and_otherwise_no_change() {
    let absent_router = Router::start(routes(vec![
        ("GET /api/schedule.list", vec![ok("[]")]),
        (
            "GET /api/schedule.one",
            vec![status(
                "404 Not Found",
                r#"{"message":"Schedule not found"}"#,
            )],
        ),
    ]));
    let present = record("0 3 * * *", None);
    let present_router = Router::start(routes(vec![
        ("GET /api/schedule.list", vec![ok(list([&present]))]),
        ("GET /api/schedule.one", vec![ok(&present)]),
    ]));
    for (router, action, removed) in [
        (
            absent_router,
            RecoveryAction::CheckpointConfirmedSuccess,
            true,
        ),
        (present_router, RecoveryAction::ConfirmNoChange, false),
    ] {
        let (workspace, config_file) = workspace(
            "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api: {}\n",
        );
        let instance = InstanceIdentity::parse(&router.url).unwrap();
        let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
        let before = schedule_state("schedule-1", inputs("0 3 * * *"), receipt(1, None));
        seed(&store, instance, Some(before.clone()));
        open_step(
            &store,
            JournalAction::Delete,
            ExpectedCheckpoint::remove(before),
        );

        let result = recover_workspace_with_approval(&router.client(), &config_file, |preview| {
            assert_eq!(preview.action(), action);
            Ok(true)
        })
        .await
        .unwrap();

        assert_eq!(result.recovered_steps(), 1);
        assert_eq!(
            store
                .inspect()
                .unwrap()
                .unwrap()
                .resource(&address("schedule.nightly"))
                .is_none(),
            removed
        );
        assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
        no_mutation(&router);
    }
}
