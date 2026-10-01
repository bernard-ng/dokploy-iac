#[path = "support/backup.rs"]
mod fake;

use std::fs;
use std::path::Path;

use dokploy_cli::executor::{
    ApplyWorkspaceError, apply_workspace, destroy_workspace_with_approval,
};
use dokploy_state::{FailureCode, RecoveryStatus, StateFile, StateStore};
use fake::{Fake, Mode, NIGHTLY, SECRET_CANARY, address, backup_state, config, seed_workspace};

fn workspace(fake: &Fake, yaml: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(directory.path(), fake.instance(), Vec::new());
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, yaml).unwrap();
    (directory, config_file)
}

fn state(fake: &Fake, directory: &Path) -> StateFile {
    store(fake, directory)
        .inspect()
        .unwrap()
        .expect("state exists")
}

fn store(fake: &Fake, directory: &Path) -> StateStore {
    StateStore::new(directory, fake.instance()).unwrap()
}

fn durable_text(directory: &Path) -> String {
    fn walk(path: &Path, output: &mut String) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                walk(&entry.unwrap().path(), output);
            }
        } else if let Ok(bytes) = fs::read(path) {
            output.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    let mut output = String::new();
    walk(&directory.join(".dokploy"), &mut output);
    output
}

fn assert_no_external_material(text: &str) {
    for needle in [SECRET_CANARY, "destination-1", "destination-2"] {
        assert!(!text.contains(needle), "{needle}");
    }
}

#[tokio::test]
async fn create_sends_the_complete_disabled_policy_and_converges_to_a_no_op() {
    let fake = Fake::start();
    let (directory, config_file) = workspace(&fake, &config(NIGHTLY));
    let client = fake.client();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    let create = fake.matching("POST /api/backup.create");
    assert_eq!(create.len(), 1);
    for expected in [
        r#""schedule":"0 3 * * *""#,
        r#""enabled":false"#,
        r#""prefix":"nightly""#,
        r#""destinationId":"destination-1""#,
        r#""keepLatestCount":7"#,
        r#""database":"app""#,
        r#""databaseType":"postgres""#,
        r#""backupType":"database""#,
        r#""includeEncryptionKey":false"#,
        r#""postgresId":"postgres-1""#,
    ] {
        assert!(create[0].contains(expected), "{expected}");
    }
    let stored = state(&fake, directory.path());
    let backup = stored.resource(&address("backup.nightly")).unwrap();
    assert_eq!(backup.remote_id().as_str(), "backup-1");
    assert_eq!(
        backup.last_applied().as_json(),
        &serde_json::json!({
            "target": "postgres.main",
            "destination": { "name": "offsite" },
            "schedule": "0 3 * * *",
            "prefix": "nightly",
            "database": "app",
            "enabled": false,
            "keep_latest": 7,
            "include_encryption_key": false
        })
    );
    assert_eq!(backup.dependencies(), &[address("postgres.main")]);
    assert_eq!(
        backup.containment(),
        Some(&address("environment.production"))
    );

    let converged = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(converged.applied(), 0);
    assert_eq!(fake.count("POST /api/backup.create"), 1);
    assert_eq!(
        store(&fake, directory.path()).recovery_status().unwrap(),
        RecoveryStatus::Clean
    );
    assert_no_external_material(&durable_text(directory.path()));
    fake.assert_inert();
}

