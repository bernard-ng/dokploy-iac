#[path = "support/backup.rs"]
mod fake;

use std::fs;
use std::path::Path;

use dokploy_cli::executor::{ApplyWorkspaceError, apply_workspace};
use dokploy_cli::recovery::{
    RecoverWorkspaceError, RecoveryAction, recover_workspace_with_approval,
};
use dokploy_state::{
    ExpectedCheckpoint, JournalAction, OperationJournal, PlanDigest, RecoveryStatus, StateStore,
};
use fake::{
    Fake, Mode, NIGHTLY, SECRET_CANARY, address, backup_state, config, row, seed_workspace,
};

fn store(fake: &Fake, directory: &Path) -> StateStore {
    StateStore::new(directory, fake.instance()).unwrap()
}

fn only_reads(fake: &Fake) {
    assert!(fake.unrouted().is_empty());
    assert!(
        fake.requests()
            .iter()
            .all(|request| request.starts_with("GET /api/")),
        "{:?}",
        fake.lines()
    );
}

fn open_step(store: &StateStore, action: JournalAction, expected: ExpectedCheckpoint) {
    let mut write = store.begin_write().unwrap();
    let mut journal =
        OperationJournal::begin(&mut write, PlanDigest::parse("e".repeat(64)).unwrap()).unwrap();
    journal
        .start_recoverable_step(address("backup.nightly"), action, expected)
        .unwrap();
}

fn pending_create() -> ExpectedCheckpoint {
    ExpectedCheckpoint::create(backup_state(
        "recovery-pending",
        "postgres.main",
        "offsite",
        "nightly",
        false,
    ))
    .unwrap()
}

/// A workspace with an interrupted Create step for the standard Backup.
fn interrupted_create(fake: &Fake) -> (tempfile::TempDir, std::path::PathBuf, StateStore) {
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(directory.path(), fake.instance(), Vec::new());
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(NIGHTLY)).unwrap();
    let store = store(fake, directory.path());
    open_step(&store, JournalAction::Create, pending_create());
    (directory, config_file, store)
}

fn assert_no_external_material(directory: &Path) {
    fn walk(path: &Path, output: &mut String) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                walk(&entry.unwrap().path(), output);
            }
        } else if let Ok(bytes) = fs::read(path) {
            output.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    let mut text = String::new();
    walk(&directory.join(".dokploy"), &mut text);
    for needle in [SECRET_CANARY, "destination-1", "destination-2"] {
        assert!(!text.contains(needle), "{needle}");
    }
}

#[tokio::test]
async fn uncertain_create_adopts_one_exact_collision_key_without_retrying() {
    let fake = Fake::start();
    fake.world()
        .add_row(row("backup-1", "postgres-1", "destination-1", "nightly"));
    let (directory, config_file, store) = interrupted_create(&fake);

    let result = recover_workspace_with_approval(&fake.client(), &config_file, |preview| {
        assert_eq!(preview.address(), Some(&address("backup.nightly")));
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    let state = store.inspect().unwrap().unwrap();
    let adopted = state.resource(&address("backup.nightly")).unwrap();
    assert_eq!(adopted.remote_id().as_str(), "backup-1");
    assert_eq!(adopted.dependencies(), &[address("postgres.main")]);
    assert_eq!(
        adopted.last_applied().as_json()["destination"]["name"],
        "offsite"
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    only_reads(&fake);
    assert_no_external_material(directory.path());
}

#[tokio::test]
async fn an_interrupted_apply_is_recovered_and_the_next_apply_creates_nothing_more() {
    let fake = Fake::start();
    fake.world().fault("backup.create", Mode::DropAfterApply);
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(directory.path(), fake.instance(), Vec::new());
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(NIGHTLY)).unwrap();
    let client = fake.client();
    apply_workspace(&client, &config_file)
        .await
        .expect_err("the create outcome is unknown");

    let result = recover_workspace_with_approval(&client, &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::AdoptCreatedResource);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    let converged = apply_workspace(&client, &config_file).await.unwrap();
    assert_eq!(converged.applied(), 0);
    assert_eq!(fake.count("POST /api/backup.create"), 1, "never retried");
    assert_eq!(fake.world().rows.len(), 1);
    fake.assert_inert();
    assert_no_external_material(directory.path());
}

#[tokio::test]
async fn uncertain_create_with_authoritative_absence_confirms_no_change() {
    let fake = Fake::start();
    let (_directory, config_file, store) = interrupted_create(&fake);

    let result = recover_workspace_with_approval(&fake.client(), &config_file, |preview| {
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
            .resource(&address("backup.nightly"))
            .is_none()
    );
    only_reads(&fake);
}

#[tokio::test]
async fn uncertain_create_with_a_different_backup_at_the_key_needs_manual_intervention() {
    let fake = Fake::start();
    let mut other = row("backup-9", "postgres-1", "destination-1", "nightly");
    other.schedule = "9 9 * * *".to_owned();
    fake.world().add_row(other);
    let (_directory, config_file, store) = interrupted_create(&fake);

    let error = recover_workspace_with_approval(&fake.client(), &config_file, |_| Ok(true))
        .await
        .expect_err("a non-matching Backup is never adopted");

    assert!(matches!(error, RecoverWorkspaceError::ManualIntervention));
    assert!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&address("backup.nightly"))
            .is_none()
    );
    assert_ne!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
}

#[tokio::test]
async fn recovery_needs_manual_intervention_when_the_destination_no_longer_resolves() {
    for destinations in [
        Vec::new(),
        vec![
            ("destination-1".to_owned(), "offsite".to_owned()),
            ("destination-9".to_owned(), "offsite".to_owned()),
        ],
    ] {
        let fake = Fake::start();
        fake.world()
            .add_row(row("backup-1", "postgres-1", "destination-1", "nightly"));
        fake.world().destinations = destinations;
        let (directory, config_file, store) = interrupted_create(&fake);

        let error = recover_workspace_with_approval(&fake.client(), &config_file, |_| Ok(true))
            .await
            .expect_err("an unresolved destination cannot prove the outcome");

        assert!(matches!(error, RecoverWorkspaceError::ManualIntervention));
        assert!(!format!("{error:?}{error}").contains("offsite"));
        assert_ne!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
        only_reads(&fake);
        assert_no_external_material(directory.path());
    }
}

/// Applies the standard Backup, then edits its schedule and injects an update fault.
async fn interrupted_update(
    fake: &Fake,
    mode: Mode,
) -> (tempfile::TempDir, std::path::PathBuf, StateStore) {
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(directory.path(), fake.instance(), Vec::new());
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(NIGHTLY)).unwrap();
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fake.world().fault("backup.update", mode);
    fs::write(
        &config_file,
        config(&NIGHTLY.replace("0 3 * * *", "1 2 * * *")),
    )
    .unwrap();
    apply_workspace(&client, &config_file)
        .await
        .expect_err("the update outcome is unknown");
    let store = store(fake, directory.path());
    (directory, config_file, store)
}

