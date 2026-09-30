use std::{fs, path::Path};

use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, StateFile, StateStore, StateStoreError, WriteSession,
};
use semver::Version;
use serde_json::json;
use tempfile::tempdir;

#[test]
fn initializes_absent_state_without_a_backup() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let state = initial_state();

    let mut session = store
        .begin_write()
        .expect("the write lock must be acquired");
    checkpoint_through_exclusive_borrow(&mut session, ExpectedState::absent(), &state)
        .expect("absent state must initialize");

    assert_eq!(
        store.inspect().expect("state must be readable"),
        Some(state)
    );
    assert!(workspace.path().join(".dokploy/state.json").is_file());
    assert!(!workspace.path().join(".dokploy/state.backup.json").exists());
}

#[test]
fn initialization_refuses_to_replace_existing_state() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let state_path = workspace.path().join(".dokploy/state.json");
    let initial = initial_state();
    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial)
        .expect("state must initialize");
    let primary_before = fs::read(&state_path).expect("primary bytes must exist");

    let replacement = initial_state();
    let error = store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &replacement)
        .expect_err("existing state must not be initialized again");

    assert!(matches!(error, StateStoreError::StateAlreadyExists));
    assert_eq!(
        fs::read(state_path).expect("primary bytes must remain"),
        primary_before
    );
    assert!(!workspace.path().join(".dokploy/state.backup.json").exists());
}

#[test]
fn checkpoint_backs_up_current_bytes_before_replacing_primary() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let state_path = workspace.path().join(".dokploy/state.json");
    let backup_path = workspace.path().join(".dokploy/state.backup.json");
    let initial = initial_state();

    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial)
        .expect("state must initialize");
    let initial_bytes = fs::read(&state_path).expect("initial state bytes must exist");

    let expected = ExpectedState::from_state(&initial);
    let mut next = initial;
    add_application(&mut next, "api");
    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(expected, &next)
        .expect("the next state must checkpoint");

    assert_eq!(
        fs::read(backup_path).expect("backup bytes must exist"),
        initial_bytes
    );
    let primary_bytes = fs::read(state_path).expect("primary bytes must exist");
    assert_ne!(primary_bytes, initial_bytes);
    assert_eq!(primary_bytes.last(), Some(&b'\n'));
    assert_eq!(store.inspect().expect("state must be readable"), Some(next));
}

#[test]
fn stale_competing_snapshot_leaves_primary_and_backup_unchanged() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let state_path = workspace.path().join(".dokploy/state.json");
    let backup_path = workspace.path().join(".dokploy/state.backup.json");
    let initial = initial_state();
    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial)
        .expect("state must initialize");

    let observed = store
        .inspect()
        .expect("state must be readable")
        .expect("state must exist");
    let expected = ExpectedState::from_state(&observed);
    let mut winner = observed.clone();
    let mut stale = observed;
    add_application(&mut winner, "winner");
    add_application(&mut stale, "stale");

    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(expected.clone(), &winner)
        .expect("the winning checkpoint must succeed");
    let primary_before = fs::read(&state_path).expect("primary bytes must exist");
    let backup_before = fs::read(&backup_path).expect("backup bytes must exist");

    let error = store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(expected, &stale)
        .expect_err("a stale checkpoint must fail");

    assert!(matches!(error, StateStoreError::StaleState { .. }));
    assert_eq!(
        fs::read(state_path).expect("primary bytes must remain"),
        primary_before
    );
    assert_eq!(
        fs::read(backup_path).expect("backup bytes must remain"),
        backup_before
    );
}

#[test]
fn writer_lock_is_fail_fast_and_released_by_drop() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let first = store
        .begin_write()
        .expect("the first writer must acquire the lock");

    let error = match store.begin_write() {
        Ok(_) => panic!("a competing writer must not acquire the lock"),
        Err(error) => error,
    };
    assert!(matches!(error, StateStoreError::LockContended));

    drop(first);
    store
        .begin_write()
        .expect("dropping the session must release the lock");
}

