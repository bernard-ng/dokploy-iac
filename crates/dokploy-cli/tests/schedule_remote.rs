mod support;

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{
    DiscoverRemoteError, DiscoveryAuthority, ScheduleTopologyAuthority, discover_remote,
};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, PlanDiagnosticCode, PropertyObservation, PropertyPath,
    RemoteFailureKind, RemoteObservation, StoredState, plan,
};
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateFile,
};
use support::{
    ENVIRONMENT, ENVIRONMENTS, Reply, Router, ScheduleTargetFixture, application_one,
    application_search, list, ok, project_topology, schedule_record, status,
};

const COMMAND_CANARY: &str = "remote-command-canary-never-leak";
const SCRIPT_CANARY: &str = "remote-script-canary-never-leak";

fn address(value: &str) -> ResourceAddress {
    value.parse().unwrap()
}

#[allow(clippy::too_many_arguments)]
fn insert(
    state: &mut StateFile,
    name: &str,
    kind: ResourceKind,
    id: &str,
    containment: Option<&str>,
    dependencies: &[&str],
    inputs: serde_json::Value,
    protected: bool,
) {
    state
        .upsert_resource(
            address(name),
            ResourceState::new(
                kind,
                RemoteId::new(id).unwrap(),
                protected,
                ManagedInputs::try_from_json(inputs).unwrap(),
                containment.map(address),
                dependencies.iter().map(|value| address(value)).collect(),
            ),
        )
        .unwrap();
}

fn stored_inputs(cron: &str, name: &str) -> serde_json::Value {
    serde_json::json!({
        "target": "application.api",
        "name": name,
        "cron_expression": cron,
        "shell_type": "bash",
        "enabled": false
    })
}

fn state(router: &Router, with_schedule: bool) -> StateFile {
    let mut state = StateFile::new(
        "0.1.0".parse().unwrap(),
        InstanceIdentity::parse(&router.url).unwrap(),
    );
    insert(
        &mut state,
        "project.platform",
        ResourceKind::Project,
        "project-1",
        None,
        &[],
        serde_json::json!({}),
        false,
    );
    insert(
        &mut state,
        "environment.production",
        ResourceKind::Environment,
        "environment-1",
        Some("project.platform"),
        &[],
        serde_json::json!({}),
        false,
    );
    for (name, id) in [("api", "application-1"), ("worker", "application-2")] {
        insert(
            &mut state,
            &format!("application.{name}"),
            ResourceKind::Application,
            id,
            Some("environment.production"),
            &[],
            serde_json::json!({}),
            false,
        );
    }
    if with_schedule {
        insert(
            &mut state,
            "schedule.nightly",
            ResourceKind::Schedule,
            "schedule-1",
            Some("environment.production"),
            &["application.api"],
            stored_inputs("0 3 * * *", "nightly"),
            true,
        );
    }
    state
}

/// A protected Schedule with an unmanaged command, the shape that needs no secret source.
fn desired(target: &str, name: &str, cron: &str) -> dokploy_cli::desired::CompiledDesired {
    compile_desired(
        &DokployConfig::parse(&format!(
            "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api: {{}}\n        worker: {{}}\n      schedules:\n        nightly:\n          name: {name}\n          target: {target}\n          cron_expression: \"{cron}\"\n          shell_type: bash\n          enabled: false\n          lifecycle: {{ protect: true }}\n"
        ))
        .unwrap(),
        ConfigDigest::parse("a".repeat(64)).unwrap(),
    )
    .unwrap()
}

fn record(id: &str, application: &str, name: &str, cron: &str) -> String {
    schedule_record(
        id,
        ScheduleTargetFixture::Application(application),
        name,
        cron,
        "bash",
        false,
        COMMAND_CANARY,
        Some(SCRIPT_CANARY),
        None,
        None,
    )
}

