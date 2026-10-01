mod support;

use dokploy_cli::desired::compile_desired;
use dokploy_cli::remote::{
    BackupTopologyAuthority, DiscoverRemoteError, DiscoveryAuthority, MountTopologyAuthority,
    discover_remote,
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
    ENVIRONMENT, ENVIRONMENTS, Reply, Router, application_one, application_search, bind_mount,
    list, ok, project_topology, status, volume_mount,
};

fn address(value: &str) -> ResourceAddress {
    value.parse().unwrap()
}

fn insert(
    state: &mut StateFile,
    name: &str,
    kind: ResourceKind,
    id: &str,
    containment: Option<&str>,
    dependencies: &[&str],
    inputs: serde_json::Value,
) {
    state
        .upsert_resource(
            address(name),
            ResourceState::new(
                kind,
                RemoteId::new(id).unwrap(),
                false,
                ManagedInputs::try_from_json(inputs).unwrap(),
                containment.map(address),
                dependencies.iter().map(|value| address(value)).collect(),
            ),
        )
        .unwrap();
}

fn state(router: &Router, with_mount: bool) -> StateFile {
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
    );
    insert(
        &mut state,
        "environment.production",
        ResourceKind::Environment,
        "environment-1",
        Some("project.platform"),
        &[],
        serde_json::json!({}),
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
        );
    }
    if with_mount {
        insert(
            &mut state,
            "mount.data",
            ResourceKind::Mount,
            "mount-1",
            Some("environment.production"),
            &["application.api"],
            serde_json::json!({
                "target": "application.api",
                "mount_type": "volume",
                "mount_path": "/data",
                "volume_name": "api-data"
            }),
        );
    }
    state
}

fn desired(target: &str, path: &str, volume: &str) -> dokploy_cli::desired::CompiledDesired {
    compile_desired(
        &DokployConfig::parse(&format!(
            "version: 1\nproject: {{ name: platform }}\nenvironments:\n  production:\n    applications:\n      api: {{}}\n      worker: {{}}\n    mounts:\n      data:\n        target: {target}\n        mount_path: {path}\n        source: {{ type: volume, volume_name: {volume} }}\n"
        ))
        .unwrap(),
        ConfigDigest::parse("a".repeat(64)).unwrap(),
    )
    .unwrap()
}

fn topology_routes(
    mount_routes: Vec<(&'static str, Vec<Reply>)>,
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
    routes.extend(mount_routes);
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

fn mount_observation(remote: &dokploy_core::RemoteState) -> Option<&RemoteObservation> {
    remote.observation(&address("mount.data"))
}

#[tokio::test]
async fn authoritative_target_collection_absence_plans_a_create() {
    let router = Router::start(topology_routes(vec![(
        "GET /api/mounts.listByServiceId|application-1",
        vec![ok("[]")],
    )]));
    let state = state(&router, false);
    let desired = desired("application.api", "/data", "api-data");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(matches!(
        mount_observation(&remote),
        Some(RemoteObservation::Missing)
    ));
    assert!(plan.applyable());
    assert!(
        plan.changes()
            .iter()
            .any(|change| change.address() == &address("mount.data")
                && change.kind() == ChangeKind::Create)
    );
    assert_eq!(router.matching("GET /api/mounts.listByServiceId").len(), 1);
    assert!(router.matching("GET /api/mounts.one").is_empty());
    assert!(router.unrouted().is_empty());
}

#[tokio::test]
async fn partial_authority_downgrades_absence_to_an_unavailable_observation() {
    let router = Router::start(topology_routes(vec![(
        "GET /api/mounts.listByServiceId|application-1",
        vec![ok("[]")],
    )]));
    let state = state(&router, false);
    let desired = desired("application.api", "/data", "api-data");
    let authority = DiscoveryAuthority {
        mounts: MountTopologyAuthority::Partial,
        backups: BackupTopologyAuthority::Partial,
        ..DiscoveryAuthority::reconciliation()
    };

    let remote = observe(&router, &state, &desired, authority).await.unwrap();
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(matches!(
        mount_observation(&remote),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse
        ))
    ));
    assert!(!plan.complete());
}

#[tokio::test]
async fn managed_mount_requires_direct_and_collection_agreement() {
    let mount = volume_mount("mount-1", "application-1", "/data", "api-data");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([&mount]))],
        ),
        ("GET /api/mounts.one", vec![ok(&mount)]),
    ]));
    let state = state(&router, true);
    let desired = desired("application.api", "/data", "api-data");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    let Some(RemoteObservation::Present(resource)) = mount_observation(&remote) else {
        panic!("Mount is present");
    };
    for path in [
        PropertyPath::Target,
        PropertyPath::MountType,
        PropertyPath::MountPath,
        PropertyPath::VolumeName,
    ] {
        assert!(matches!(
            resource.property(&path),
            Some(PropertyObservation::Known(_))
        ));
    }
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );
    assert!(plan.changes().is_empty() && plan.applyable());
}