#[tokio::test]
async fn every_mutable_field_updates_in_place_with_a_complete_replacement() {
    let fake = Fake::start();
    let (directory, config_file) = workspace(&fake, &config(NIGHTLY));
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        config(
            "      nightly:\n        target: postgres.main\n        destination: { name: offsite }\n        schedule: \"5 4 * * 1\"\n        prefix: weekly\n        database: other\n        enabled: true\n        keep_latest: 30\n        include_encryption_key: true\n",
        ),
    )
    .unwrap();
    fake.forget_requests();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    let lines = fake.lines();
    let update = lines
        .iter()
        .position(|line| line == "POST /api/backup.update")
        .unwrap();
    let before_update = lines[..update]
        .iter()
        .rposition(|line| line == "GET /api/backup.one")
        .expect("a fresh identity-checked read precedes the update");
    assert!(
        lines[before_update..update]
            .iter()
            .all(|line| line.starts_with("GET /api/")),
        "only reads separate the fresh identity check from the update"
    );
    let body = &fake.matching("POST /api/backup.update")[0];
    for expected in [
        r#""backupId":"backup-1""#,
        r#""schedule":"5 4 * * 1""#,
        r#""prefix":"weekly""#,
        r#""database":"other""#,
        r#""enabled":true"#,
        r#""keepLatestCount":30"#,
        r#""includeEncryptionKey":true"#,
        r#""destinationId":"destination-1""#,
        r#""databaseType":"postgres""#,
    ] {
        assert!(body.contains(expected), "{expected}");
    }
    assert_eq!(fake.count("POST /api/backup.create"), 0);
    assert_eq!(fake.count("POST /api/backup.remove"), 0);
    let stored = state(&fake, directory.path());
    let backup = stored.resource(&address("backup.nightly")).unwrap();
    assert_eq!(backup.remote_id().as_str(), "backup-1");
    assert_eq!(backup.last_applied().as_json()["prefix"], "weekly");
    assert_eq!(
        apply_workspace(&client, &config_file)
            .await
            .unwrap()
            .applied(),
        0
    );

    // Clearing retention is an in-place update that sends an explicit null.
    fs::write(
        &config_file,
        config(
            "      nightly:\n        target: postgres.main\n        destination: { name: offsite }\n        schedule: \"5 4 * * 1\"\n        prefix: weekly\n        database: other\n        enabled: true\n        keep_latest: null\n        include_encryption_key: true\n",
        ),
    )
    .unwrap();
    apply_workspace(&client, &config_file).await.unwrap();
    assert!(
        fake.matching("POST /api/backup.update")
            .last()
            .unwrap()
            .contains(r#""keepLatestCount":null"#)
    );
    assert_eq!(fake.world().rows[0].keep, None);
    assert_no_external_material(&durable_text(directory.path()));
    fake.assert_inert();
}