fn topology_routes(
    schedule_routes: Vec<(&'static str, Vec<Reply>)>,
) -> Vec<(&'static str, Vec<Reply>)> {
    let mut routes = vec![
        (
            "GET /api/project.all",
            vec![ok(project_topology(&format!(
                "{},{}",
                support::application_item("application-1", "api"),
                support::application_item("application-2", "worker")
            )))],
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
    routes.extend(schedule_routes);
    routes
}

async fn observe(
    router: &Router,
    state: &StateFile,
    desired: &dokploy_cli::desired::CompiledDesired,
    authority: DiscoveryAuthority,
) -> Result<dokploy_core::RemoteState, DiscoverRemoteError> {
    discover_remote(&router.client(), desired, state, authority).await
}

fn schedule_observation(remote: &dokploy_core::RemoteState) -> Option<&RemoteObservation> {
    remote.observation(&address("schedule.nightly"))
}

fn plan_of(
    desired: &dokploy_cli::desired::CompiledDesired,
    state: &StateFile,
    remote: &dokploy_core::RemoteState,
) -> dokploy_core::Plan {
    plan(
        desired.desired_state(),
        &StoredState::try_from_state(state).unwrap(),
        remote,
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
async fn authoritative_collection_absence_is_missing_and_unmanaged_command_blocks_creation() {
    let router = Router::start(topology_routes(vec![(
        "GET /api/schedule.list|application-1",
        vec![ok("[]")],
    )]));
    let state = state(&router, false);
    let desired = desired("application.api", "nightly", "0 3 * * *");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan_of(&desired, &state, &remote);

    assert!(matches!(
        schedule_observation(&remote),
        Some(RemoteObservation::Missing)
    ));
    // The command is required on create and an unmanaged command can never be invented.
    assert!(!plan.applyable());
    assert!(plan.diagnostics().iter().any(|issue| {
        issue.code() == PlanDiagnosticCode::MissingCreateProperty
            && issue.property() == Some(&PropertyPath::Command)
    }));
    assert_eq!(router.matching("GET /api/schedule.list").len(), 1);
    assert!(router.matching("GET /api/schedule.one").is_empty());
    assert_read_only(&router);
}

#[tokio::test]
async fn partial_authority_downgrades_absence_to_an_unavailable_observation() {
    let router = Router::start(topology_routes(vec![(
        "GET /api/schedule.list|application-1",
        vec![ok("[]")],
    )]));
    let state = state(&router, false);
    let desired = desired("application.api", "nightly", "0 3 * * *");
    let authority = DiscoveryAuthority {
        schedules: ScheduleTopologyAuthority::Partial,
        ..DiscoveryAuthority::reconciliation()
    };

    let remote = observe(&router, &state, &desired, authority).await.unwrap();

    assert!(matches!(
        schedule_observation(&remote),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    assert!(!plan_of(&desired, &state, &remote).applyable());
}

#[tokio::test]
async fn matching_direct_and_collection_records_are_present_without_leaking_executables() {
    let remote_record = record("schedule-1", "application-1", "nightly", "0 3 * * *");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/schedule.list|application-1",
            vec![ok(list([&remote_record]))],
        ),
        ("GET /api/schedule.one", vec![ok(&remote_record)]),
    ]));
    let state = state(&router, true);
    let desired = desired("application.api", "nightly", "0 3 * * *");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan_of(&desired, &state, &remote);

    let Some(RemoteObservation::Present(resource)) = schedule_observation(&remote) else {
        panic!("the Schedule is present");
    };
    assert_eq!(resource.remote_id().as_str(), "schedule-1");
    for path in [
        PropertyPath::Target,
        PropertyPath::Name,
        PropertyPath::CronExpression,
        PropertyPath::ShellType,
        PropertyPath::Enabled,
    ] {
        assert!(
            matches!(
                resource.property(&path),
                Some(PropertyObservation::Known(_))
            ),
            "{path}"
        );
    }
    let rendered = format!("{remote:?} {plan:?}");
    assert!(!rendered.contains(COMMAND_CANARY));
    assert!(!rendered.contains(SCRIPT_CANARY));
    assert!(
        !String::from_utf8(plan.to_json_bytes())
            .unwrap()
            .contains(COMMAND_CANARY)
    );
    assert!(plan.applyable() && plan.changes().is_empty());
    assert_read_only(&router);
}

#[tokio::test]
async fn cron_drift_plans_an_in_place_update() {
    let remote_record = record("schedule-1", "application-1", "nightly", "9 9 * * 9");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/schedule.list|application-1",
            vec![ok(list([&remote_record]))],
        ),
        ("GET /api/schedule.one", vec![ok(&remote_record)]),
    ]));
    let state = state(&router, true);
    let desired = desired("application.api", "nightly", "0 3 * * *");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan_of(&desired, &state, &remote);

    let change = plan
        .changes()
        .iter()
        .find(|change| change.address() == &address("schedule.nightly"))
        .expect("drift is planned");
    assert_eq!(change.kind(), ChangeKind::Update);
    assert!(
        change
            .fields()
            .iter()
            .any(|field| field.key() == &PropertyPath::CronExpression)
    );
}