#[test]
fn malformed_primary_is_not_treated_as_absent_or_overwritten() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let state_directory = workspace.path().join(".dokploy");
    fs::create_dir_all(&state_directory).expect("state directory must be created");
    let state_path = state_directory.join("state.json");
    let evidence = b"{ malformed state evidence\n";
    fs::write(&state_path, evidence).expect("malformed evidence must be written");
    let store = state_store(workspace.path());

    let inspection_error = store
        .inspect()
        .expect_err("malformed state must be rejected");
    assert!(matches!(inspection_error, StateStoreError::StateCorrupt));
    assert!(!inspection_error.to_string().contains("evidence"));
    let error = store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial_state())
        .expect_err("malformed state must not be reinitialized");

    assert!(matches!(error, StateStoreError::StateCorrupt));
    assert_eq!(
        fs::read(state_path).expect("evidence must remain"),
        evidence
    );
    assert!(!state_directory.join("state.backup.json").exists());
}

#[test]
fn orphan_backup_fails_closed_without_overwriting_evidence() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let state_directory = workspace.path().join(".dokploy");
    fs::create_dir_all(&state_directory).expect("state directory must be created");
    let backup_path = state_directory.join("state.backup.json");
    let evidence = b"previous durable state\n";
    fs::write(&backup_path, evidence).expect("backup evidence must be written");
    let store = state_store(workspace.path());

    assert!(matches!(
        store.inspect(),
        Err(StateStoreError::OrphanBackup)
    ));
    let error = store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial_state())
        .expect_err("orphan evidence must require recovery");

    assert!(matches!(error, StateStoreError::OrphanBackup));
    assert_eq!(
        fs::read(backup_path).expect("backup evidence must remain"),
        evidence
    );
    assert!(!state_directory.join("state.json").exists());
}

#[test]
fn oversized_state_is_rejected_before_parsing() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let state_directory = workspace.path().join(".dokploy");
    fs::create_dir_all(&state_directory).expect("state directory must be created");
    fs::write(state_directory.join("state.json"), b"123456789")
        .expect("oversized state must be written");
    let store = StateStore::with_max_state_bytes(workspace.path(), instance(), 8)
        .expect("state store must bind to the workspace");

    assert!(matches!(
        store.inspect(),
        Err(StateStoreError::StateTooLarge { max_bytes: 8 })
    ));
}

#[test]
fn oversized_proposed_state_is_rejected_before_backup_or_primary_changes() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let initial = initial_state();
    let initial_size = serde_json::to_vec_pretty(&initial)
        .expect("initial state must serialize")
        .len() as u64
        + 1;
    let store = StateStore::with_max_state_bytes(workspace.path(), instance(), initial_size + 128)
        .expect("state store must bind to the workspace");
    let state_path = workspace.path().join(".dokploy/state.json");
    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial)
        .expect("state must initialize");
    let primary_before = fs::read(&state_path).expect("primary bytes must exist");

    let expected = ExpectedState::from_state(&initial);
    let mut proposed = initial;
    add_application(&mut proposed, &"a".repeat(1024));
    let error = store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(expected, &proposed)
        .expect_err("oversized proposed state must fail");

    assert!(matches!(
        error,
        StateStoreError::ProposedStateTooLarge { .. }
    ));
    assert_eq!(
        fs::read(state_path).expect("primary bytes must remain"),
        primary_before
    );
    assert!(!workspace.path().join(".dokploy/state.backup.json").exists());
}

#[test]
fn duplicate_resource_addresses_are_rejected_at_the_json_boundary() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let state_directory = workspace.path().join(".dokploy");
    fs::create_dir_all(&state_directory).expect("state directory must be created");
    let mut state = initial_state();
    add_application(&mut state, "api");
    let document = serde_json::to_value(state).expect("state must serialize");
    let resource = &document["resources"]["application.api"];
    let duplicate = format!(
        concat!(
            "{{",
            "\"formatVersion\":{},",
            "\"cliVersion\":{},",
            "\"lineage\":{},",
            "\"serial\":{},",
            "\"instance\":{},",
            "\"resources\":{{",
            "\"application.api\":{},",
            "\"application.api\":{}",
            "}}}}"
        ),
        document["formatVersion"],
        document["cliVersion"],
        document["lineage"],
        document["serial"],
        document["instance"],
        resource,
        resource,
    );
    fs::write(state_directory.join("state.json"), duplicate)
        .expect("duplicate state must be written");
    let store = state_store(workspace.path());

    assert!(matches!(
        store.inspect(),
        Err(StateStoreError::StateCorrupt)
    ));
}