#[tokio::test]
async fn uncertain_update_is_confirmed_only_from_authoritative_complete_state() {
    let fake = Fake::start();
    let (directory, config_file, store) = interrupted_update(&fake, Mode::DropAfterApply).await;
    fake.forget_requests();

    let result = recover_workspace_with_approval(&fake.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::CheckpointConfirmedSuccess);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    let state = store.inspect().unwrap().unwrap();
    assert_eq!(
        state
            .resource(&address("backup.nightly"))
            .unwrap()
            .last_applied()
            .as_json()["schedule"],
        "1 2 * * *"
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    only_reads(&fake);
    assert_no_external_material(directory.path());
}

#[tokio::test]
async fn uncertain_update_that_did_not_apply_confirms_no_change() {
    let fake = Fake::start();
    let (_directory, config_file, store) = interrupted_update(&fake, Mode::DropBeforeApply).await;
    fake.forget_requests();

    let result = recover_workspace_with_approval(&fake.client(), &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::ConfirmNoChange);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    let state = store.inspect().unwrap().unwrap();
    assert_eq!(
        state
            .resource(&address("backup.nightly"))
            .unwrap()
            .last_applied()
            .as_json()["schedule"],
        "0 3 * * *"
    );
    only_reads(&fake);
}

#[tokio::test]
async fn uncertain_update_that_landed_somewhere_else_needs_manual_intervention() {
    let fake = Fake::start();
    let (_directory, config_file, store) = interrupted_update(&fake, Mode::DropBeforeApply).await;
    // Someone else changed the Backup to a third value in the meantime.
    fake.world().row_mut("backup-1").schedule = "7 7 * * *".to_owned();

    let error = recover_workspace_with_approval(&fake.client(), &config_file, |_| Ok(true))
        .await
        .expect_err("an unexplained remote value is never checkpointed");

    assert!(matches!(error, RecoverWorkspaceError::ManualIntervention));
    assert_ne!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
}

#[tokio::test]
async fn uncertain_delete_is_confirmed_by_authoritative_absence() {
    let fake = Fake::start();
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(directory.path(), fake.instance(), Vec::new());
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(NIGHTLY)).unwrap();
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fake.world().fault("backup.remove", Mode::DropAfterApply);
    fs::write(&config_file, config("      {}\n")).unwrap();
    apply_workspace(&client, &config_file)
        .await
        .expect_err("the delete outcome is unknown");
    fake.forget_requests();

    let result = recover_workspace_with_approval(&client, &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::CheckpointConfirmedSuccess);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    let store = store(&fake, directory.path());
    assert!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&address("backup.nightly"))
            .is_none()
    );
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    only_reads(&fake);
}

#[tokio::test]
async fn uncertain_delete_that_did_not_apply_confirms_no_change_and_is_not_retried() {
    let fake = Fake::start();
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(directory.path(), fake.instance(), Vec::new());
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, config(NIGHTLY)).unwrap();
    let client = fake.client();
    apply_workspace(&client, &config_file).await.unwrap();
    fake.world().fault("backup.remove", Mode::DropBeforeApply);
    fs::write(&config_file, config("      {}\n")).unwrap();
    let error = apply_workspace(&client, &config_file)
        .await
        .expect_err("the delete outcome is unknown");
    assert!(matches!(error, ApplyWorkspaceError::RemoteMutation { .. }));
    fake.forget_requests();

    let result = recover_workspace_with_approval(&client, &config_file, |preview| {
        assert_eq!(preview.action(), RecoveryAction::ConfirmNoChange);
        Ok(true)
    })
    .await
    .unwrap();

    assert_eq!(result.recovered_steps(), 1);
    assert_eq!(fake.world().rows.len(), 1, "recovery never deletes");
    only_reads(&fake);
}
