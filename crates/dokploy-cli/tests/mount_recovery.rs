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
    ENVIRONMENT, ENVIRONMENTS, Router, application_one, application_search, file_mount, list, ok,
    project_topology, status, volume_mount,
};

const CONTENT_CANARY: &str = "recovery-mount-content-canary-never-leak";

fn address(value: &str) -> ResourceAddress {
    value.parse().unwrap()
}

fn routes(
    mount_routes: Vec<(&'static str, Vec<support::Reply>)>,
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
    routes.extend(mount_routes);
    routes
}

fn receipt(byte: u8) -> SensitiveInputs {
    let key_id = FingerprintKeyId::new(
        uuid::Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").unwrap(),
    )
    .unwrap();
    SensitiveInputs::try_from_entries([(
        SensitivePropertyPath::parse("content").unwrap(),
        SensitiveFingerprint::new_v1(key_id, [byte; 32]),
    )])
    .unwrap()
}

fn mount_state(id: &str, inputs: serde_json::Value, sensitive: SensitiveInputs) -> ResourceState {
    ResourceState::try_new(
        ResourceKind::Mount,
        RemoteId::new(id).unwrap(),
        false,
        ManagedInputs::try_from_json(inputs).unwrap(),
        sensitive,
        Some(address("environment.production")),
        vec![address("application.api")],
    )
    .unwrap()
}

/// Seeds project, environment, and application state, optionally with a Mount.
fn seed(store: &StateStore, instance: InstanceIdentity, mount: Option<ResourceState>) {
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
    if let Some(mount) = mount {
        entries.push((address("mount.data"), mount));
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
        .start_recoverable_step(address("mount.data"), action, expected)
        .unwrap();
}

fn volume_config(path: &str) -> String {
    format!(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api: {{}}\n      mounts:\n        data:\n          target: application.api\n          mount_path: {path}\n          source: {{ type: volume, volume_name: api-data }}\n"
    )
}

fn volume_inputs(path: &str) -> serde_json::Value {
    serde_json::json!({
        "target": "application.api",
        "mount_type": "volume",
        "mount_path": path,
        "volume_name": "api-data"
    })
}

fn pending_create(inputs: serde_json::Value, sensitive: SensitiveInputs) -> ExpectedCheckpoint {
    ExpectedCheckpoint::create(mount_state("recovery-pending", inputs, sensitive)).unwrap()
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

#[tokio::test]
async fn uncertain_create_adopts_one_exact_collision_key_without_retrying() {
    let mount = volume_mount("mount-1", "application-1", "/data", "api-data");
    let router = Router::start(routes(vec![(
        "GET /api/mounts.listByServiceId",
        vec![ok(list([mount]))],
    )]));
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(&config_file, volume_config("/data")).unwrap();
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    seed(&store, instance, None);
    open_step(
        &store,
        JournalAction::Create,
        pending_create(volume_inputs("/data"), SensitiveInputs::default()),
    );

    let result = recover_workspace_with_approval(&router.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&address("mount.data")));
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    let state = store.inspect().unwrap().unwrap();
    let adopted = state.resource(&address("mount.data")).unwrap();
    assert_eq!(adopted.remote_id().as_str(), "mount-1");
    assert_eq!(adopted.last_applied().as_json(), &volume_inputs("/data"));
    assert_eq!(adopted.dependencies(), &[address("application.api")]);
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    no_mutation(&router);
}

#[tokio::test]
async fn uncertain_file_create_adopts_without_resending_or_exposing_content() {
    let mount = file_mount(
        "mount-1",
        "application-1",
        "/etc/settings.conf",
        "settings.conf",
    );
    let router = Router::start(routes(vec![(
        "GET /api/mounts.listByServiceId",
        vec![ok(list([mount]))],
    )]));
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(workspace.path().join("content"), CONTENT_CANARY).unwrap();
    fs::write(
        &config_file,
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api: {}\n      mounts:\n        data:\n          target: application.api\n          mount_path: /etc/settings.conf\n          source:\n            type: file\n            file_path: settings.conf\n            content: { file: content }\n",
    )
    .unwrap();
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    seed(&store, instance, None);
    open_step(
        &store,
        JournalAction::Create,
        pending_create(
            serde_json::json!({
                "target": "application.api",
                "mount_type": "file",
                "mount_path": "/etc/settings.conf",
                "file_path": "settings.conf"
            }),
            receipt(1),
        ),
    );

    let result = recover_workspace_with_approval(&router.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    no_mutation(&router);
    let state_text = fs::read_to_string(workspace.path().join(".dokploy/state.json")).unwrap();
    assert!(!state_text.contains(CONTENT_CANARY));
    assert!(!state_text.contains("remote-content-is-never-modeled"));
}

#[tokio::test]
async fn uncertain_create_with_authoritative_absence_confirms_no_change() {
    let router = Router::start(routes(vec![(
        "GET /api/mounts.listByServiceId",
        vec![ok("[]")],
    )]));
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(&config_file, volume_config("/data")).unwrap();
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    seed(&store, instance, None);
    open_step(
        &store,
        JournalAction::Create,
        pending_create(volume_inputs("/data"), SensitiveInputs::default()),
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
            .resource(&address("mount.data"))
            .is_none()
    );
    no_mutation(&router);
}

#[tokio::test]
async fn uncertain_create_with_a_different_mount_at_the_key_needs_manual_intervention() {
    let other = volume_mount("mount-9", "application-1", "/data", "someone-elses-volume");
    let router = Router::start(routes(vec![(
        "GET /api/mounts.listByServiceId",
        vec![ok(list([other]))],
    )]));
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(&config_file, volume_config("/data")).unwrap();
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    seed(&store, instance, None);
    open_step(
        &store,
        JournalAction::Create,
        pending_create(volume_inputs("/data"), SensitiveInputs::default()),
    );

    let error = recover_workspace_with_approval(&router.client(), &config_file, |_| Ok(true))
        .await
        .expect_err("a non-matching resource is never adopted");

    assert!(matches!(error, RecoverWorkspaceError::ManualIntervention));
    assert!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&address("mount.data"))
            .is_none()
    );
    assert_ne!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
}

#[tokio::test]
async fn uncertain_update_is_confirmed_only_from_authoritative_complete_state() {
    let updated = volume_mount("mount-1", "application-1", "/updated", "api-data");
    let router = Router::start(routes(vec![
        (
            "GET /api/mounts.listByServiceId",
            vec![ok(list([&updated]))],
        ),
        ("GET /api/mounts.one", vec![ok(&updated)]),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(&config_file, volume_config("/updated")).unwrap();
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    let before = mount_state(
        "mount-1",
        volume_inputs("/data"),
        SensitiveInputs::default(),
    );
    seed(&store, instance, Some(before.clone()));
    open_step(
        &store,
        JournalAction::Update,
        ExpectedCheckpoint::update(
            before,
            mount_state(
                "mount-1",
                volume_inputs("/updated"),
                SensitiveInputs::default(),
            ),
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
            .resource(&address("mount.data"))
            .unwrap()
            .last_applied()
            .as_json(),
        &volume_inputs("/updated")
    );
    no_mutation(&router);
}

#[tokio::test]
async fn uncertain_update_that_did_not_apply_confirms_no_change() {
    let original = volume_mount("mount-1", "application-1", "/data", "api-data");
    let router = Router::start(routes(vec![
        (
            "GET /api/mounts.listByServiceId",
            vec![ok(list([&original]))],
        ),
        ("GET /api/mounts.one", vec![ok(&original)]),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(&config_file, volume_config("/updated")).unwrap();
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    let before = mount_state(
        "mount-1",
        volume_inputs("/data"),
        SensitiveInputs::default(),
    );
    seed(&store, instance, Some(before.clone()));
    open_step(
        &store,
        JournalAction::Update,
        ExpectedCheckpoint::update(
            before,
            mount_state(
                "mount-1",
                volume_inputs("/updated"),
                SensitiveInputs::default(),
            ),
        )
        .unwrap(),
    );

    let result = recover_workspace_with_approval(&router.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::ConfirmNoChange);
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
            .resource(&address("mount.data"))
            .unwrap()
            .last_applied()
            .as_json(),
        &volume_inputs("/data")
    );
}

#[tokio::test]
async fn uncertain_content_rotation_requires_manual_intervention() {
    let mount = file_mount(
        "mount-1",
        "application-1",
        "/etc/settings.conf",
        "settings.conf",
    );
    let router = Router::start(routes(vec![
        ("GET /api/mounts.listByServiceId", vec![ok(list([&mount]))]),
        ("GET /api/mounts.one", vec![ok(&mount)]),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(workspace.path().join("content"), CONTENT_CANARY).unwrap();
    fs::write(
        &config_file,
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api: {}\n      mounts:\n        data:\n          target: application.api\n          mount_path: /etc/settings.conf\n          source:\n            type: file\n            file_path: settings.conf\n            content: { file: content }\n",
    )
    .unwrap();
    let inputs = serde_json::json!({
        "target": "application.api",
        "mount_type": "file",
        "mount_path": "/etc/settings.conf",
        "file_path": "settings.conf"
    });
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    let before = mount_state("mount-1", inputs.clone(), receipt(1));
    seed(&store, instance, Some(before.clone()));
    open_step(
        &store,
        JournalAction::Update,
        ExpectedCheckpoint::update(before, mount_state("mount-1", inputs, receipt(2))).unwrap(),
    );

    let error = recover_workspace_with_approval(&router.client(), &config_file, |_| Ok(true))
        .await
        .expect_err("write-only content cannot be proven applied");

    assert!(matches!(error, RecoverWorkspaceError::ManualIntervention));
    assert!(!format!("{error:?}{error}").contains(CONTENT_CANARY));
    assert_ne!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
}

#[tokio::test]
async fn uncertain_delete_is_confirmed_by_authoritative_absence() {
    let router = Router::start(routes(vec![
        ("GET /api/mounts.listByServiceId", vec![ok("[]")]),
        (
            "GET /api/mounts.one",
            vec![status("404 Not Found", r#"{"message":"Mount not found"}"#)],
        ),
    ]));
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api: {}\n",
    )
    .unwrap();
    let instance = InstanceIdentity::parse(&router.url).unwrap();
    let store = StateStore::new(workspace.path(), instance.clone()).unwrap();
    let before = mount_state(
        "mount-1",
        volume_inputs("/data"),
        SensitiveInputs::default(),
    );
    seed(&store, instance, Some(before.clone()));
    open_step(
        &store,
        JournalAction::Delete,
        ExpectedCheckpoint::remove(before),
    );

    let result = recover_workspace_with_approval(&router.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::CheckpointConfirmedSuccess);
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
            .resource(&address("mount.data"))
            .is_none()
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    no_mutation(&router);
}