#[test]
fn invalid_proposed_revision_is_rejected_before_backup_or_primary_changes() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let state_path = workspace.path().join(".dokploy/state.json");
    let backup_path = workspace.path().join(".dokploy/state.backup.json");
    let initial = initial_state();
    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial)
        .expect("state must initialize");
    let primary_before = fs::read(&state_path).expect("primary bytes must exist");

    let expected = ExpectedState::from_state(&initial);
    let mut skipped_serial = initial;
    add_application(&mut skipped_serial, "first");
    add_application(&mut skipped_serial, "second");
    let error = store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(expected, &skipped_serial)
        .expect_err("a skipped serial must fail");

    assert!(matches!(
        error,
        StateStoreError::InvalidSerialTransition {
            expected: 1,
            found: 2
        }
    ));
    assert_eq!(
        fs::read(state_path).expect("primary bytes must remain"),
        primary_before
    );
    assert!(!backup_path.exists());
}

#[test]
fn proposed_lineage_mismatch_is_rejected_before_writes() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let state_path = workspace.path().join(".dokploy/state.json");
    let initial = initial_state();
    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial)
        .expect("state must initialize");
    let primary_before = fs::read(&state_path).expect("primary bytes must exist");

    let expected = ExpectedState::from_state(&initial);
    let mut another_lineage = initial_state();
    add_application(&mut another_lineage, "api");
    let error = store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(expected, &another_lineage)
        .expect_err("another lineage must fail");

    assert!(matches!(error, StateStoreError::ProposedLineageMismatch));
    assert_eq!(
        fs::read(state_path).expect("primary bytes must remain"),
        primary_before
    );
    assert!(!workspace.path().join(".dokploy/state.backup.json").exists());
}

#[test]
fn expected_and_proposed_instance_mismatches_are_rejected_before_writes() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let state_path = workspace.path().join(".dokploy/state.json");
    let initial = initial_state();
    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial)
        .expect("state must initialize");
    let primary_before = fs::read(&state_path).expect("primary bytes must exist");

    let mut other_document = serde_json::to_value(&initial).expect("state must serialize");
    other_document["instance"] = json!("https://staging.example.com/");
    let other_document =
        serde_json::to_vec(&other_document).expect("other instance JSON must serialize");
    let other_instance =
        StateFile::from_json_slice(&other_document).expect("other instance state must deserialize");
    let mut valid_next = initial.clone();
    add_application(&mut valid_next, "valid");
    let expected_error = store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::from_state(&other_instance), &valid_next)
        .expect_err("an expected-instance mismatch must fail");
    assert!(matches!(
        expected_error,
        StateStoreError::ExpectedInstanceMismatch
    ));

    let mut wrong_instance_next = other_instance;
    add_application(&mut wrong_instance_next, "wrong");
    let proposed_error = store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::from_state(&initial), &wrong_instance_next)
        .expect_err("a proposed-instance mismatch must fail");
    assert!(matches!(
        proposed_error,
        StateStoreError::ProposedInstanceMismatch
    ));

    assert_eq!(
        fs::read(state_path).expect("primary bytes must remain"),
        primary_before
    );
    assert!(!workspace.path().join(".dokploy/state.backup.json").exists());
}

