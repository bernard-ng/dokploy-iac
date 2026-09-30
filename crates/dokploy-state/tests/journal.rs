use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use dokploy_state::{
    ExpectedCheckpoint, ExpectedState, FailureCode, InstanceIdentity, JournalAction, ManagedInputs,
    OperationJournal, PlanDigest, RecoveryError, RecoveryReason, RecoveryStatus,
    RecoveryStepOutcome, RemoteId, ResourceAddress, ResourceKind, ResourceState, StateFile,
    StateStore, StateStoreError,
};
use semver::Version;
use serde_json::json;
use tempfile::{TempDir, tempdir};
use uuid::Uuid;

#[test]
fn clean_commit_allows_the_next_writer() {
    let (_workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("api"), JournalAction::Create)
        .expect("step must start");
    let mut proposed = initial;
    add_application(&mut proposed, "api", "public value");
    journal
        .succeed(token, Some(remote_id("remote-api")), &proposed)
        .expect("success must checkpoint");
    journal.commit().expect("journal must commit");
    let mut after_commit = proposed.clone();
    add_application(&mut after_commit, "worker", "public value");
    session
        .checkpoint(ExpectedState::from_state(&proposed), &after_commit)
        .expect("successful commit must release the session journal guard");
    drop(session);

    assert_eq!(
        store.recovery_status().expect("scan must succeed"),
        RecoveryStatus::Clean
    );
    store
        .begin_write()
        .expect("a committed journal must not block a writer");
}