#[tokio::test]
async fn direct_and_collection_disagreement_fails_closed() {
    let direct = volume_mount("mount-1", "application-1", "/data", "api-data");
    let listed = volume_mount("mount-1", "application-1", "/other", "api-data");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([listed]))],
        ),
        ("GET /api/mounts.one", vec![ok(&direct)]),
    ]));
    let state = state(&router, true);

    let error = observe(
        &router,
        &state,
        &desired("application.api", "/data", "api-data"),
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("contradictory reads fail closed");

    assert!(matches!(error, DiscoverRemoteError::MountTopologyConflict));
}

#[tokio::test]
async fn a_direct_read_on_another_target_fails_closed() {
    let elsewhere = volume_mount("mount-1", "application-2", "/data", "api-data");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok("[]")],
        ),
        ("GET /api/mounts.one", vec![ok(&elsewhere)]),
    ]));
    let state = state(&router, true);

    let error = observe(
        &router,
        &state,
        &desired("application.api", "/data", "api-data"),
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("a mount attached to a different target is never accepted");

    assert!(matches!(error, DiscoverRemoteError::MountTopologyConflict));
}

#[tokio::test]
async fn direct_404_proves_absence_only_from_an_authoritative_collection() {
    let routes = || {
        topology_routes(vec![
            (
                "GET /api/mounts.listByServiceId|application-1",
                vec![ok("[]")],
            ),
            (
                "GET /api/mounts.one",
                vec![status("404 Not Found", r#"{"message":"Mount not found"}"#)],
            ),
        ])
    };
    let router = Router::start(routes());
    let state_value = state(&router, true);
    let desired_value = desired("application.api", "/data", "api-data");
    let remote = observe(
        &router,
        &state_value,
        &desired_value,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    assert!(matches!(
        mount_observation(&remote),
        Some(RemoteObservation::Missing)
    ));
    let plan_value = plan(
        desired_value.desired_state(),
        &StoredState::try_from_state(&state_value).unwrap(),
        &remote,
    );
    assert!(
        plan_value
            .changes()
            .iter()
            .any(|change| change.address() == &address("mount.data")
                && change.kind() == ChangeKind::Create)
    );

    let partial = Router::start(routes());
    let state_value = state(&partial, true);
    let remote = observe(
        &partial,
        &state_value,
        &desired_value,
        DiscoveryAuthority {
            mounts: MountTopologyAuthority::Partial,
            backups: BackupTopologyAuthority::Partial,
            ..DiscoveryAuthority::reconciliation()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        mount_observation(&remote),
        Some(RemoteObservation::Unavailable(_))
    ));

    let contradicted = Router::start(topology_routes(vec![
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([volume_mount(
                "mount-1",
                "application-1",
                "/data",
                "api-data",
            )]))],
        ),
        (
            "GET /api/mounts.one",
            vec![status("404 Not Found", r#"{"message":"Mount not found"}"#)],
        ),
    ]));
    let state_value = state(&contradicted, true);
    let remote = observe(
        &contradicted,
        &state_value,
        &desired_value,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            mount_observation(&remote),
            Some(RemoteObservation::Unavailable(_))
        ),
        "a 404 contradicted by the collection is never an absence proof"
    );
}

#[tokio::test]
async fn failing_collection_makes_the_observation_unavailable() {
    let mount = volume_mount("mount-1", "application-1", "/data", "api-data");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![status("500 Internal Server Error", r#"{"message":"down"}"#)],
        ),
        ("GET /api/mounts.one", vec![ok(&mount)]),
    ]));
    let state = state(&router, true);
    let desired = desired("application.api", "/data", "api-data");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(matches!(
        mount_observation(&remote),
        Some(RemoteObservation::Unavailable(
            RemoteFailureKind::Unavailable
        ))
    ));
    assert!(!plan.complete() && plan.changes().is_empty());
}

#[tokio::test]
async fn duplicate_target_scoped_paths_fail_closed() {
    let first = volume_mount("mount-1", "application-1", "/data", "one");
    let second = volume_mount("mount-2", "application-1", "/data", "two");
    let router = Router::start(topology_routes(vec![(
        "GET /api/mounts.listByServiceId|application-1",
        vec![ok(list([first, second]))],
    )]));
    let state = state(&router, false);

    let error = observe(
        &router,
        &state,
        &desired("application.api", "/data", "api-data"),
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("two mounts cannot share one target and path");

    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateMountCollision
    ));
}