#[tokio::test]
async fn omitted_retention_leaves_the_remote_value_unmanaged() {
    let fake = Fake::start();
    let (_directory, config_file) = workspace(&fake, &config(NIGHTLY));
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    // Dropping `keep_latest` stops managing it; only state changes.
    fs::write(
        &config_file,
        config(&NIGHTLY.replace("        keep_latest: 7\n", "")),
    )
    .unwrap();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    assert_eq!(fake.count("POST /api/backup.update"), 0);
    assert_eq!(fake.world().rows[0].keep, Some(7));

    // An unrelated edit then carries the unmanaged remote retention through.
    fs::write(
        &config_file,
        config(
            &NIGHTLY
                .replace("        keep_latest: 7\n", "")
                .replace("0 3 * * *", "1 2 * * *"),
        ),
    )
    .unwrap();
    apply_workspace(&client, &config_file).await.unwrap();
    assert!(
        fake.matching("POST /api/backup.update")[0].contains(r#""keepLatestCount":7"#),
        "unmanaged fields are carried from the fresh read"
    );
    fake.assert_inert();
}

#[tokio::test]
async fn ignored_fields_carry_the_freshly_read_remote_value() {
    let fake = Fake::start();
    let ignoring = |schedule: &str, prefix: &str| {
        config(&format!(
            "      nightly:\n        target: postgres.main\n        destination: {{ name: offsite }}\n        schedule: \"{schedule}\"\n        prefix: {prefix}\n        database: app\n        enabled: false\n        keep_latest: 7\n        lifecycle: {{ ignore_changes: [schedule] }}\n"
        ))
    };
    let (_directory, config_file) = workspace(&fake, &ignoring("0 3 * * *", "nightly"));
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    // The remote schedule drifts out of band and the configuration edits another field.
    fake.world().row_mut("backup-1").schedule = "9 9 * * *".to_owned();
    fs::write(&config_file, ignoring("1 1 * * *", "weekly")).unwrap();

    apply_workspace(&client, &config_file).await.unwrap();

    let update = &fake.matching("POST /api/backup.update")[0];
    assert!(update.contains(r#""prefix":"weekly""#));
    assert!(
        update.contains(r#""schedule":"9 9 * * *""#),
        "the ignored schedule is never overwritten"
    );
    assert_eq!(fake.world().rows[0].schedule, "9 9 * * *");
    fake.assert_inert();
}

#[tokio::test]
async fn destination_reselection_updates_the_association_in_place() {
    let fake = Fake::start();
    let (directory, config_file) = workspace(&fake, &config(NIGHTLY));
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(&config_file, config(&NIGHTLY.replace("offsite", "archive"))).unwrap();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    assert!(
        fake.matching("POST /api/backup.update")[0].contains(r#""destinationId":"destination-2""#)
    );
    assert_eq!(fake.world().rows[0].destination_id, "destination-2");
    assert_eq!(fake.count("POST /api/backup.remove"), 0);
    let stored = state(&fake, directory.path());
    assert_eq!(
        stored
            .resource(&address("backup.nightly"))
            .unwrap()
            .last_applied()
            .as_json()["destination"],
        serde_json::json!({ "name": "archive" })
    );
    assert_eq!(
        apply_workspace(&client, &config_file)
            .await
            .unwrap()
            .applied(),
        0
    );
    assert_no_external_material(&durable_text(directory.path()));
    fake.assert_inert();
}

#[tokio::test]
async fn target_change_deletes_before_creating_under_the_new_target() {
    let fake = Fake::start();
    let (directory, config_file) = workspace(&fake, &config(NIGHTLY));
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        config(&NIGHTLY.replace("postgres.main", "mysql.sql")),
    )
    .unwrap();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    let lines = fake.lines();
    let remove = lines
        .iter()
        .position(|line| line == "POST /api/backup.remove")
        .unwrap();
    let create = lines
        .iter()
        .rposition(|line| line == "POST /api/backup.create")
        .unwrap();
    assert!(remove < create);
    assert!(fake.matching("POST /api/backup.remove")[0].contains(r#""backupId":"backup-1""#));
    let created = &fake.matching("POST /api/backup.create")[1];
    assert!(created.contains(r#""mysqlId":"mysql-1""#));
    assert!(created.contains(r#""databaseType":"mysql""#));
    assert_eq!(fake.count("POST /api/backup.update"), 0);
    let stored = state(&fake, directory.path());
    let backup = stored.resource(&address("backup.nightly")).unwrap();
    assert_eq!(backup.remote_id().as_str(), "backup-2");
    assert_eq!(backup.dependencies(), &[address("mysql.sql")]);
    assert_eq!(fake.world().rows.len(), 1);
    fake.assert_inert();
}

#[tokio::test]
async fn removing_a_backup_and_its_target_deletes_the_backup_first() {
    let fake = Fake::start();
    let (directory, config_file) = workspace(&fake, &config(NIGHTLY));
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    postgres:\n      other: {}\n    mysql:\n      sql: {}\n",
    )
    .unwrap();

    apply_workspace(&client, &config_file).await.unwrap();

    let lines = fake.lines();
    let remove = lines
        .iter()
        .position(|line| line == "POST /api/backup.remove")
        .expect("the Backup is removed");
    let delete = lines
        .iter()
        .position(|line| line == "POST /api/postgres.remove")
        .expect("the target is removed");
    assert!(remove < delete, "a Backup is removed before its target");
    let stored = state(&fake, directory.path());
    assert!(stored.resource(&address("backup.nightly")).is_none());
    assert!(fake.world().rows.is_empty());
    fake.assert_inert();
}

#[tokio::test]
async fn declarative_removal_of_only_the_backup_proves_absence_and_converges() {
    let fake = Fake::start();
    let (directory, config_file) = workspace(&fake, &config(NIGHTLY));
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(&config_file, config("      {}\n")).unwrap();

    let summary = apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(summary.applied(), 1);
    assert_eq!(fake.count("POST /api/backup.remove"), 1);
    assert!(fake.world().rows.is_empty());
    assert!(
        state(&fake, directory.path())
            .resource(&address("backup.nightly"))
            .is_none()
    );
    assert_eq!(
        apply_workspace(&client, &config_file)
            .await
            .unwrap()
            .applied(),
        0
    );
    fake.assert_inert();
}

#[tokio::test]
async fn destroy_removes_backups_before_their_targets() {
    let fake = Fake::start();
    let (_directory, config_file) = workspace(&fake, &config(NIGHTLY));
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();

    destroy_workspace_with_approval(&client, &config_file, |plan| {
        let order = plan
            .changes()
            .iter()
            .map(|change| change.address().to_string())
            .collect::<Vec<_>>();
        let position = |name: &str| order.iter().position(|item| item == name).unwrap();
        assert!(position("backup.nightly") < position("postgres.main"));
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(fake.count("POST /api/backup.remove"), 1);
    fake.assert_inert();
}

#[tokio::test]
async fn protected_backup_replacement_is_blocked_before_any_mutation() {
    let fake = Fake::start();
    fake.world().add_row(fake::row(
        "backup-1",
        "postgres-1",
        "destination-1",
        "nightly",
    ));
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(
        directory.path(),
        fake.instance(),
        vec![(
            "backup.nightly",
            backup_state("backup-1", "postgres.main", "offsite", "nightly", true),
        )],
    );
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(
        &config_file,
        config(&NIGHTLY.replace("postgres.main", "mysql.sql")),
    )
    .unwrap();

    let error = apply_workspace(&fake.client(), &config_file)
        .await
        .expect_err("a protected Backup cannot be replaced");

    assert!(matches!(error, ApplyWorkspaceError::PlanBlocked));
    assert!(
        fake.requests()
            .iter()
            .all(|request| request.starts_with("GET /api/"))
    );
}

#[tokio::test]
async fn an_unresolved_destination_blocks_apply_before_any_mutation() {
    for destinations in [
        Vec::new(),
        vec![
            ("destination-1".to_owned(), "offsite".to_owned()),
            ("destination-9".to_owned(), "offsite".to_owned()),
        ],
    ] {
        let fake = Fake::start();
        fake.world().destinations = destinations;
        let (directory, config_file) = workspace(&fake, &config(NIGHTLY));

        let error = apply_workspace(&fake.client(), &config_file)
            .await
            .expect_err("an unresolved destination blocks apply");

        assert!(matches!(error, ApplyWorkspaceError::PlanBlocked));
        assert!(
            fake.requests()
                .iter()
                .all(|request| request.starts_with("GET /api/"))
        );
        assert_no_external_material(&durable_text(directory.path()));
    }
}

#[tokio::test]
async fn an_unmanaged_backup_at_the_collision_key_blocks_planning() {
    let fake = Fake::start();
    fake.world().add_row(fake::row(
        "backup-9",
        "postgres-1",
        "destination-1",
        "/nightly/",
    ));
    let (_directory, config_file) = workspace(&fake, &config(NIGHTLY));

    let error = apply_workspace(&fake.client(), &config_file)
        .await
        .expect_err("an existing Backup at the key is never adopted by planning");

    assert!(matches!(error, ApplyWorkspaceError::PlanBlocked));
    assert_eq!(fake.count("POST /api/backup.create"), 0);
    assert_eq!(fake.count("POST /api/backup.update"), 0);
}

#[tokio::test]
async fn definitive_create_rejection_fails_the_step_without_retry_or_state() {
    let fake = Fake::start();
    fake.world()
        .fault("backup.create", Mode::Reject("400 Bad Request"));
    let (directory, config_file) = workspace(&fake, &config(NIGHTLY));

    let error = apply_workspace(&fake.client(), &config_file)
        .await
        .expect_err("a definitive rejection fails the apply");

    assert!(matches!(
        error,
        ApplyWorkspaceError::RemoteMutation {
            code: FailureCode::Validation
        }
    ));
    assert_eq!(fake.count("POST /api/backup.create"), 1);
    assert!(
        state(&fake, directory.path())
            .resource(&address("backup.nightly"))
            .is_none()
    );
    // The failed step is journaled as definitive, never as an uncertain in-progress step.
    let status = format!(
        "{:?}",
        store(&fake, directory.path()).recovery_status().unwrap()
    );
    assert!(status.contains("Failed(Validation)"), "{status}");
    assert!(!status.contains("InProgress"), "{status}");
    assert_no_external_material(&format!("{error:?}{error}"));
    fake.assert_inert();
}

#[tokio::test]
async fn outcome_unknown_create_stays_in_progress_and_is_never_retried() {
    let fake = Fake::start();
    fake.world().fault("backup.create", Mode::DropAfterApply);
    let (directory, config_file) = workspace(&fake, &config(NIGHTLY));

    let error = apply_workspace(&fake.client(), &config_file)
        .await
        .expect_err("an unknown outcome stops the apply");

    assert!(matches!(
        error,
        ApplyWorkspaceError::RemoteMutation {
            code: FailureCode::TransportOutcomeUnknown
        }
    ));
    assert_eq!(fake.count("POST /api/backup.create"), 1);
    assert_eq!(
        fake.world().rows.len(),
        1,
        "the mutation did apply remotely"
    );
    assert_ne!(
        store(&fake, directory.path()).recovery_status().unwrap(),
        RecoveryStatus::Clean,
        "the interrupted step remains recoverable"
    );
    assert_no_external_material(&durable_text(directory.path()));
}

#[tokio::test]
async fn outcome_unknown_update_stays_in_progress_and_is_never_retried() {
    let fake = Fake::start();
    let (directory, config_file) = workspace(&fake, &config(NIGHTLY));
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fake.world().fault("backup.update", Mode::DropBeforeApply);
    fs::write(
        &config_file,
        config(&NIGHTLY.replace("0 3 * * *", "1 2 * * *")),
    )
    .unwrap();

    let error = apply_workspace(&client, &config_file)
        .await
        .expect_err("an unknown outcome stops the apply");

    assert!(matches!(
        error,
        ApplyWorkspaceError::RemoteMutation {
            code: FailureCode::TransportOutcomeUnknown
        }
    ));
    assert_eq!(fake.count("POST /api/backup.update"), 1);
    assert_ne!(
        store(&fake, directory.path()).recovery_status().unwrap(),
        RecoveryStatus::Clean
    );
}

#[tokio::test]
async fn a_backup_never_executes_deploys_or_contacts_its_destination() {
    let fake = Fake::start();
    let (_directory, config_file) = workspace(
        &fake,
        &config(&format!(
            "{NIGHTLY}      second:\n        target: mysql.sql\n        destination: {{ name: archive }}\n        schedule: \"0 4 * * *\"\n        prefix: second\n        database: app\n"
        )),
    );
    let client = fake.client();

    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(
        &config_file,
        config(&format!(
            "{}      second:\n        target: mysql.sql\n        destination: {{ name: offsite }}\n        schedule: \"0 5 * * *\"\n        prefix: second\n        database: app\n        enabled: false\n",
            NIGHTLY.replace("keep_latest: 7", "keep_latest: 8")
        )),
    )
    .unwrap();
    apply_workspace(&client, &config_file).await.unwrap();
    fs::write(&config_file, config("      {}\n")).unwrap();
    apply_workspace(&client, &config_file).await.unwrap();

    assert_eq!(fake.count("POST /api/backup.create"), 2);
    assert_eq!(fake.count("POST /api/backup.update"), 2);
    assert_eq!(fake.count("POST /api/backup.remove"), 2);
    fake.assert_inert();
}