#[test]
fn begin_only_requires_recovery_and_blocks_same_session_mutations() {
    let (_workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    drop(journal);

    let checkpoint_error = session
        .checkpoint(
            ExpectedState::from_state(&initial),
            &next_state(&initial, "other"),
        )
        .expect_err("ordinary checkpoint must be blocked");
    assert!(matches!(
        checkpoint_error,
        StateStoreError::JournalOperationPending
    ));
    assert!(OperationJournal::begin(&mut session, digest()).is_err());
    drop(session);

    let summary = recovery_summary(&store);
    assert_eq!(summary.reason(), &RecoveryReason::Begun);
    assert!(matches!(
        store.begin_write(),
        Err(StateStoreError::RecoveryRequired { .. })
    ));
}

#[test]
fn an_open_step_is_reported_as_uncertain() {
    let (_workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    journal
        .start_step(address("api"), JournalAction::Update)
        .expect("step must start");
    drop(journal);
    drop(session);

    let summary = recovery_summary(&store);
    assert_eq!(summary.reason(), &RecoveryReason::StepInProgress);
    let uncertain = summary.uncertain_step().expect("step must be uncertain");
    assert_eq!(uncertain.sequence(), 1);
    assert_eq!(uncertain.address(), &address("api"));
    assert_eq!(uncertain.action(), JournalAction::Update);
}

#[test]
fn recoverable_steps_expose_exact_safe_intent_and_plan_identity() {
    let (workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let target = application_state("remote-placeholder", "public value");
    let expected = ExpectedCheckpoint::create(target).expect("create intent must be valid");

    journal
        .start_recoverable_step(address("api"), JournalAction::Create, expected.clone())
        .expect("recoverable step must start");
    journal
        .start_recoverable_step(
            address("worker"),
            JournalAction::Create,
            ExpectedCheckpoint::create(application_state(
                "remote-placeholder",
                "other public value",
            ))
            .expect("create intent must be valid"),
        )
        .expect("a concurrent recoverable step must start");
    drop(journal);
    drop(session);

    let bytes = journal_bytes(workspace.path());
    assert!(bytes.contains("public value"));
    assert!(!bytes.contains("remote-placeholder"));
    let summary = recovery_summary(&store);
    assert_eq!(summary.plan_digest(), &digest());
    assert_eq!(summary.steps().len(), 2);
    assert_eq!(summary.steps()[0].expected_checkpoint(), Some(&expected));
    assert!(summary.steps()[0].outcome().is_in_progress());
    assert_eq!(summary.steps()[1].address(), &address("worker"));
    assert!(summary.steps()[1].outcome().is_in_progress());
    assert!(!format!("{summary:?}").contains("public value"));
    assert_eq!(
        store.inspect().expect("state remains readable"),
        Some(initial)
    );
}

#[test]
fn recovery_evidence_includes_open_succeeded_and_failed_steps_in_sequence_order() {
    let (_workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let failed = journal
        .start_step(address("api"), JournalAction::Create)
        .expect("failed step must start");
    let succeeded = journal
        .start_step(address("worker"), JournalAction::Create)
        .expect("successful step must start");
    journal
        .start_step(address("later"), JournalAction::Create)
        .expect("open step must start");
    journal
        .fail(failed, FailureCode::RemoteRejected)
        .expect("failure must persist");
    let proposed = next_state(&initial, "worker");
    journal
        .succeed(succeeded, Some(remote_id("remote-worker")), &proposed)
        .expect("success must checkpoint");
    drop(journal);
    drop(session);

    let summary = recovery_summary(&store);
    assert_eq!(summary.steps().len(), 3);
    assert!(matches!(
        summary.steps()[0].outcome(),
        RecoveryStepOutcome::Failed(FailureCode::RemoteRejected)
    ));
    assert!(summary.steps()[1].outcome().is_checkpointed());
    assert!(summary.steps()[2].outcome().is_in_progress());
}

#[test]
fn begin_only_recovery_resolves_once_and_releases_the_writer() {
    let (_workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    drop(OperationJournal::begin(&mut session, digest()).expect("journal must begin"));
    drop(session);

    store
        .begin_recovery()
        .expect("begin-only journal is recoverable")
        .resolve()
        .expect("recovery resolution must persist");

    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
    assert!(matches!(
        store.begin_recovery(),
        Err(RecoveryError::NoRecoveryRequired)
    ));
    store.begin_write().expect("writer must be available");
}

#[test]
fn success_append_checkpoint_and_resolve_boundaries_are_retryable() {
    let (_workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    journal
        .start_recoverable_step(
            address("api"),
            JournalAction::Create,
            ExpectedCheckpoint::create(application_state("placeholder", "public value")).unwrap(),
        )
        .expect("step must start");
    drop(journal);
    drop(session);

    let mut recovery = store.begin_recovery().expect("recovery must begin");
    let invalid = next_state(&initial, "other");
    assert!(
        recovery
            .checkpoint_uncertain_success(1, Some(remote_id("remote-api")), &invalid)
            .is_err(),
        "success is durable before the rejected checkpoint"
    );
    drop(recovery);

    let mut recovery = store
        .begin_recovery()
        .expect("recorded success must reopen");
    assert_eq!(
        recovery.evidence().reason(),
        &RecoveryReason::SuccessWithoutCheckpoint
    );
    let proposed = next_state(&initial, "api");
    recovery
        .checkpoint_recorded_success(1, &proposed)
        .expect("recorded success must checkpoint exactly once");
    drop(recovery);

    let recovery = store
        .begin_recovery()
        .expect("checkpointed success must reopen");
    assert_eq!(
        recovery.evidence().reason(),
        &RecoveryReason::CheckpointedWithoutCommit
    );
    recovery.resolve().expect("resolution must persist");
    assert_eq!(store.inspect().unwrap(), Some(proposed));
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
}

#[test]
fn interrupted_forget_is_recovered_as_state_only_removal() {
    let (_workspace, store, initial) = initialized_store();
    let current = next_state(&initial, "api");
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::from_state(&initial), &current)
        .unwrap();
    let before = current.resource(&address("api")).unwrap().clone();
    let mut session = store.begin_write().unwrap();
    let mut journal = OperationJournal::begin(&mut session, digest()).unwrap();
    journal
        .start_recoverable_step(
            address("api"),
            JournalAction::Forget,
            ExpectedCheckpoint::remove(before),
        )
        .unwrap();
    drop(journal);
    drop(session);

    let mut recovery = store.begin_recovery().expect("forget must be recoverable");
    assert_eq!(
        recovery.evidence().steps()[0].action(),
        JournalAction::Forget
    );
    let proposed = recovery
        .expected_proposed_state(1, None)
        .expect("forget target must reconstruct from state-only evidence");
    recovery
        .checkpoint_uncertain_success(1, None, &proposed)
        .expect("forget requires no remote identity");
    recovery.resolve().expect("forget recovery must resolve");

    assert!(
        store
            .inspect()
            .unwrap()
            .unwrap()
            .resource(&address("api"))
            .is_none()
    );
}

#[test]
fn legacy_uncertain_evidence_fails_closed_but_safe_terminal_cases_resolve() {
    let (workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().unwrap();
    let mut journal = OperationJournal::begin(&mut session, digest()).unwrap();
    let operation_id = journal.operation_id();
    journal
        .start_step(address("api"), JournalAction::Create)
        .unwrap();
    drop(journal);
    drop(session);
    rewrite_format_version(&journal_path(workspace.path(), operation_id), 1);
    assert!(matches!(
        store.begin_recovery(),
        Err(RecoveryError::UnsafeLegacyEvidence)
    ));

    let (workspace, store, _initial) = initialized_store();
    let path = begin_and_drop(&store, &workspace);
    rewrite_format_version(&path, 1);
    store.begin_recovery().unwrap().resolve().unwrap();

    let (workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().unwrap();
    let mut journal = OperationJournal::begin(&mut session, digest()).unwrap();
    let operation_id = journal.operation_id();
    let token = journal
        .start_step(address("api"), JournalAction::Create)
        .unwrap();
    journal.fail(token, FailureCode::RemoteRejected).unwrap();
    drop(journal);
    drop(session);
    rewrite_format_version(&journal_path(workspace.path(), operation_id), 1);
    store.begin_recovery().unwrap().resolve().unwrap();

    let (workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().unwrap();
    let mut journal = OperationJournal::begin(&mut session, digest()).unwrap();
    let operation_id = journal.operation_id();
    let token = journal
        .start_step(address("api"), JournalAction::Create)
        .unwrap();
    journal
        .fail(token, FailureCode::TransportOutcomeUnknown)
        .unwrap();
    drop(journal);
    drop(session);
    rewrite_format_version(&journal_path(workspace.path(), operation_id), 1);
    assert!(matches!(
        store.begin_recovery(),
        Err(RecoveryError::UnsafeLegacyEvidence)
    ));

    let (workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().unwrap();
    let mut journal = OperationJournal::begin(&mut session, digest()).unwrap();
    let operation_id = journal.operation_id();
    let token = journal
        .start_step(address("api"), JournalAction::Create)
        .unwrap();
    journal
        .succeed(
            token,
            Some(remote_id("remote-api")),
            &next_state(&initial, "api"),
        )
        .unwrap();
    drop(journal);
    drop(session);
    rewrite_format_version(&journal_path(workspace.path(), operation_id), 1);
    store.begin_recovery().unwrap().resolve().unwrap();
}

#[test]
fn truncated_tail_is_archived_before_trim_and_partial_resolution_is_retryable() {
    let (workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().unwrap();
    let mut journal = OperationJournal::begin(&mut session, digest()).unwrap();
    let path = journal_path(workspace.path(), journal.operation_id());
    journal
        .start_recoverable_step(
            address("api"),
            JournalAction::Create,
            ExpectedCheckpoint::create(application_state("placeholder", "public value")).unwrap(),
        )
        .unwrap();
    drop(journal);
    drop(session);
    append(&path, b"{\"type\":\"stepSucceeded\"");
    let original = fs::read(&path).unwrap();

    let recovery = store
        .begin_recovery()
        .expect("tail must be archived and trimmed");
    let archives = fs::read_dir(workspace.path().join(".dokploy/journal/archive"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(archives.len(), 1);
    assert_eq!(fs::read(&archives[0]).unwrap(), original);
    assert!(fs::read(&path).unwrap().ends_with(b"\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(workspace.path().join(".dokploy/journal/archive"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&archives[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    drop(recovery);

    append(&path, b"{\"type\":\"stepSucceeded\"");
    let mut recovery = store
        .begin_recovery()
        .expect("an identical archived tail must be retryable");
    recovery
        .confirm_no_change(1)
        .expect("the open mutation may be proven unchanged");
    drop(recovery);

    append(&path, b"{\"type\":\"recoveryResolved\"");
    store
        .begin_recovery()
        .expect("partial resolution must trim safely")
        .resolve()
        .expect("resolution retry must succeed");
    assert_eq!(store.recovery_status().unwrap(), RecoveryStatus::Clean);
}

#[test]
fn durable_success_precedes_a_failed_checkpoint() {
    let (workspace, store, initial) = initialized_store();
    let primary_path = workspace.path().join(".dokploy/state.json");
    let primary_before = fs::read(&primary_path).expect("state must exist");
    let mut invalid = initial.clone();
    add_application(&mut invalid, "one", "one");
    add_application(&mut invalid, "two", "two");
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("one"), JournalAction::Create)
        .expect("step must start");

    assert!(
        journal
            .succeed(token, Some(remote_id("remote-one")), &invalid)
            .is_err()
    );
    drop(journal);
    drop(session);

    assert_eq!(
        fs::read(primary_path).expect("state must remain"),
        primary_before
    );
    assert!(journal_bytes(workspace.path()).contains("stepSucceeded"));
    assert_eq!(
        recovery_summary(&store).reason(),
        &RecoveryReason::SuccessWithoutCheckpoint
    );
}

#[test]
fn checkpoint_without_commit_is_distinguished_from_unknown_remote_outcome() {
    let (_workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("api"), JournalAction::Create)
        .expect("step must start");
    let proposed = next_state(&initial, "api");
    journal
        .succeed(token, Some(remote_id("remote-api")), &proposed)
        .expect("success must checkpoint");
    drop(journal);
    drop(session);

    let summary = recovery_summary(&store);
    assert_eq!(summary.reason(), &RecoveryReason::CheckpointedWithoutCommit);
    assert_eq!(summary.last_confirmed_sequence(), Some(1));
    assert!(summary.uncertain_step().is_none());
}

#[test]
fn constrained_failure_is_terminal_and_requires_recovery() {
    let (_workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("api"), JournalAction::Deploy)
        .expect("step must start");
    journal
        .fail(token, FailureCode::TransportOutcomeUnknown)
        .expect("failure must be durable");
    drop(journal);
    drop(session);

    assert_eq!(
        recovery_summary(&store).reason(),
        &RecoveryReason::Failed(FailureCode::TransportOutcomeUnknown)
    );
}

#[test]
fn concurrent_steps_checkpoint_successes_that_finish_after_a_sibling_failure() {
    let (_workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let failed = journal
        .start_step(address("api"), JournalAction::Create)
        .expect("first step must start");
    let succeeded = journal
        .start_step(address("worker"), JournalAction::Create)
        .expect("second step must start while the first is in flight");

    journal
        .fail(failed, FailureCode::RemoteRejected)
        .expect("failure must be durable");
    assert!(
        journal
            .start_step(address("later"), JournalAction::Create)
            .is_err(),
        "new work must stop after the first failure"
    );

    let proposed = next_state(&initial, "worker");
    journal
        .succeed(succeeded, Some(remote_id("remote-worker")), &proposed)
        .expect("already-running success must still checkpoint");
    assert!(
        journal.commit().is_err(),
        "a failed operation cannot commit"
    );
    drop(session);

    assert_eq!(
        store
            .inspect()
            .expect("state must remain readable")
            .expect("state must exist")
            .resource(&address("worker"))
            .expect("successful sibling must be durable")
            .remote_id(),
        &remote_id("remote-worker")
    );
    let summary = recovery_summary(&store);
    assert_eq!(
        summary.reason(),
        &RecoveryReason::Failed(FailureCode::RemoteRejected)
    );
    assert_eq!(summary.last_confirmed_sequence(), Some(2));
}

#[test]
fn trailing_fragment_is_recoverable_but_a_complete_malformed_record_is_corrupt() {
    let (workspace, store, _initial) = initialized_store();
    let journal_path = begin_and_drop(&store, &workspace);
    append(&journal_path, b"{unfinished");
    let summary = recovery_summary(&store);
    assert_eq!(summary.reason(), &RecoveryReason::TruncatedTail);
    assert!(summary.has_truncated_tail());

    let (other_workspace, other_store, _initial) = initialized_store();
    let other_path = begin_and_drop(&other_store, &other_workspace);
    append(&other_path, b"{malformed}\n");
    assert!(matches!(
        other_store.recovery_status(),
        Err(StateStoreError::JournalCorrupt)
    ));
}

#[test]
fn scanner_rejects_action_mismatch_and_records_after_commit() {
    let (workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let operation_id = journal.operation_id();
    journal
        .start_step(address("api"), JournalAction::Create)
        .expect("step must start");
    drop(journal);
    drop(session);
    append(
        &journal_path(workspace.path(), operation_id),
        br#"{"type":"stepSucceeded","sequence":1,"address":"application.api","action":"delete","remoteId":null}
"#,
    );
    assert!(matches!(
        store.recovery_status(),
        Err(StateStoreError::JournalCorrupt)
    ));

    let (committed_workspace, committed_store, _initial) = initialized_store();
    let mut session = committed_store.begin_write().expect("writer must start");
    let journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let operation_id = journal.operation_id();
    journal.commit().expect("journal must commit");
    drop(session);
    append(
        &journal_path(committed_workspace.path(), operation_id),
        br#"{"type":"stepStarted","sequence":1,"address":"application.api","action":"update"}
"#,
    );
    assert!(matches!(
        committed_store.recovery_status(),
        Err(StateStoreError::JournalCorrupt)
    ));
}

#[test]
fn two_incomplete_journals_are_ambiguous() {
    let (workspace, store, _initial) = initialized_store();
    let original_path = begin_and_drop(&store, &workspace);
    let original_id = original_path.file_stem().unwrap().to_str().unwrap();
    let second_id = Uuid::new_v4().to_string();
    let bytes = fs::read_to_string(&original_path).expect("journal must be readable");
    let second_bytes = bytes.replace(original_id, &second_id);
    fs::write(
        journal_path(workspace.path(), Uuid::parse_str(&second_id).unwrap()),
        second_bytes,
    )
    .expect("second journal must be written");

    assert!(matches!(
        store.recovery_status(),
        Err(StateStoreError::RecoveryAmbiguous)
    ));
    assert!(matches!(
        store.begin_write(),
        Err(StateStoreError::RecoveryAmbiguous)
    ));
}

#[test]
fn journal_never_serializes_managed_input_values() {
    const CANARY: &str = "journal-secret-canary-9f37";
    let (workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("api"), JournalAction::Create)
        .expect("step must start");
    let mut proposed = initial;
    add_application(&mut proposed, "api", CANARY);
    journal
        .succeed(token, Some(remote_id("remote-api")), &proposed)
        .expect("success must checkpoint");
    drop(journal);
    drop(session);

    let bytes = journal_bytes(workspace.path());
    assert!(!bytes.contains(CANARY));
    assert!(!format!("{:?}", recovery_summary(&store)).contains(CANARY));
}

#[test]
fn plan_digest_accepts_only_canonical_sha256_hex() {
    assert!(PlanDigest::parse("a".repeat(64)).is_ok());
    assert!(PlanDigest::parse("A".repeat(64)).is_err());
    assert!(PlanDigest::parse("a".repeat(63)).is_err());
    assert!(PlanDigest::parse(format!("{}g", "a".repeat(63))).is_err());
}

#[test]
fn success_remote_id_rules_fail_before_appending_success() {
    let (workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("api"), JournalAction::Create)
        .expect("step must start");
    let before = journal_bytes(workspace.path());
    assert!(
        journal
            .succeed(token.clone(), None, &next_state(&initial, "api"))
            .is_err()
    );
    assert_eq!(journal_bytes(workspace.path()), before);
    journal
        .fail(token, FailureCode::Internal)
        .expect("test journal must terminate");
    drop(journal);
    drop(session);

    let (workspace, store, initial) = initialized_store();
    let with_resource = next_state(&initial, "api");
    store
        .begin_write()
        .expect("writer must start")
        .checkpoint(ExpectedState::from_state(&initial), &with_resource)
        .expect("resource must checkpoint");
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("api"), JournalAction::Delete)
        .expect("step must start");
    let mut proposed = with_resource;
    proposed
        .remove_resource(&address("api"))
        .expect("resource must be removed");
    let before = journal_bytes(workspace.path());
    assert!(
        journal
            .succeed(token.clone(), Some(remote_id("unexpected")), &proposed)
            .is_err()
    );
    assert_eq!(journal_bytes(workspace.path()), before);
    journal
        .fail(token, FailureCode::Internal)
        .expect("test journal must terminate");
}

#[test]
fn success_rejects_wrong_address_remote_id_and_action_transition_after_recording_evidence() {
    let (workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("api"), JournalAction::Create)
        .expect("step must start");
    let unrelated = next_state(&initial, "worker");
    let error = journal
        .succeed(token, Some(remote_id("remote-api")), &unrelated)
        .expect_err("an unrelated transition must fail");
    assert!(format!("{error}").contains("transition"));
    drop(journal);
    drop(session);
    assert_eq!(
        store.inspect().expect("state must remain readable"),
        Some(initial)
    );
    assert!(journal_bytes(workspace.path()).contains("stepSucceeded"));

    let (_workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("api"), JournalAction::Create)
        .expect("step must start");
    let proposed = next_state(&initial, "api");
    assert!(
        journal
            .succeed(token, Some(remote_id("wrong-remote")), &proposed)
            .is_err()
    );
    drop(journal);
    drop(session);
    assert_eq!(
        store.inspect().expect("state must remain readable"),
        Some(initial)
    );

    let (_workspace, store, initial) = initialized_store();
    let current = next_state(&initial, "api");
    store
        .begin_write()
        .expect("writer must start")
        .checkpoint(ExpectedState::from_state(&initial), &current)
        .expect("resource must checkpoint");
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let token = journal
        .start_step(address("missing"), JournalAction::Delete)
        .expect("step must start");
    let mut proposed = current.clone();
    proposed
        .remove_resource(&address("api"))
        .expect("resource must be removed");
    assert!(journal.succeed(token, None, &proposed).is_err());
    drop(journal);
    drop(session);
    assert_eq!(
        store.inspect().expect("state must remain readable"),
        Some(current)
    );
}

#[test]
fn scanner_preserves_truncated_evidence_after_complete_or_partial_failure_records() {
    let (workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let operation_id = journal.operation_id();
    let token = journal
        .start_step(address("api"), JournalAction::Update)
        .expect("step must start");
    journal
        .fail(token, FailureCode::Validation)
        .expect("failure must be recorded");
    drop(journal);
    drop(session);
    append(
        &journal_path(workspace.path(), operation_id),
        b"{trailing fragment",
    );
    let summary = recovery_summary(&store);
    assert_eq!(summary.reason(), &RecoveryReason::TruncatedTail);
    store
        .begin_recovery()
        .expect("definitive failure may trim its archived tail")
        .resolve()
        .expect("definitive failure may resolve");

    let (workspace, store, _initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let mut journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let operation_id = journal.operation_id();
    journal
        .start_step(address("api"), JournalAction::Update)
        .expect("step must start");
    drop(journal);
    drop(session);
    append(
        &journal_path(workspace.path(), operation_id),
        b"{\"type\":\"stepFailed\"",
    );
    let summary = recovery_summary(&store);
    assert_eq!(summary.reason(), &RecoveryReason::TruncatedTail);
    assert_eq!(
        summary
            .uncertain_step()
            .expect("open step must remain uncertain")
            .address(),
        &address("api")
    );
}

#[test]
fn commit_refuses_a_non_cooperating_state_revision_change() {
    let (workspace, store, initial) = initialized_store();
    let mut session = store.begin_write().expect("writer must start");
    let journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let externally_written = next_state(&initial, "outside");
    write_state_directly(workspace.path(), &externally_written);

    assert!(journal.commit().is_err());
    assert!(!journal_bytes(workspace.path()).contains("\"type\":\"commit\""));
    drop(session);
    assert_eq!(
        store.inspect().expect("external state must be readable"),
        Some(externally_written)
    );
}

#[test]
fn scanner_rejects_sequence_gap_duplicate_or_missing_begin() {
    let (workspace, store, _initial) = initialized_store();
    let path = begin_and_drop(&store, &workspace);
    append(
        &path,
        b"{\"type\":\"stepStarted\",\"sequence\":2,\"address\":\"application.api\",\"action\":\"update\"}\n",
    );
    assert_journal_corrupt(&store);

    let (workspace, store, _initial) = initialized_store();
    let path = begin_and_drop(&store, &workspace);
    let begin = fs::read(&path).expect("Begin must be readable");
    append(&path, &begin);
    assert_journal_corrupt(&store);

    let (workspace, store, _initial) = initialized_store();
    let directory = workspace.path().join(".dokploy/journal");
    fs::create_dir(&directory).expect("journal directory must be created");
    fs::write(
        directory.join(format!("{}.jsonl", Uuid::new_v4())),
        b"{\"type\":\"stepStarted\",\"sequence\":1,\"address\":\"application.api\",\"action\":\"update\"}\n",
    )
    .expect("journal must be written");
    assert_journal_corrupt(&store);
}

#[test]
fn scanner_rejects_filename_mismatch_unknown_fields_and_unknown_types() {
    let (workspace, store, _initial) = initialized_store();
    let path = begin_and_drop(&store, &workspace);
    fs::rename(&path, journal_path(workspace.path(), Uuid::new_v4()))
        .expect("journal must be renamed");
    assert_journal_corrupt(&store);

    let (workspace, store, _initial) = initialized_store();
    let path = begin_and_drop(&store, &workspace);
    let mut begin: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("Begin must be readable"))
            .expect("Begin must be JSON");
    begin["unexpected"] = json!(true);
    fs::write(&path, format!("{}\n", begin)).expect("unknown field must be written");
    assert_journal_corrupt(&store);

    let (workspace, store, _initial) = initialized_store();
    let path = begin_and_drop(&store, &workspace);
    append(&path, b"{\"type\":\"futureRecord\"}\n");
    assert_journal_corrupt(&store);
}

#[test]
fn scanner_enforces_byte_and_record_limits() {
    let (workspace, store, _initial) = initialized_store();
    let path = begin_and_drop(&store, &workspace);
    OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("journal must open")
        .set_len(16 * 1024 * 1024 + 1)
        .expect("journal must be enlarged");
    assert_journal_corrupt(&store);

    let (workspace, store, _initial) = initialized_store();
    let path = begin_and_drop(&store, &workspace);
    append(&path, b"{}\n".repeat(10_000).as_slice());
    assert_journal_corrupt(&store);
}

#[cfg(unix)]
#[test]
fn journal_directory_and_file_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let (workspace, store, _initial) = initialized_store();
    let journal_path = begin_and_drop(&store, &workspace);

    assert_eq!(mode(&workspace.path().join(".dokploy/journal")), 0o700);
    assert_eq!(mode(&journal_path), 0o600);

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
}

fn initialized_store() -> (TempDir, StateStore, StateFile) {
    let workspace = tempdir().expect("temporary workspace must be created");
    let store = StateStore::new(workspace.path(), instance()).expect("store must bind");
    let state = StateFile::new(Version::new(0, 1, 0), instance());
    store
        .begin_write()
        .expect("writer must start")
        .checkpoint(ExpectedState::absent(), &state)
        .expect("state must initialize");

    (workspace, store, state)
}

fn begin_and_drop(store: &StateStore, workspace: &TempDir) -> PathBuf {
    let mut session = store.begin_write().expect("writer must start");
    let journal = OperationJournal::begin(&mut session, digest()).expect("journal must begin");
    let path = journal_path(workspace.path(), journal.operation_id());
    drop(journal);
    drop(session);
    path
}

fn recovery_summary(store: &StateStore) -> dokploy_state::RecoverySummary {
    match store.recovery_status().expect("scan must succeed") {
        RecoveryStatus::RecoveryRequired(summary) => *summary,
        RecoveryStatus::Clean => panic!("recovery must be required"),
    }
}

fn assert_journal_corrupt(store: &StateStore) {
    assert!(matches!(
        store.recovery_status(),
        Err(StateStoreError::JournalCorrupt)
    ));
}

fn digest() -> PlanDigest {
    PlanDigest::parse("a".repeat(64)).expect("digest must be valid")
}

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://deploy.example.com").expect("instance must be valid")
}

fn address(name: &str) -> ResourceAddress {
    format!("application.{name}")
        .parse()
        .expect("address must parse")
}

fn remote_id(value: &str) -> RemoteId {
    RemoteId::new(value).expect("remote ID must be valid")
}

fn next_state(initial: &StateFile, name: &str) -> StateFile {
    let mut state = initial.clone();
    add_application(&mut state, name, "public value");
    state
}

fn add_application(state: &mut StateFile, name: &str, description: &str) {
    state
        .upsert_resource(
            address(name),
            application_state(&format!("remote-{name}"), description),
        )
        .expect("state must mutate");
}

fn application_state(remote: &str, description: &str) -> ResourceState {
    ResourceState::new(
        ResourceKind::Application,
        remote_id(remote),
        false,
        ManagedInputs::try_from_json(json!({ "description": description }))
            .expect("managed inputs must be safe"),
        Some(
            "environment.production"
                .parse()
                .expect("containment must parse"),
        ),
        Vec::new(),
    )
}

fn journal_path(workspace: &Path, operation_id: Uuid) -> PathBuf {
    workspace
        .join(".dokploy/journal")
        .join(format!("{operation_id}.jsonl"))
}

fn journal_bytes(workspace: &Path) -> String {
    let path = fs::read_dir(workspace.join(".dokploy/journal"))
        .expect("journal directory must exist")
        .next()
        .expect("journal file must exist")
        .expect("journal entry must be readable")
        .path();
    fs::read_to_string(path).expect("journal must be UTF-8 JSON")
}

fn append(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .expect("journal must open")
        .write_all(bytes)
        .expect("journal bytes must append");
}

fn rewrite_format_version(path: &Path, version: u32) {
    let bytes = fs::read_to_string(path).expect("journal must be readable");
    fs::write(
        path,
        bytes.replacen(
            "\"formatVersion\":2",
            &format!("\"formatVersion\":{version}"),
            1,
        ),
    )
    .expect("journal format must be rewritten");
}

fn write_state_directly(workspace: &Path, state: &StateFile) {
    let mut bytes = serde_json::to_vec_pretty(state).expect("state must serialize");
    bytes.push(b'\n');
    fs::write(workspace.join(".dokploy/state.json"), bytes)
        .expect("non-cooperating writer must replace state");
}