#[tokio::test]
async fn an_unmanaged_schedule_at_the_collision_key_blocks_planning_and_is_never_adopted() {
    let existing = record("schedule-9", "application-1", "nightly", "0 3 * * *");
    let router = Router::start(topology_routes(vec![(
        "GET /api/schedule.list|application-1",
        vec![ok(list([&existing]))],
    )]));
    let state = state(&router, false);
    let desired = desired("application.api", "nightly", "0 3 * * *");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan_of(&desired, &state, &remote);

    assert!(matches!(
        schedule_observation(&remote),
        Some(RemoteObservation::Present(resource)) if resource.remote_id().as_str() == "schedule-9"
    ));
    assert!(!plan.applyable());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|issue| issue.code() == PlanDiagnosticCode::UnmanagedAddressCollision)
    );
    assert!(plan.changes().iter().all(|change| {
        change.address().kind() != ResourceKind::Schedule || change.kind() == ChangeKind::NoOp
    }));
}

#[tokio::test]
async fn contradictory_direct_and_collection_reads_fail_closed() {
    let direct = record("schedule-1", "application-1", "nightly", "1 1 * * *");
    let listed = record("schedule-1", "application-1", "nightly", "2 2 * * *");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/schedule.list|application-1",
            vec![ok(list([&listed]))],
        ),
        ("GET /api/schedule.one", vec![ok(&direct)]),
    ]));
    let state = state(&router, true);
    let desired = desired("application.api", "nightly", "0 3 * * *");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        schedule_observation(&remote),
        Some(RemoteObservation::Unavailable(_))
    ));
    assert!(!plan_of(&desired, &state, &remote).applyable());
}

#[tokio::test]
async fn duplicate_names_and_privileged_targets_make_the_collection_unavailable() {
    let first = record("schedule-1", "application-1", "nightly", "0 3 * * *");
    let duplicate = record("schedule-2", "application-1", "nightly", "0 4 * * *");
    let privileged = r#"{"scheduleId":"schedule-3","name":"host-job","description":null,"cronExpression":"0 3 * * *","shellType":"bash","scheduleType":"server","command":"x","script":null,"applicationId":null,"composeId":null,"serverId":"server-1","serviceName":null,"enabled":false,"timezone":null}"#;
    for entries in [
        vec![first.clone(), duplicate],
        vec![first.clone(), privileged.to_owned()],
    ] {
        let router = Router::start(topology_routes(vec![
            (
                "GET /api/schedule.list|application-1",
                vec![ok(list(entries))],
            ),
            ("GET /api/schedule.one", vec![ok(&first)]),
        ]));
        let state = state(&router, true);
        let desired = desired("application.api", "nightly", "0 3 * * *");

        let remote = observe(
            &router,
            &state,
            &desired,
            DiscoveryAuthority::reconciliation(),
        )
        .await
        .unwrap();

        assert!(matches!(
            schedule_observation(&remote),
            Some(RemoteObservation::Unavailable(_))
        ));
        assert!(!plan_of(&desired, &state, &remote).applyable());
    }
}

#[tokio::test]
async fn direct_not_found_is_absence_only_from_an_authoritative_collection() {
    let routes = || {
        topology_routes(vec![
            ("GET /api/schedule.list|application-1", vec![ok("[]")]),
            (
                "GET /api/schedule.one",
                vec![status(
                    "404 Not Found",
                    r#"{"message":"Schedule not found"}"#,
                )],
            ),
        ])
    };
    let router = Router::start(routes());
    let state_value = state(&router, true);
    let desired_value = desired("application.api", "nightly", "0 3 * * *");
    let remote = observe(
        &router,
        &state_value,
        &desired_value,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    assert!(matches!(
        schedule_observation(&remote),
        Some(RemoteObservation::Missing)
    ));

    let partial = Router::start(routes());
    let state_value = state(&partial, true);
    let remote = observe(
        &partial,
        &state_value,
        &desired_value,
        DiscoveryAuthority {
            schedules: ScheduleTopologyAuthority::Partial,
            ..DiscoveryAuthority::reconciliation()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        schedule_observation(&remote),
        Some(RemoteObservation::Unavailable(_))
    ));

    // A 404 while the collection still lists the identity is contradictory.
    let listed = record("schedule-1", "application-1", "nightly", "0 3 * * *");
    let contradictory = Router::start(topology_routes(vec![
        (
            "GET /api/schedule.list|application-1",
            vec![ok(list([&listed]))],
        ),
        (
            "GET /api/schedule.one",
            vec![status(
                "404 Not Found",
                r#"{"message":"Schedule not found"}"#,
            )],
        ),
    ]));
    let state_value = state(&contradictory, true);
    let remote = observe(
        &contradictory,
        &state_value,
        &desired_value,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    assert!(matches!(
        schedule_observation(&remote),
        Some(RemoteObservation::Unavailable(_))
    ));
}

#[tokio::test]
async fn a_rename_that_would_land_on_another_schedule_fails_closed() {
    let current = record("schedule-1", "application-1", "nightly", "0 3 * * *");
    let other = record("schedule-2", "application-1", "other", "0 4 * * *");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/schedule.list|application-1",
            vec![ok(list([&current, &other]))],
        ),
        ("GET /api/schedule.one", vec![ok(&current)]),
    ]));
    let state = state(&router, true);
    let desired = desired("application.api", "other", "0 3 * * *");

    let error = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("a rename onto an occupied name is a collision");

    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateScheduleCollision
    ));
}

