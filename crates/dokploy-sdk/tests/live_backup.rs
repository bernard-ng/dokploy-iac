use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use dokploy_sdk::{
    BackupId, BackupTarget, CreateBackup, DestinationId, Dokploy, PostgresId, UpdateBackup,
};

fn required_environment(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live Backup test"))
}

#[tokio::test]
#[ignore = "mutates one disposable disabled Backup on local Dokploy"]
async fn live_backup_adapter_converges_without_execution_or_deployment() {
    assert_eq!(
        std::env::var("DOKPLOY_BACKUP_LIVE_TEST").as_deref(),
        Ok("1"),
        "run this test through scripts/integration/test-backup-sdk.sh"
    );
    let client = Dokploy::builder()
        .url(required_environment("DOKPLOY_URL"))
        .api_key(required_environment("DOKPLOY_API_KEY"))
        .build()
        .expect("live client configuration is valid");
    let target = BackupTarget::Postgres(PostgresId::new(required_environment(
        "DOKPLOY_BACKUP_POSTGRES_ID",
    )));
    let first_destination =
        DestinationId::new(required_environment("DOKPLOY_BACKUP_DESTINATION_ID"));
    let second_destination = DestinationId::new(required_environment(
        "DOKPLOY_BACKUP_UPDATED_DESTINATION_ID",
    ));

    let before = client
        .backups()
        .by_target(target.clone())
        .await
        .expect("preflight Backup collection is readable");
    assert!(before.backups().is_empty());

    let created = client
        .backups()
        .create(CreateBackup::new(
            target.clone(),
            first_destination.clone(),
            "* * * * *",
            false,
            "/live-created/",
            "postgres",
            Some(NonZeroU32::new(2).unwrap()),
            true,
        ))
        .await
        .expect("live disabled Backup creation succeeds");
    let backup_id = created.backup_id().clone();

    let details = client
        .backups()
        .get(backup_id.clone())
        .await
        .expect("created Backup agrees with its target collection");
    assert_eq!(details.target, target);
    assert_eq!(details.destination_id, first_destination);
    assert_eq!(details.enabled, Some(false));
    assert_eq!(details.prefix, "/live-created/");
    assert_eq!(details.database, "postgres");
    assert_eq!(details.keep_latest_count, NonZeroU32::new(2));
    assert!(details.include_encryption_key);

    client
        .backups()
        .update(UpdateBackup::new(
            backup_id.clone(),
            target.clone(),
            second_destination.clone(),
            "*/1 * * * *",
            false,
            "/live-updated/",
            "postgres-updated",
            Some(NonZeroU32::new(3).unwrap()),
            false,
        ))
        .await
        .expect("live complete Backup update succeeds");
    let updated = client
        .backups()
        .get(backup_id.clone())
        .await
        .expect("updated Backup agrees with its target collection");
    assert_eq!(updated.target, target);
    assert_eq!(updated.destination_id, second_destination);
    assert_eq!(updated.schedule, "*/1 * * * *");
    assert_eq!(updated.enabled, Some(false));
    assert_eq!(updated.prefix, "/live-updated/");
    assert_eq!(updated.database, "postgres-updated");
    assert_eq!(updated.keep_latest_count, NonZeroU32::new(3));
    assert!(!updated.include_encryption_key);

    let handshake = PathBuf::from(required_environment("DOKPLOY_BACKUP_HANDSHAKE_DIR"));
    std::fs::create_dir(handshake.join(format!("ready-{}", backup_id.as_str())))
        .expect("the live wrapper can inspect the disabled Backup");
    for _ in 0..300 {
        if handshake.join("continue").is_dir() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        handshake.join("continue").is_dir(),
        "the live wrapper did not release Backup cleanup"
    );

    client
        .backups()
        .delete(backup_id.clone(), target.clone())
        .await
        .expect("live Backup deletion succeeds");
    let removed = client
        .backups()
        .get(BackupId::new(backup_id.as_str()))
        .await
        .expect_err("removed Backup must not be directly readable");
    assert!(removed.dokploy().is_some());
    let after = client
        .backups()
        .by_target(target)
        .await
        .expect("cleanup target collection is readable");
    assert!(after.backups().is_empty());
}
