mod support;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dokploy_cli::executor::{
    ApplyWorkspaceError, apply_workspace, destroy_workspace_with_approval,
};
use dokploy_state::{
    FailureCode, InstanceIdentity, RecoveryStatus, ResourceAddress, StateFile, StateStore,
};
use support::{
    APPLICATION_CREATE, ENVIRONMENT, ENVIRONMENTS, PROJECT_CREATE, Reply, Router,
    ScheduleTargetFixture, application_one, application_search, list, ok, project_topology,
    schedule_record, status,
};

const COMMAND_CANARY: &str = "schedule-command-canary-never-leak";
const SCRIPT_CANARY: &str = "schedule-script-canary-never-leak";
const ROTATED_COMMAND: &str = "rotated-command-canary-never-leak";
const ROTATED_SCRIPT: &str = "rotated-script-canary-never-leak";

fn address(value: &str) -> ResourceAddress {
    value.parse().unwrap()
}

fn config(schedules: &str) -> String {
    format!(
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      api: {{}}\n      worker: {{}}\n    schedules:\n{schedules}"
    )
}

/// One disabled Bash Schedule on `application.api` whose command comes from a file.
fn job(cron: &str, enabled: bool, extra: &str) -> String {
    format!(
        "      nightly:\n        name: nightly\n        target: application.api\n        cron_expression: \"{cron}\"\n        shell_type: bash\n        enabled: {enabled}\n        command: {{ file: command.sh }}\n{extra}"
    )
}

fn state(router: &Router, directory: &Path) -> StateFile {
    StateStore::new(directory, InstanceIdentity::parse(&router.url).unwrap())
        .unwrap()
        .inspect()
        .unwrap()
        .expect("state exists")
}

fn store(router: &Router, directory: &Path) -> StateStore {
    StateStore::new(directory, InstanceIdentity::parse(&router.url).unwrap()).unwrap()
}

fn base_routes(extra: Vec<(&'static str, Vec<Reply>)>) -> Vec<(&'static str, Vec<Reply>)> {
    let topology = project_topology(&format!(
        "{},{}",
        support::application_item("application-1", "api"),
        support::application_item("application-2", "worker")
    ));
    let mut routes = vec![
        (
            "GET /api/project.all",
            vec![
                ok("[]"),
                ok(topology.clone()),
                ok(topology.clone()),
                ok(topology),
            ],
        ),
        ("POST /api/project.create", vec![ok(PROJECT_CREATE)]),
        (
            "POST /api/application.create",
            vec![
                ok(APPLICATION_CREATE),
                ok(APPLICATION_CREATE.replace("application-1", "application-2")),
            ],
        ),
        ("GET /api/environment.byProjectId", vec![ok(ENVIRONMENTS)]),
        ("GET /api/environment.one", vec![ok(ENVIRONMENT)]),
        (
            "GET /api/application.search",
            vec![ok(application_search(&[
                ("application-1", "api"),
                ("application-2", "worker"),
            ]))],
        ),
        (
            "GET /api/application.one|application-1",
            vec![ok(application_one("application-1", "api"))],
        ),
        (
            "GET /api/application.one|application-2",
            vec![ok(application_one("application-2", "worker"))],
        ),
    ];
    routes.extend(extra);
    routes
}

fn count(log: &[String], prefix: &str) -> usize {
    log.iter()
        .filter(|request| request.starts_with(prefix))
        .count()
}

fn api_record(id: &str, application: &str, cron: &str, enabled: bool, command: &str) -> String {
    schedule_record(
        id,
        ScheduleTargetFixture::Application(application),
        "nightly",
        cron,
        "bash",
        enabled,
        command,
        None,
        None,
        None,
    )
}

fn assert_no_deployment_or_execution(router: &Router) {
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
    assert!(router.requests().iter().all(|request| {
        !request.contains(".deploy")
            && !request.contains(".redeploy")
            && !request.contains("runManually")
    }));
}

fn scan_for(directory: &Path, canary: &str) -> bool {
    fn walk(path: &Path, canary: &str) -> bool {
        if path.is_dir() {
            return fs::read_dir(path)
                .unwrap()
                .any(|entry| walk(&entry.unwrap().path(), canary));
        }
        fs::read(path).is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(canary))
    }
    walk(&directory.join(".dokploy"), canary)
}