#[test]
fn store_instance_binding_applies_to_inspection_and_initialization() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let production = state_store(workspace.path());
    let staging_instance = InstanceIdentity::parse("https://staging.example.com")
        .expect("staging instance must be valid");
    let staging_state = StateFile::new(Version::new(0, 1, 0), staging_instance.clone());

    let error = production
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &staging_state)
        .expect_err("initial state from another instance must fail");
    assert!(matches!(error, StateStoreError::ProposedInstanceMismatch));

    let staging_store = StateStore::new(workspace.path(), staging_instance)
        .expect("staging store must bind to workspace");
    staging_store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &staging_state)
        .expect("staging state must initialize");
    assert!(matches!(
        production.inspect(),
        Err(StateStoreError::StateInstanceMismatch)
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_state_directory_is_rejected_before_locking_or_inspection() {
    use std::os::unix::fs::symlink;

    let workspace = tempdir().expect("temporary workspace must be created");
    let outside = tempdir().expect("outside directory must be created");
    symlink(outside.path(), workspace.path().join(".dokploy"))
        .expect("state-directory symlink must be created");
    let error = StateStore::new(workspace.path(), instance())
        .expect_err("symlinked state directory must fail construction");

    assert!(matches!(error, StateStoreError::UnsafeStateDirectory));
}

#[cfg(unix)]
#[test]
fn checkpoint_revalidates_state_directory_after_lock_acquisition() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let mut session = store
        .begin_write()
        .expect("the write lock must be acquired");
    let state_directory = workspace.path().join(".dokploy");
    let displaced = workspace.path().join(".dokploy-displaced");
    fs::rename(&state_directory, &displaced).expect("state directory must move");
    fs::create_dir(&state_directory).expect("replacement directory must be created");

    let error = session
        .checkpoint(ExpectedState::absent(), &initial_state())
        .expect_err("replaced state directory must fail");

    assert!(matches!(error, StateStoreError::UnsafeStateDirectory));
    assert!(!state_directory.join("state.json").exists());
}

#[cfg(unix)]
#[test]
fn symlinked_state_artifact_is_rejected_during_inspection_and_checkpoint() {
    use std::os::unix::fs::symlink;

    let workspace = tempdir().expect("temporary workspace must be created");
    let outside = tempdir().expect("outside directory must be created");
    let outside_state = outside.path().join("state.json");
    let evidence = b"outside evidence\n";
    fs::write(&outside_state, evidence).expect("outside evidence must be written");
    let store = state_store(workspace.path());
    let mut session = store
        .begin_write()
        .expect("the write lock must be acquired");
    symlink(&outside_state, workspace.path().join(".dokploy/state.json"))
        .expect("state artifact symlink must be created");

    assert!(matches!(
        store.inspect(),
        Err(StateStoreError::UnsafeStateArtifact)
    ));
    let error = session
        .checkpoint(ExpectedState::absent(), &initial_state())
        .expect_err("symlinked state artifact must fail");

    assert!(matches!(error, StateStoreError::UnsafeStateArtifact));
    assert_eq!(
        fs::read(outside_state).expect("outside evidence must remain"),
        evidence
    );
}

#[cfg(unix)]
#[test]
fn state_directory_and_artifacts_use_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = tempdir().expect("temporary workspace must be created");
    let store = state_store(workspace.path());
    let initial = initial_state();
    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(ExpectedState::absent(), &initial)
        .expect("state must initialize");
    let expected = ExpectedState::from_state(&initial);
    let mut next = initial;
    add_application(&mut next, "api");
    store
        .begin_write()
        .expect("the write lock must be acquired")
        .checkpoint(expected, &next)
        .expect("state must checkpoint");

    assert_eq!(mode(&workspace.path().join(".dokploy")), 0o700);
    for name in ["state.json", "state.backup.json", "state.lock"] {
        assert_eq!(mode(&workspace.path().join(".dokploy").join(name)), 0o600);
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path)
            .expect("artifact metadata must exist")
            .permissions()
            .mode()
            & 0o777
    }
}

fn initial_state() -> StateFile {
    StateFile::new(Version::new(0, 1, 0), instance())
}

fn state_store(workspace: &Path) -> StateStore {
    StateStore::new(workspace, instance()).expect("state store must bind to the workspace")
}

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://deploy.example.com").expect("the instance must be valid")
}

fn add_application(state: &mut StateFile, name: &str) {
    let address: ResourceAddress = format!("application.{name}")
        .parse()
        .expect("address must parse");
    state
        .upsert_resource(
            address,
            ResourceState::new(
                ResourceKind::Application,
                RemoteId::new(format!("application-{name}")).expect("remote ID must be valid"),
                false,
                ManagedInputs::try_from_json(json!({ "description": name }))
                    .expect("inputs must be safe"),
                Some(
                    "environment.production"
                        .parse()
                        .expect("containment must parse"),
                ),
                Vec::new(),
            ),
        )
        .expect("state mutation must succeed");
}

fn checkpoint_through_exclusive_borrow(
    session: &mut WriteSession<'_>,
    expected: ExpectedState,
    proposed: &StateFile,
) -> Result<(), StateStoreError> {
    session.checkpoint(expected, proposed)
}
