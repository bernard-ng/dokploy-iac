mod support;

use std::fs;
use std::path::Path;

use dokploy_cli::executor::{
    ApplyWorkspaceError, apply_workspace, destroy_workspace_with_approval,
};
use dokploy_state::{
    FailureCode, InstanceIdentity, RecoveryStatus, ResourceAddress, StateFile, StateStore,
};
use support::{
    APPLICATION_CREATE, ENVIRONMENT, ENVIRONMENTS, PROJECT_CREATE, Reply, Router, application_one,
    application_search, bind_mount, file_mount, list, ok, project_topology, status, volume_mount,
};

const CONTENT_CANARY: &str = "mount-file-content-canary-never-leak";
const ROTATED_CANARY: &str = "rotated-mount-content-canary-never-leak";

fn address(value: &str) -> ResourceAddress {
    value.parse().unwrap()
}

fn config(mounts: &str) -> String {
    format!(
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      api: {{}}\n      worker: {{}}\n    mounts:\n{mounts}"
    )
}

const VOLUME: &str = "      data:\n        target: application.api\n        mount_path: /data\n        source: { type: volume, volume_name: api-data }\n";

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

/// Routes that serve the initial empty instance and an instance holding the
/// application(s) created by the first apply.
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

fn assert_no_deployment_or_unrouted(router: &Router) {
    assert!(router.unrouted().is_empty(), "{:?}", router.unrouted());
    assert!(
        router
            .requests()
            .iter()
            .all(|request| !request.contains(".deploy") && !request.contains(".redeploy"))
    );
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

#[tokio::test]
async fn create_checkpoints_the_target_dependency_and_the_next_apply_is_a_no_op() {
    let mount = volume_mount("mount-1", "application-1", "/data", "api-data");
    let router = Router::start(base_routes(vec![
        ("POST /api/mounts.create", vec![ok(&mount)]),
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
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(VOLUME)).unwrap();
    let client = router.client();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(
        summary.applied(),
        5,
        "project, environment, two applications, Mount"
    );
    let create = router.matching("POST /api/mounts.create");
    assert_eq!(create.len(), 1);
    for expected in [
        r#""type":"volume""#,
        r#""volumeName":"api-data""#,
        r#""mountPath":"/data""#,
        r#""serviceType":"application""#,
        r#""serviceId":"application-1""#,
    ] {
        assert!(create[0].contains(expected), "{expected}");
    }
    let state = state(&router, directory.path());
    let stored = state.resource(&address("mount.data")).unwrap();
    assert_eq!(stored.remote_id().as_str(), "mount-1");
    assert_eq!(
        stored.last_applied().as_json(),
        &serde_json::json!({
            "target": "application.api",
            "mount_type": "volume",
            "mount_path": "/data",
            "volume_name": "api-data"
        })
    );
    assert_eq!(stored.dependencies(), &[address("application.api")]);
    assert_eq!(
        stored.containment(),
        Some(&address("environment.production"))
    );

    let converged = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(converged.applied(), 0);
    assert_eq!(router.matching("POST /api/mounts.create").len(), 1);
    assert_eq!(
        store(&router, directory.path()).recovery_status().unwrap(),
        RecoveryStatus::Clean
    );
    assert_no_deployment_or_unrouted(&router);
}

#[tokio::test]
async fn path_edit_updates_in_place_after_a_fresh_identity_checked_read() {
    let before = volume_mount("mount-1", "application-1", "/data", "api-data");
    let after = volume_mount("mount-1", "application-1", "/updated", "api-data");
    let router = Router::start(base_routes(vec![
        ("POST /api/mounts.create", vec![ok(&before)]),
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([&before])), ok(list([&after]))],
        ),
        (
            "GET /api/mounts.listByServiceId|application-2",
            vec![ok("[]")],
        ),
        (
            "GET /api/mounts.one",
            vec![ok(&before), ok(&before), ok(&after)],
        ),
        ("POST /api/mounts.update", vec![ok(r#"{"ok":true}"#)]),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(VOLUME)).unwrap();
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(&config_file, config(&VOLUME.replace("/data", "/updated"))).unwrap();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    let lines = router.lines();
    let one = lines
        .iter()
        .rposition(|line| line.starts_with("GET /api/mounts.one"))
        .unwrap();
    let update = lines
        .iter()
        .position(|line| line.starts_with("POST /api/mounts.update"))
        .unwrap();
    assert!(one < update, "the update is preceded by a fresh read");
    let body = &router.matching("POST /api/mounts.update")[0];
    assert!(body.contains(r#""mountId":"mount-1""#));
    assert!(body.contains(r#""mountPath":"/updated""#));
    assert!(body.contains(r#""volumeName":"api-data""#));
    assert_eq!(router.matching("POST /api/mounts.create").len(), 1);
    assert!(router.matching("POST /api/mounts.remove").is_empty());
    assert_eq!(
        state(&router, directory.path())
            .resource(&address("mount.data"))
            .unwrap()
            .last_applied()
            .as_json()["mount_path"],
        "/updated"
    );
    assert_no_deployment_or_unrouted(&router);
}

#[tokio::test]
async fn target_change_deletes_before_creating_under_the_new_target() {
    let api = volume_mount("mount-1", "application-1", "/data", "api-data");
    let worker = volume_mount("mount-2", "application-2", "/data", "api-data");
    let router = Router::start(base_routes(vec![
        ("POST /api/mounts.create", vec![ok(&api), ok(&worker)]),
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([&api]))],
        ),
        (
            "GET /api/mounts.listByServiceId|application-2",
            vec![ok("[]")],
        ),
        ("GET /api/mounts.one", vec![ok(&api)]),
        ("POST /api/mounts.remove", vec![ok(r#"{"ok":true}"#)]),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(VOLUME)).unwrap();
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        config(&VOLUME.replace("application.api", "application.worker")),
    )
    .unwrap();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    let lines = router.lines();
    let remove = lines
        .iter()
        .position(|line| line.starts_with("POST /api/mounts.remove"))
        .unwrap();
    let create = lines
        .iter()
        .rposition(|line| line.starts_with("POST /api/mounts.create"))
        .unwrap();
    assert!(remove < create);
    assert!(router.matching("POST /api/mounts.remove")[0].contains(r#""mountId":"mount-1""#));
    assert!(
        router.matching("POST /api/mounts.create")[1].contains(r#""serviceId":"application-2""#)
    );
    let state = state(&router, directory.path());
    let stored = state.resource(&address("mount.data")).unwrap();
    assert_eq!(stored.remote_id().as_str(), "mount-2");
    assert_eq!(stored.dependencies(), &[address("application.worker")]);
    assert_no_deployment_or_unrouted(&router);
}

#[tokio::test]
async fn storage_type_change_is_a_replacement_not_an_in_place_update() {
    let volume = volume_mount("mount-1", "application-1", "/data", "api-data");
    let bind = bind_mount("mount-2", "application-1", "/data", "/srv/api");
    let router = Router::start(base_routes(vec![
        ("POST /api/mounts.create", vec![ok(&volume), ok(&bind)]),
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([&volume]))],
        ),
        (
            "GET /api/mounts.listByServiceId|application-2",
            vec![ok("[]")],
        ),
        ("GET /api/mounts.one", vec![ok(&volume)]),
        ("POST /api/mounts.remove", vec![ok(r#"{"ok":true}"#)]),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(VOLUME)).unwrap();
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        config("      data:\n        target: application.api\n        mount_path: /data\n        source: { type: bind, host_path: /srv/api }\n"),
    )
    .unwrap();

    apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(router.matching("POST /api/mounts.remove").len(), 1);
    assert!(router.matching("POST /api/mounts.update").is_empty());
    assert!(router.matching("POST /api/mounts.create")[1].contains(r#""hostPath":"/srv/api""#));
    assert_no_deployment_or_unrouted(&router);
}

#[tokio::test]
async fn removing_a_mount_and_its_target_deletes_the_mount_first() {
    let mount = volume_mount("mount-1", "application-1", "/data", "api-data");
    let router = Router::start(base_routes(vec![
        ("POST /api/mounts.create", vec![ok(&mount)]),
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([&mount]))],
        ),
        (
            "GET /api/mounts.listByServiceId|application-2",
            vec![ok("[]")],
        ),
        ("GET /api/mounts.one", vec![ok(&mount)]),
        ("POST /api/mounts.remove", vec![ok(r#"{"ok":true}"#)]),
        ("POST /api/application.delete", vec![ok(r#"{"ok":true}"#)]),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(VOLUME)).unwrap();
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
        .position(|line| line.starts_with("POST /api/mounts.remove"))
        .expect("the Mount is removed");
    let delete = lines
        .iter()
        .position(|line| line.starts_with("POST /api/application.delete"))
        .expect("the target is removed");
    assert!(remove < delete, "a Mount is removed before its target");
    assert!(
        state(&router, directory.path())
            .resource(&address("mount.data"))
            .is_none()
    );
}

#[tokio::test]
async fn destroy_removes_mounts_before_their_targets() {
    let mount = volume_mount("mount-1", "application-1", "/data", "api-data");
    let router = Router::start(base_routes(vec![
        ("POST /api/mounts.create", vec![ok(&mount)]),
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([&mount]))],
        ),
        (
            "GET /api/mounts.listByServiceId|application-2",
            vec![ok("[]")],
        ),
        ("GET /api/mounts.one", vec![ok(&mount)]),
        ("POST /api/mounts.remove", vec![ok(r#"{"ok":true}"#)]),
        ("POST /api/application.delete", vec![ok(r#"{"ok":true}"#)]),
        ("POST /api/project.remove", vec![ok(r#"{"ok":true}"#)]),
        ("POST /api/environment.remove", vec![ok(r#"{"ok":true}"#)]),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(VOLUME)).unwrap();
    let client = router.client();
    apply_workspace(&client, &config_file).await.unwrap();

    destroy_workspace_with_approval(&client, &config_file, |plan| {
        let order = plan
            .changes()
            .iter()
            .map(|change| change.address().to_string())
            .collect::<Vec<_>>();
        let position = |name: &str| order.iter().position(|item| item == name).unwrap();
        assert!(position("mount.data") < position("application.api"));
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(router.matching("POST /api/mounts.remove").len(), 1);
    assert_no_deployment_or_unrouted(&router);
}

fn file_config(secret: &str) -> String {
    config(&format!(
        "      settings:\n        target: application.api\n        mount_path: /etc/settings.conf\n        source:\n          type: file\n          file_path: settings.conf\n          content: {{ file: {secret} }}\n"
    ))
}

#[tokio::test]
async fn file_content_is_sent_once_and_never_persisted_then_rotates_with_an_update() {
    let mount = file_mount(
        "mount-1",
        "application-1",
        "/etc/settings.conf",
        "settings.conf",
    );
    let router = Router::start(base_routes(vec![
        ("POST /api/mounts.create", vec![ok(&mount)]),
        (
            "GET /api/mounts.listByServiceId|application-1",
            vec![ok(list([&mount]))],
        ),
        (
            "GET /api/mounts.listByServiceId|application-2",
            vec![ok("[]")],
        ),
        ("GET /api/mounts.one", vec![ok(&mount)]),
        ("POST /api/mounts.update", vec![ok(r#"{"ok":true}"#)]),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    let secret = directory.path().join("settings-content");
    fs::write(&secret, CONTENT_CANARY).unwrap();
    fs::write(&config_file, file_config("settings-content")).unwrap();
    let client = router.client();

    apply_workspace(&client, &config_file).await.unwrap();

    let create = &router.matching("POST /api/mounts.create")[0];
    assert!(create.contains(r#""type":"file""#));
    assert!(create.contains(r#""filePath":"settings.conf""#));
    assert!(
        create.contains(CONTENT_CANARY),
        "the content crosses only the mutation seam"
    );
    assert!(
        !scan_for(directory.path(), CONTENT_CANARY),
        "state and journal never hold content"
    );
    let stored = state(&router, directory.path());
    let settings = stored.resource(&address("mount.settings")).unwrap();
    assert_eq!(settings.sensitive_inputs().paths().count(), 1);
    assert!(
        !settings
            .last_applied()
            .as_json()
            .as_object()
            .unwrap()
            .contains_key("content")
    );

    assert_eq!(
        apply_workspace(&client, &config_file)
            .await
            .unwrap()
            .applied(),
        0
    );

    fs::write(&secret, ROTATED_CANARY).unwrap();
    let rotated = apply_workspace(&client, &config_file).await.unwrap();
    assert_eq!(rotated.applied(), 1);
    let update = &router.matching("POST /api/mounts.update")[0];
    assert!(update.contains(ROTATED_CANARY));
    assert!(update.contains(r#""filePath":"settings.conf""#));
    assert!(!scan_for(directory.path(), ROTATED_CANARY));
    assert!(!scan_for(directory.path(), CONTENT_CANARY));
    assert_no_deployment_or_unrouted(&router);
}

#[tokio::test]
async fn definitive_create_rejection_fails_the_step_without_retry_or_state() {
    let router = Router::start(base_routes(vec![
        (
            "POST /api/mounts.create",
            vec![status("400 Bad Request", r#"{"message":"invalid mount"}"#)],
        ),
        ("GET /api/mounts.listByServiceId", vec![ok("[]")]),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(VOLUME)).unwrap();

    let error = apply_workspace(&router.client(), &config_file)
        .await
        .expect_err("a definitive rejection fails the apply");

    assert!(matches!(
        error,
        ApplyWorkspaceError::RemoteMutation {
            code: FailureCode::Validation
        }
    ));
    assert_eq!(router.matching("POST /api/mounts.create").len(), 1);
    assert!(
        state(&router, directory.path())
            .resource(&address("mount.data"))
            .is_none()
    );
    assert_no_deployment_or_unrouted(&router);
}

#[tokio::test]
async fn outcome_unknown_create_stays_in_progress_and_is_never_retried() {
    let router = Router::start(base_routes(vec![
        ("POST /api/mounts.create", vec![Reply::Drop]),
        ("GET /api/mounts.listByServiceId", vec![ok("[]")]),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(VOLUME)).unwrap();

    let error = apply_workspace(&router.client(), &config_file)
        .await
        .expect_err("an unknown outcome stops the apply");

    assert!(matches!(
        error,
        ApplyWorkspaceError::RemoteMutation {
            code: FailureCode::TransportOutcomeUnknown
        }
    ));
    assert_eq!(router.matching("POST /api/mounts.create").len(), 1);
    assert_ne!(
        store(&router, directory.path()).recovery_status().unwrap(),
        RecoveryStatus::Clean,
        "the interrupted step remains recoverable"
    );
}

#[tokio::test]
async fn unmanaged_file_content_is_never_created_as_an_empty_file() {
    let router = Router::start(base_routes(vec![(
        "GET /api/mounts.listByServiceId",
        vec![ok("[]")],
    )]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        config("      settings:\n        target: application.api\n        mount_path: /etc/settings.conf\n        source: { type: file, file_path: settings.conf }\n        lifecycle: { protect: true }\n"),
    )
    .unwrap();

    let error = apply_workspace(&router.client(), &config_file)
        .await
        .expect_err("content cannot be invented");

    assert!(matches!(error, ApplyWorkspaceError::InvalidCheckpoint));
    assert!(router.matching("POST /api/mounts.create").is_empty());
    let RecoveryStatus::RecoveryRequired(summary) =
        store(&router, directory.path()).recovery_status().unwrap()
    else {
        panic!("earlier checkpointed steps leave the operation uncommitted");
    };
    assert!(
        summary
            .steps()
            .iter()
            .all(|step| step.address().kind() != dokploy_state::ResourceKind::Mount),
        "pre-mutation rejection opens no Mount journal step"
    );
}

#[tokio::test]
async fn compose_target_uses_the_typed_compose_service_identity() {
    let compose_mount = r#"{"mountId":"mount-1","type":"volume","mountPath":"/data","serviceType":"compose","composeId":"compose-1","volumeName":"stack-data","hostPath":null,"filePath":null}"#;
    let router = Router::start(base_routes(vec![
        (
            "POST /api/compose.create",
            vec![ok(
                r#"{"composeId":"compose-1","environmentId":"environment-1","name":"stack","appName":"stack","serverId":null}"#,
            )],
        ),
        ("POST /api/mounts.create", vec![ok(compose_mount)]),
        ("GET /api/mounts.listByServiceId", vec![ok("[]")]),
    ]));
    let directory = tempfile::tempdir().unwrap();
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(directory.path().join("compose.yaml"), "services: {}\n").unwrap();
    fs::write(
        &config_file,
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    compose:\n      stack:\n        document: { file: compose.yaml }\n    mounts:\n      data:\n        target: compose.stack\n        mount_path: /data\n        source: { type: volume, volume_name: stack-data }\n",
    )
    .unwrap();

    apply_workspace(&router.client(), &config_file)
        .await
        .unwrap();

    let create = &router.matching("POST /api/mounts.create")[0];
    assert!(create.contains(r#""serviceType":"compose""#));
    assert!(create.contains(r#""serviceId":"compose-1""#));
    let stored = state(&router, directory.path());
    assert_eq!(
        stored
            .resource(&address("mount.data"))
            .unwrap()
            .dependencies(),
        &[address("compose.stack")]
    );
    assert_no_deployment_or_unrouted(&router);
}
