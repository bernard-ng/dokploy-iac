use dokploy_sdk::{ApplicationId, CreateMount, Dokploy, MountId, ServiceTarget, UpdateMount};

fn required_environment(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live Mount test"))
}

#[tokio::test]
#[ignore = "mutates a disposable target on the pinned local Dokploy instance"]
async fn live_mount_adapter_converges_without_deploying_its_target() {
    assert_eq!(
        std::env::var("DOKPLOY_MOUNT_LIVE_TEST").as_deref(),
        Ok("1"),
        "run this test through scripts/integration/test-mount-sdk.sh"
    );
    let client = Dokploy::builder()
        .url(required_environment("DOKPLOY_URL"))
        .api_key(required_environment("DOKPLOY_API_KEY"))
        .build()
        .expect("live client configuration is valid");
    let target = ServiceTarget::Application(ApplicationId::new(required_environment(
        "DOKPLOY_MOUNT_APPLICATION_ID",
    )));
    let volume_name = required_environment("DOKPLOY_MOUNT_VOLUME_NAME");

    let before = client
        .mounts()
        .by_target(target.clone())
        .await
        .expect("preflight target list is readable");
    assert!(
        before.mounts().is_empty(),
        "disposable target must be empty"
    );

    let created = client
        .mounts()
        .create(CreateMount::volume(target.clone(), volume_name, "/data"))
        .await
        .expect("live volume Mount creation succeeds");
    let mount_id = created.mount_id().clone();

    let created_details = client
        .mounts()
        .get(mount_id.clone())
        .await
        .expect("created Mount is readable");
    assert_eq!(created_details.target, target);
    assert_eq!(created_details.mount_path, "/data");
    let created_list = client
        .mounts()
        .by_target(created_details.target.clone())
        .await
        .expect("created target list is readable");
    assert_eq!(created_list.mounts().len(), 1);
    assert_eq!(created_list.mounts()[0].mount_id, mount_id);

    client
        .mounts()
        .update(UpdateMount::new(mount_id.clone()).with_mount_path("/updated"))
        .await
        .expect("live Mount update succeeds");
    let updated = client
        .mounts()
        .get(mount_id.clone())
        .await
        .expect("updated Mount is readable");
    assert_eq!(updated.mount_path, "/updated");

    client
        .mounts()
        .delete(mount_id.clone())
        .await
        .expect("live Mount removal succeeds");
    let removed = client
        .mounts()
        .get(MountId::new(mount_id.as_str()))
        .await
        .expect_err("removed Mount must be absent");
    assert_eq!(removed.dokploy().map(|error| error.status()), Some(404));
    let after = client
        .mounts()
        .by_target(target)
        .await
        .expect("cleanup target list is readable");
    assert!(after.mounts().is_empty());
}