#[tokio::test]
async fn an_unmanaged_mount_at_the_collision_key_blocks_instead_of_being_adopted() {
    let existing = bind_mount("mount-9", "application-1", "/data", "/srv/data");
    let router = Router::start(topology_routes(vec![(
        "GET /api/mounts.listByServiceId|application-1",
        vec![ok(list([existing]))],
    )]));
    let state = state(&router, false);
    let desired = desired("application.api", "/data", "api-data");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan = plan(
        desired.desired_state(),
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(matches!(
        mount_observation(&remote),
        Some(RemoteObservation::Present(_))
    ));
    assert!(!plan.applyable());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|issue| issue.code() == PlanDiagnosticCode::UnmanagedAddressCollision)
    );
}

#[tokio::test]
async fn retargeting_plans_a_replacement_and_blocks_on_a_collision_at_the_new_target() {
    let mount = volume_mount("mount-1", "application-1", "/data", "api-data");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([&mount]))],
        ),
        (
            "GET /api/mounts.listByServiceId|application-2",
            vec![ok("[]")],
        ),
        ("GET /api/mounts.one", vec![ok(&mount)]),
    ]));
    let state_value = state(&router, true);
    let desired_value = desired("application.worker", "/data", "api-data");
    let remote = observe(
        &router,
        &state_value,
        &desired_value,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let plan_value = plan(
        desired_value.desired_state(),
        &StoredState::try_from_state(&state_value).unwrap(),
        &remote,
    );
    let change = plan_value
        .changes()
        .iter()
        .find(|change| change.address() == &address("mount.data"))
        .unwrap();
    assert_eq!(change.kind(), ChangeKind::Replace);

    let occupied = volume_mount("mount-7", "application-2", "/data", "other");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([&mount]))],
        ),
        (
            "GET /api/mounts.listByServiceId|application-2",
            vec![ok(list([occupied]))],
        ),
        ("GET /api/mounts.one", vec![ok(&mount)]),
    ]));
    let state_value = state(&router, true);
    let error = observe(
        &router,
        &state_value,
        &desired_value,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("a replacement must not land on an occupied collision key");
    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateMountCollision
    ));
}

#[tokio::test]
async fn a_path_edit_that_collides_with_a_sibling_fails_closed() {
    let managed = volume_mount("mount-1", "application-1", "/data", "api-data");
    let sibling = volume_mount("mount-3", "application-1", "/updated", "other");
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list(&[managed.clone(), sibling]))],
        ),
        ("GET /api/mounts.one", vec![ok(&managed)]),
    ]));
    let state = state(&router, true);

    let error = observe(
        &router,
        &state,
        &desired("application.api", "/updated", "api-data"),
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("an in-place path edit must not duplicate a sibling path");

    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateMountCollision
    ));
}

#[tokio::test]
async fn a_missing_target_makes_the_mount_missing_without_a_collection_read() {
    let router = Router::start(vec![("GET /api/project.all", vec![ok("[]")])]);
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
    );
    let desired = desired("application.api", "/data", "api-data");

    let remote = observe(
        &router,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        mount_observation(&remote),
        Some(RemoteObservation::Missing)
    ));
    assert!(router.matching("GET /api/mounts.").is_empty());
}

#[tokio::test]
async fn compose_targets_use_the_typed_service_type_for_the_collection() {
    let router = Router::start(topology_routes(vec![
        (
            "GET /api/compose.search",
            vec![ok(
                r#"{"items":[{"composeId":"compose-1","environmentId":"environment-1","name":"stack"}],"total":1}"#,
            )],
        ),
        (
            "GET /api/compose.one",
            vec![ok(
                r#"{"composeId":"compose-1","environmentId":"environment-1","name":"stack","appName":"stack","sourceType":"raw"}"#,
            )],
        ),
        (
            "GET /api/mounts.listByServiceId|serviceType=compose",
            vec![ok("[]")],
        ),
    ]));
    let mut state = state(&router, false);
    insert(
        &mut state,
        "compose.stack",
        ResourceKind::Compose,
        "compose-1",
        Some("environment.production"),
        &[],
        serde_json::json!({}),
    );
    let desired = compile_desired(
        &DokployConfig::parse(
            "version: 1\nproject: { name: platform }\nenvironments:\n  production:\n    compose:\n      stack:\n        lifecycle: { protect: true }\n    mounts:\n      data:\n        target: compose.stack\n        mount_path: /data\n        source: { type: volume, volume_name: v }\n",
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

    assert!(matches!(
        mount_observation(&remote),
        Some(RemoteObservation::Missing)
    ));
    let lists = router.matching("GET /api/mounts.listByServiceId");
    assert_eq!(lists.len(), 1);
    assert!(lists[0].contains("serviceId=compose-1"));
}