fn workspace(config_text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(directory.path().join("command.sh"), COMMAND_CANARY).unwrap();
    fs::write(&config_file, config_text).unwrap();
    (directory, config_file)
}

/// The Schedule collection of `application-1` as it evolves across a create and an update.
fn staged_list(created: String, updated: String) -> Reply {
    Reply::Dynamic(Arc::new(move |log| {
        if count(log, "POST /api/schedule.update") > 0 {
            ok(list([&updated]))
        } else if count(log, "POST /api/schedule.create") > 0 {
            ok(list([&created]))
        } else {
            ok("[]")
        }
    }))
}

fn staged_one(created: String, updated: String) -> Reply {
    Reply::Dynamic(Arc::new(move |log| {
        if count(log, "POST /api/schedule.update") > 0 {
            ok(&updated)
        } else {
            ok(&created)
        }
    }))
}

#[tokio::test]
async fn create_checkpoints_the_target_dependency_and_the_next_apply_is_a_no_op() {
    let record = api_record(
        "schedule-1",
        "application-1",
        "0 3 * * *",
        false,
        COMMAND_CANARY,
    );
    let router = Router::start(base_routes(vec![
        ("POST /api/schedule.create", vec![ok(&record)]),
        (
            "GET /api/schedule.list|application-1",
            vec![support::switch_after(
                "POST /api/schedule.create",
                ok("[]"),
                ok(list([&record])),
            )],
        ),
        ("GET /api/schedule.list|application-2", vec![ok("[]")]),
        ("GET /api/schedule.one", vec![ok(&record)]),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));
    let client = router.client();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(
        summary.applied(),
        5,
        "project, environment, two applications, Schedule"
    );
    let create = router.matching("POST /api/schedule.create");
    assert_eq!(create.len(), 1);
    for expected in [
        r#""scheduleType":"application""#,
        r#""applicationId":"application-1""#,
        r#""name":"nightly""#,
        r#""cronExpression":"0 3 * * *""#,
        r#""shellType":"bash""#,
        r#""enabled":false"#,
    ] {
        assert!(create[0].contains(expected), "{expected}");
    }
    assert!(
        create[0].contains(COMMAND_CANARY),
        "the command crosses only the mutation seam"
    );
    let stored_state = state(&router, directory.path());
    let stored = stored_state.resource(&address("schedule.nightly")).unwrap();
    assert_eq!(stored.remote_id().as_str(), "schedule-1");
    assert_eq!(
        stored.last_applied().as_json(),
        &serde_json::json!({
            "target": "application.api",
            "name": "nightly",
            "cron_expression": "0 3 * * *",
            "shell_type": "bash",
            "enabled": false
        })
    );
    assert_eq!(stored.dependencies(), &[address("application.api")]);
    assert_eq!(
        stored.containment(),
        Some(&address("environment.production"))
    );
    assert_eq!(stored.sensitive_inputs().paths().count(), 1);
    assert!(!scan_for(directory.path(), COMMAND_CANARY));

    let converged = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(converged.applied(), 0);
    assert_eq!(router.matching("POST /api/schedule.create").len(), 1);
    assert_eq!(
        store(&router, directory.path()).recovery_status().unwrap(),
        RecoveryStatus::Clean
    );
    assert_no_deployment_or_execution(&router);
}

#[tokio::test]
async fn in_place_update_resends_complete_state_including_rotated_executables() {
    let before = api_record(
        "schedule-1",
        "application-1",
        "0 3 * * *",
        false,
        COMMAND_CANARY,
    );
    let after = schedule_record(
        "schedule-1",
        ScheduleTargetFixture::Application("application-1"),
        "nightly",
        "5 4 * * 1",
        "bash",
        true,
        ROTATED_COMMAND,
        Some(ROTATED_SCRIPT),
        None,
        None,
    );
    let router = Router::start(base_routes(vec![
        ("POST /api/schedule.create", vec![ok(&before)]),
        ("POST /api/schedule.update", vec![ok(&after)]),
        (
            "GET /api/schedule.list|application-1",
            vec![staged_list(before.clone(), after.clone())],
        ),
        ("GET /api/schedule.list|application-2", vec![ok("[]")]),
        (
            "GET /api/schedule.one",
            vec![staged_one(before.clone(), after.clone())],
        ),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));
    fs::write(directory.path().join("script.sh"), ROTATED_SCRIPT).unwrap();
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(directory.path().join("command.sh"), ROTATED_COMMAND).unwrap();
    fs::write(
        &config_file,
        config(&job(
            "5 4 * * 1",
            true,
            "        script: { file: script.sh }\n",
        )),
    )
    .unwrap();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    let lines = router.lines();
    let one = lines
        .iter()
        .rposition(|line| line.starts_with("GET /api/schedule.one"))
        .unwrap();
    let update = lines
        .iter()
        .position(|line| line.starts_with("POST /api/schedule.update"))
        .unwrap();
    assert!(one < update, "the update is preceded by a fresh read");
    let body = &router.matching("POST /api/schedule.update")[0];
    assert!(body.contains(r#""scheduleId":"schedule-1""#));
    assert!(body.contains(r#""cronExpression":"5 4 * * 1""#));
    assert!(body.contains(r#""enabled":true"#));
    assert!(body.contains(ROTATED_COMMAND));
    assert!(body.contains(ROTATED_SCRIPT));
    assert!(
        !body.contains("applicationId"),
        "the target is never rewritten"
    );
    assert_eq!(router.matching("POST /api/schedule.create").len(), 1);
    assert!(router.matching("POST /api/schedule.delete").is_empty());
    let stored_state = state(&router, directory.path());
    let stored = stored_state.resource(&address("schedule.nightly")).unwrap();
    assert_eq!(
        stored.last_applied().as_json()["cron_expression"],
        "5 4 * * 1"
    );
    assert_eq!(stored.last_applied().as_json()["enabled"], true);
    assert_eq!(stored.sensitive_inputs().paths().count(), 2);
    for canary in [
        COMMAND_CANARY,
        SCRIPT_CANARY,
        ROTATED_COMMAND,
        ROTATED_SCRIPT,
    ] {
        assert!(!scan_for(directory.path(), canary));
    }
    assert_no_deployment_or_execution(&router);
}

#[tokio::test]
async fn target_change_deletes_before_creating_under_the_new_target() {
    let api = api_record(
        "schedule-1",
        "application-1",
        "0 3 * * *",
        false,
        COMMAND_CANARY,
    );
    let worker = api_record(
        "schedule-2",
        "application-2",
        "0 3 * * *",
        false,
        COMMAND_CANARY,
    );
    let (api_list, worker_list) = (api.clone(), worker.clone());
    let router = Router::start(base_routes(vec![
        ("POST /api/schedule.create", vec![ok(&api), ok(&worker)]),
        (
            "GET /api/schedule.list|application-1",
            vec![Reply::Dynamic(Arc::new(move |log| {
                if count(log, "POST /api/schedule.create") >= 1
                    && count(log, "POST /api/schedule.delete") == 0
                {
                    ok(list([&api_list]))
                } else {
                    ok("[]")
                }
            }))],
        ),
        (
            "GET /api/schedule.list|application-2",
            vec![Reply::Dynamic(Arc::new(move |log| {
                if count(log, "POST /api/schedule.create") >= 2 {
                    ok(list([&worker_list]))
                } else {
                    ok("[]")
                }
            }))],
        ),
        ("GET /api/schedule.one", vec![ok(&api)]),
        ("POST /api/schedule.delete", vec![ok("true")]),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        config(&job("0 3 * * *", false, "").replace("application.api", "application.worker")),
    )
    .unwrap();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    let lines = router.lines();
    let remove = lines
        .iter()
        .position(|line| line.starts_with("POST /api/schedule.delete"))
        .unwrap();
    let create = lines
        .iter()
        .rposition(|line| line.starts_with("POST /api/schedule.create"))
        .unwrap();
    assert!(remove < create);
    assert!(
        router.matching("POST /api/schedule.delete")[0].contains(r#""scheduleId":"schedule-1""#)
    );
    assert!(
        router.matching("POST /api/schedule.create")[1]
            .contains(r#""applicationId":"application-2""#)
    );
    assert!(router.matching("POST /api/schedule.update").is_empty());
    let stored_state = state(&router, directory.path());
    let stored = stored_state.resource(&address("schedule.nightly")).unwrap();
    assert_eq!(stored.remote_id().as_str(), "schedule-2");
    assert_eq!(stored.dependencies(), &[address("application.worker")]);
    assert_no_deployment_or_execution(&router);
}

#[tokio::test]
async fn removing_a_schedule_and_its_target_deletes_the_schedule_first() {
    let record = api_record(
        "schedule-1",
        "application-1",
        "0 3 * * *",
        false,
        COMMAND_CANARY,
    );
    let router = Router::start(base_routes(vec![
        ("POST /api/schedule.create", vec![ok(&record)]),
        (
            "GET /api/schedule.list|application-1",
            vec![Reply::Dynamic(Arc::new({
                let record = record.clone();
                move |log| {
                    if count(log, "POST /api/schedule.create") >= 1
                        && count(log, "POST /api/schedule.delete") == 0
                    {
                        ok(list([&record]))
                    } else {
                        ok("[]")
                    }
                }
            }))],
        ),
        ("GET /api/schedule.list|application-2", vec![ok("[]")]),
        ("GET /api/schedule.one", vec![ok(&record)]),
        ("POST /api/schedule.delete", vec![ok("true")]),
        ("POST /api/application.delete", vec![ok(r#"{"ok":true}"#)]),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      worker: {}\n",
    )
    .unwrap();

    apply_workspace(&client, &config_file).await.unwrap();

    let lines = router.lines();
    let remove = lines
        .iter()
        .position(|line| line.starts_with("POST /api/schedule.delete"))
        .expect("the Schedule is removed");
    let delete = lines
        .iter()
        .position(|line| line.starts_with("POST /api/application.delete"))
        .expect("the target is removed");
    assert!(remove < delete, "a Schedule is removed before its target");
    assert!(
        state(&router, directory.path())
            .resource(&address("schedule.nightly"))
            .is_none()
    );
    assert_no_deployment_or_execution(&router);
}

#[tokio::test]
async fn destroy_removes_schedules_before_their_targets() {
    let record = api_record(
        "schedule-1",
        "application-1",
        "0 3 * * *",
        false,
        COMMAND_CANARY,
    );
    let router = Router::start(base_routes(vec![
        ("POST /api/schedule.create", vec![ok(&record)]),
        (
            "GET /api/schedule.list|application-1",
            vec![Reply::Dynamic(Arc::new({
                let record = record.clone();
                move |log| {
                    if count(log, "POST /api/schedule.create") >= 1
                        && count(log, "POST /api/schedule.delete") == 0
                    {
                        ok(list([&record]))
                    } else {
                        ok("[]")
                    }
                }
            }))],
        ),
        ("GET /api/schedule.list|application-2", vec![ok("[]")]),
        ("GET /api/schedule.one", vec![ok(&record)]),
        ("POST /api/schedule.delete", vec![ok("true")]),
        ("POST /api/application.delete", vec![ok(r#"{"ok":true}"#)]),
        ("POST /api/project.remove", vec![ok(r#"{"ok":true}"#)]),
        ("POST /api/environment.remove", vec![ok(r#"{"ok":true}"#)]),
    ]));
    let (_directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();

    destroy_workspace_with_approval(&client, &config_file, |plan| {
        let order = plan
            .changes()
            .iter()
            .map(|change| change.address().to_string())
            .collect::<Vec<_>>();
        let position = |name: &str| order.iter().position(|item| item == name).unwrap();
        assert!(position("schedule.nightly") < position("application.api"));
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(router.matching("POST /api/schedule.delete").len(), 1);
    assert_no_deployment_or_execution(&router);
}

#[tokio::test]
async fn definitive_create_rejection_fails_the_step_without_retry_or_state() {
    let router = Router::start(base_routes(vec![
        (
            "POST /api/schedule.create",
            vec![status(
                "400 Bad Request",
                r#"{"message":"invalid schedule"}"#,
            )],
        ),
        ("GET /api/schedule.list", vec![ok("[]")]),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));

    let error = apply_workspace(&router.client(), &config_file)
        .await
        .expect_err("a definitive rejection fails the apply");

    assert!(matches!(
        error,
        ApplyWorkspaceError::RemoteMutation {
            code: FailureCode::Validation
        }
    ));
    assert!(!format!("{error:?}{error}").contains(COMMAND_CANARY));
    assert_eq!(router.matching("POST /api/schedule.create").len(), 1);
    assert!(
        state(&router, directory.path())
            .resource(&address("schedule.nightly"))
            .is_none()
    );
    assert!(!scan_for(directory.path(), COMMAND_CANARY));
    assert_no_deployment_or_execution(&router);
}

#[tokio::test]
async fn outcome_unknown_create_stays_in_progress_and_is_never_retried() {
    let router = Router::start(base_routes(vec![
        ("POST /api/schedule.create", vec![Reply::Drop]),
        ("GET /api/schedule.list", vec![ok("[]")]),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));

    let error = apply_workspace(&router.client(), &config_file)
        .await
        .expect_err("an unknown outcome stops the apply");

    assert!(matches!(
        error,
        ApplyWorkspaceError::RemoteMutation {
            code: FailureCode::TransportOutcomeUnknown
        }
    ));
    assert_eq!(router.matching("POST /api/schedule.create").len(), 1);
    assert_ne!(
        store(&router, directory.path()).recovery_status().unwrap(),
        RecoveryStatus::Clean,
        "the interrupted step remains recoverable"
    );
    assert!(!scan_for(directory.path(), COMMAND_CANARY));
}

#[tokio::test]
async fn a_collision_found_by_the_sdk_preflight_is_never_adopted_and_stays_recoverable() {
    let existing = api_record("schedule-9", "application-1", "0 3 * * *", false, "other");
    let router = Router::start(base_routes(vec![
        // Discovery cannot see the key because the target does not exist yet; the SDK
        // preflight then finds it occupied.
        (
            "GET /api/schedule.list|application-1",
            vec![ok(list([&existing]))],
        ),
        ("POST /api/schedule.create", vec![ok(&existing)]),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));

    let error = apply_workspace(&router.client(), &config_file).await;

    // The SDK reports the unproven situation as an unknown outcome, so the step stays
    // recoverable and nothing is mutated or adopted.
    assert!(matches!(
        error,
        Err(ApplyWorkspaceError::RemoteMutation {
            code: FailureCode::TransportOutcomeUnknown
        })
    ));
    assert_ne!(
        store(&router, directory.path()).recovery_status().unwrap(),
        RecoveryStatus::Clean
    );
    assert!(router.matching("POST /api/schedule.create").is_empty());
    assert!(
        state(&router, directory.path())
            .resource(&address("schedule.nightly"))
            .is_none()
    );
}

#[tokio::test]
async fn unmanaged_command_is_never_created_as_an_empty_command() {
    let router = Router::start(base_routes(vec![(
        "GET /api/schedule.list",
        vec![ok("[]")],
    )]));
    let (directory, config_file) = workspace(&config(
        "      nightly:\n        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: bash\n        enabled: false\n        lifecycle: { protect: true }\n",
    ));

    let error = apply_workspace(&router.client(), &config_file)
        .await
        .expect_err("a command cannot be invented");

    assert!(matches!(error, ApplyWorkspaceError::PlanBlocked));
    assert!(router.matching("POST /api/schedule.create").is_empty());
    assert!(
        store(&router, directory.path())
            .inspect()
            .unwrap()
            .is_none_or(|state| state.resource(&address("schedule.nightly")).is_none())
    );
}

#[tokio::test]
async fn updating_a_schedule_whose_command_is_unmanaged_is_refused_before_any_mutation() {
    let before = api_record(
        "schedule-1",
        "application-1",
        "0 3 * * *",
        false,
        COMMAND_CANARY,
    );
    let router = Router::start(base_routes(vec![
        ("POST /api/schedule.create", vec![ok(&before)]),
        (
            "GET /api/schedule.list|application-1",
            vec![support::switch_after(
                "POST /api/schedule.create",
                ok("[]"),
                ok(list([&before])),
            )],
        ),
        ("GET /api/schedule.list|application-2", vec![ok("[]")]),
        ("GET /api/schedule.one", vec![ok(&before)]),
        ("POST /api/schedule.update", vec![ok(&before)]),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        config(
            "      nightly:\n        name: nightly\n        target: application.api\n        cron_expression: \"5 4 * * 1\"\n        shell_type: bash\n        enabled: false\n        lifecycle: { protect: true }\n",
        ),
    )
    .unwrap();

    let error = apply_workspace(&client, &config_file)
        .await
        .expect_err("unmanaged executable text can never be resent");

    assert!(matches!(error, ApplyWorkspaceError::UnsupportedChange));
    assert!(router.matching("POST /api/schedule.update").is_empty());
    assert!(
        state(&router, directory.path())
            .resource(&address("schedule.nightly"))
            .is_some()
    );
    assert_no_deployment_or_execution(&router);
}

#[tokio::test]
async fn updating_around_an_unmanaged_remote_script_is_refused_before_any_mutation() {
    let without_script = api_record(
        "schedule-1",
        "application-1",
        "0 3 * * *",
        false,
        COMMAND_CANARY,
    );
    let with_script_before = schedule_record(
        "schedule-1",
        ScheduleTargetFixture::Application("application-1"),
        "nightly",
        "0 3 * * *",
        "bash",
        false,
        COMMAND_CANARY,
        Some("script-added-out-of-band"),
        None,
        None,
    );
    let out_of_band = Arc::new(AtomicBool::new(false));
    let flag = out_of_band.clone();
    let (plain, scripted) = (without_script.clone(), with_script_before);
    let (plain_one, scripted_one) = (plain.clone(), scripted.clone());
    let flag_one = out_of_band.clone();
    let router = Router::start(base_routes(vec![
        ("POST /api/schedule.create", vec![ok(&without_script)]),
        (
            "GET /api/schedule.list|application-1",
            vec![Reply::Dynamic(Arc::new(move |log| {
                if count(log, "POST /api/schedule.create") == 0 {
                    ok("[]")
                } else if flag.load(Ordering::SeqCst) {
                    ok(list([&scripted]))
                } else {
                    ok(list([&plain]))
                }
            }))],
        ),
        ("GET /api/schedule.list|application-2", vec![ok("[]")]),
        (
            "GET /api/schedule.one",
            vec![Reply::Dynamic(Arc::new(move |_| {
                if flag_one.load(Ordering::SeqCst) {
                    ok(&scripted_one)
                } else {
                    ok(&plain_one)
                }
            }))],
        ),
        ("POST /api/schedule.update", vec![ok(&without_script)]),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();
    out_of_band.store(true, Ordering::SeqCst);
    fs::write(&config_file, config(&job("5 4 * * 1", false, ""))).unwrap();

    let error = apply_workspace(&client, &config_file)
        .await
        .expect_err("an update would silently drop a script it cannot resend");

    assert!(matches!(error, ApplyWorkspaceError::UnsupportedChange));
    assert!(router.matching("POST /api/schedule.update").is_empty());
    assert!(!scan_for(directory.path(), "script-added-out-of-band"));
    assert_no_deployment_or_execution(&router);
}

#[tokio::test]
async fn compose_target_uses_the_typed_compose_service_identity() {
    let compose_record = schedule_record(
        "schedule-1",
        ScheduleTargetFixture::Compose("compose-1", "worker"),
        "worker-job",
        "@daily",
        "sh",
        false,
        COMMAND_CANARY,
        None,
        None,
        None,
    );
    let router = Router::start(base_routes(vec![
        (
            "POST /api/compose.create",
            vec![ok(
                r#"{"composeId":"compose-1","environmentId":"environment-1","name":"stack","appName":"stack","serverId":null}"#,
            )],
        ),
        ("POST /api/schedule.create", vec![ok(&compose_record)]),
        (
            "GET /api/schedule.list|compose-1",
            vec![support::switch_after(
                "POST /api/schedule.create",
                ok("[]"),
                ok(list([&compose_record])),
            )],
        ),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(directory.path().join("compose.yaml"), "services: {}\n").unwrap();
    fs::write(directory.path().join("command.sh"), COMMAND_CANARY).unwrap();
    fs::write(
        &config_file,
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    compose:\n      stack:\n        document: { file: compose.yaml }\n    schedules:\n      worker:\n        name: worker-job\n        target: compose.stack\n        service_name: worker\n        cron_expression: \"@daily\"\n        shell_type: sh\n        enabled: false\n        command: { file: command.sh }\n",
    )
    .unwrap();

    apply_workspace(&router.client(), &config_file)
        .await
        .unwrap();

    let create = &router.matching("POST /api/schedule.create")[0];
    assert!(create.contains(r#""scheduleType":"compose""#));
    assert!(create.contains(r#""composeId":"compose-1""#));
    assert!(create.contains(r#""serviceName":"worker""#));
    assert!(router.matching("GET /api/schedule.list")[0].contains("scheduleType=compose"));
    let stored_state = state(&router, directory.path());
    let stored = stored_state.resource(&address("schedule.worker")).unwrap();
    assert_eq!(stored.dependencies(), &[address("compose.stack")]);
    assert_eq!(stored.last_applied().as_json()["service_name"], "worker");
    assert!(!scan_for(directory.path(), COMMAND_CANARY));
    assert_no_deployment_or_execution(&router);
}

#[tokio::test]
async fn an_empty_command_is_rejected_before_any_journal_step_or_mutation() {
    let router = Router::start(base_routes(vec![(
        "GET /api/schedule.list",
        vec![ok("[]")],
    )]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));
    fs::write(directory.path().join("command.sh"), "").unwrap();

    let error = apply_workspace(&router.client(), &config_file)
        .await
        .expect_err("the SDK requires a nonempty command, so none is invented");

    assert!(
        matches!(
            error,
            ApplyWorkspaceError::InvalidCheckpoint | ApplyWorkspaceError::Desired(_)
        ),
        "{error:?}"
    );
    assert!(router.matching("POST /api/schedule.create").is_empty());
    if let Ok(RecoveryStatus::RecoveryRequired(summary)) =
        store(&router, directory.path()).recovery_status()
    {
        assert!(
            summary
                .steps()
                .iter()
                .all(|step| step.address().kind() != dokploy_state::ResourceKind::Schedule),
            "pre-mutation rejection opens no Schedule journal step"
        );
    }
}

#[tokio::test]
async fn an_ignored_field_is_preserved_from_the_fresh_remote_read_on_update() {
    let before = api_record(
        "schedule-1",
        "application-1",
        "0 3 * * *",
        false,
        COMMAND_CANARY,
    );
    let after = api_record(
        "schedule-1",
        "application-1",
        "0 3 * * *",
        true,
        COMMAND_CANARY,
    );
    let router = Router::start(base_routes(vec![
        ("POST /api/schedule.create", vec![ok(&before)]),
        ("POST /api/schedule.update", vec![ok(&after)]),
        (
            "GET /api/schedule.list|application-1",
            vec![staged_list(before.clone(), after.clone())],
        ),
        ("GET /api/schedule.list|application-2", vec![ok("[]")]),
        (
            "GET /api/schedule.one",
            vec![staged_one(before.clone(), after.clone())],
        ),
    ]));
    let (directory, config_file) = workspace(&config(&job("0 3 * * *", false, "")));
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        config(&job(
            "7 7 * * 7",
            true,
            "        lifecycle: { ignore_changes: [cron_expression] }\n",
        )),
    )
    .unwrap();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    let body = &router.matching("POST /api/schedule.update")[0];
    assert!(body.contains(r#""cronExpression":"0 3 * * *""#), "{body}");
    assert!(!body.contains("7 7 * * 7"));
    assert!(body.contains(r#""enabled":true"#));
    assert!(!scan_for(directory.path(), COMMAND_CANARY));
    assert_no_deployment_or_execution(&router);
}