#[tokio::test]
async fn a_direct_record_on_another_target_is_a_topology_conflict() {
    let moved = record("schedule-1", "application-2", "nightly", "0 3 * * *");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/schedule.list|application-2",
            vec![ok(list([&moved]))],
        ),
        ("GET /api/schedule.list|application-1", vec![ok("[]")]),
        ("GET /api/schedule.one", vec![ok(&moved)]),
    ]));
    let state = state(&router, true);
    let desired = desired("application.api", "nightly", "0 3 * * *");

    let error = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("the identity belongs to a different target");

    assert!(matches!(
        error,
        DiscoverRemoteError::ScheduleTopologyConflict
    ));
}

#[tokio::test]
async fn a_target_change_reads_the_new_collection_and_blocks_on_a_collision_there() {
    let current = record("schedule-1", "application-1", "nightly", "0 3 * * *");
    let colliding = record("schedule-7", "application-2", "nightly", "0 3 * * *");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/schedule.list|application-1",
            vec![ok(list([&current]))],
        ),
        (
            "GET /api/schedule.list|application-2",
            vec![ok(list([&colliding]))],
        ),
        ("GET /api/schedule.one", vec![ok(&current)]),
    ]));
    let state = state(&router, true);
    let desired = desired("application.worker", "nightly", "0 3 * * *");

    let error = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("a replacement must not land on an existing Schedule");

    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateScheduleCollision
    ));
}

#[tokio::test]
async fn a_target_change_to_a_free_name_plans_replacement() {
    let current = record("schedule-1", "application-1", "nightly", "0 3 * * *");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/schedule.list|application-1",
            vec![ok(list([&current]))],
        ),
        ("GET /api/schedule.list|application-2", vec![ok("[]")]),
        ("GET /api/schedule.one", vec![ok(&current)]),
    ]));
    let mut state = state(&router, false);
    insert(
        &mut state,
        "schedule.nightly",
        ResourceKind::Schedule,
        "schedule-1",
        Some("environment.production"),
        &["application.api"],
        stored_inputs("0 3 * * *", "nightly"),
        false,
    );
    // Without protection the replacement is permitted; the desired config must match.
    let desired = compile_desired(
        &DokployConfig::parse(
            "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api: {}\n        worker: {}\n      schedules:\n        nightly:\n          name: nightly\n          target: application.worker\n          cron_expression: \"0 3 * * *\"\n          shell_type: bash\n          enabled: false\n          lifecycle: { protect: true }\n",
        )
        .unwrap(),
        ConfigDigest::parse("a".repeat(64)).unwrap(),
    )
    .unwrap();

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan_of(&desired, &state, &remote);

    let Some(RemoteObservation::Present(resource)) = schedule_observation(&remote) else {
        panic!("the Schedule is present");
    };
    // The observation describes the stored physical target, so the move is a replacement.
    assert!(matches!(
        resource.property(&PropertyPath::Target),
        Some(PropertyObservation::Known(value)) if format!("{value:?}").contains("REDACTED")
    ));
    let change = plan
        .changes()
        .iter()
        .find(|change| change.address() == &address("schedule.nightly"))
        .unwrap_or_else(|| panic!("a replacement is planned: {:?}", plan.diagnostics()));
    assert_eq!(change.kind(), ChangeKind::Replace);
    assert!(plan.applyable());
}
