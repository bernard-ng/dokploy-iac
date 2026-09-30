use std::collections::BTreeMap;
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use dokploy_cli::saved_plan::{SavedPlan, SavedPlanError, read, write_new};
use dokploy_core::{ConfigDigest, DesiredState, RemoteState, StoredState, plan};
use dokploy_state::{InstanceIdentity, StateFile};
use semver::Version;

#[test]
fn saved_plan_round_trip_rebinds_fresh_evidence_and_is_owner_only() {
    let (instance, plan) = empty_plan();
    let document = SavedPlan::from_fresh_plan(instance.clone(), &plan, [7; 32])
        .expect("complete plan can be saved");
    let directory = tempfile::tempdir().expect("temporary directory is available");
    let path = directory.path().join("plan.json");

    write_new(&path, &document).expect("saved plan is written");
    let loaded = read(&path).expect("saved plan is readable");

    loaded
        .verify_fresh(&instance, &plan, [7; 32])
        .expect("fresh evidence matches");
    assert!(matches!(
        loaded.verify_fresh(&instance, &plan, [8; 32]),
        Err(SavedPlanError::StaleEvidence)
    ));
    assert!(matches!(
        write_new(&path, &document),
        Err(SavedPlanError::AlreadyExists)
    ));
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&path)
            .expect("saved plan metadata is readable")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn saved_plan_reader_rejects_unknown_duplicate_and_tampered_fields() {
    let (instance, plan) = empty_plan();
    let document =
        SavedPlan::from_fresh_plan(instance, &plan, [7; 32]).expect("complete plan can be saved");
    let directory = tempfile::tempdir().expect("temporary directory is available");
    let valid_path = directory.path().join("valid.json");
    write_new(&valid_path, &document).expect("saved plan is written");
    let valid = fs::read_to_string(&valid_path).expect("saved plan is readable");

    let duplicate_path = directory.path().join("duplicate.json");
    fs::write(
        &duplicate_path,
        valid.replacen('{', "{\"formatVersion\":1,", 1),
    )
    .expect("duplicate fixture is writable");
    assert!(matches!(
        read(&duplicate_path),
        Err(SavedPlanError::InvalidDocument)
    ));

    let mut tampered: serde_json::Value =
        serde_json::from_str(&valid).expect("valid document parses");
    tampered["plan"]["stateSerial"] = serde_json::json!(99);
    let tampered_path = directory.path().join("tampered.json");
    fs::write(
        &tampered_path,
        serde_json::to_vec(&tampered).expect("tampered fixture serializes"),
    )
    .expect("tampered fixture is writable");
    assert!(matches!(
        read(&tampered_path),
        Err(SavedPlanError::InvalidDocument)
    ));

    let mut unknown: serde_json::Value =
        serde_json::from_str(&valid).expect("valid document parses");
    unknown["unexpected"] = serde_json::json!(true);
    let unknown_path = directory.path().join("unknown.json");
    fs::write(
        &unknown_path,
        serde_json::to_vec(&unknown).expect("unknown fixture serializes"),
    )
    .expect("unknown fixture is writable");
    assert!(matches!(
        read(&unknown_path),
        Err(SavedPlanError::InvalidDocument)
    ));
}

#[test]
fn saved_plan_rejects_each_stale_execution_boundary() {
    let instance =
        InstanceIdentity::parse("https://deploy.example.test").expect("instance is valid");
    let state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    let original = empty_plan_for_state(&instance, "a", &state);
    let document = SavedPlan::from_fresh_plan(instance.clone(), &original, [7; 32])
        .expect("complete plan can be saved");

    let other_instance =
        InstanceIdentity::parse("https://other.example.test").expect("instance is valid");
    assert_stale(document.verify_fresh(&other_instance, &original, [7; 32]));

    let other_lineage = StateFile::new(Version::new(0, 1, 0), instance.clone());
    let other_lineage_plan = empty_plan_for_state(&instance, "a", &other_lineage);
    assert_stale(document.verify_fresh(&instance, &other_lineage_plan, [7; 32]));

    let mut next_serial_json = serde_json::to_value(&state).expect("state serializes");
    next_serial_json["serial"] = serde_json::json!(1);
    let next_serial = StateFile::from_json_slice(
        &serde_json::to_vec(&next_serial_json).expect("state JSON serializes"),
    )
    .expect("next state serial is valid");
    let next_serial_plan = empty_plan_for_state(&instance, "a", &next_serial);
    assert_stale(document.verify_fresh(&instance, &next_serial_plan, [7; 32]));

    let changed_configuration = empty_plan_for_state(&instance, "b", &state);
    assert_stale(document.verify_fresh(&instance, &changed_configuration, [7; 32]));

    assert_stale(document.verify_fresh(&instance, &original, [8; 32]));
}

fn assert_stale(result: Result<(), SavedPlanError>) {
    assert!(matches!(result, Err(SavedPlanError::StaleEvidence)));
}

fn empty_plan_for_state(
    instance: &InstanceIdentity,
    digest_byte: &str,
    state: &StateFile,
) -> dokploy_core::Plan {
    let desired = DesiredState::try_new(
        ConfigDigest::parse(digest_byte.repeat(64)).expect("digest is valid"),
        BTreeMap::new(),
    )
    .expect("desired state is valid");
    let stored = StoredState::try_from_state(state).expect("stored state is valid");
    let remote = RemoteState::try_new(
        instance.clone(),
        std::iter::empty::<(
            dokploy_state::ResourceAddress,
            dokploy_core::RemoteObservation,
        )>(),
    )
    .expect("remote state is valid");

    plan(&desired, &stored, &remote)
}

fn empty_plan() -> (InstanceIdentity, dokploy_core::Plan) {
    let instance =
        InstanceIdentity::parse("https://deploy.example.test").expect("instance is valid");
    let desired = DesiredState::try_new(
        ConfigDigest::parse("a".repeat(64)).expect("digest is valid"),
        BTreeMap::new(),
    )
    .expect("desired state is valid");
    let stored = StoredState::absent(instance.clone());
    let remote = RemoteState::try_new(
        instance.clone(),
        std::iter::empty::<(
            dokploy_state::ResourceAddress,
            dokploy_core::RemoteObservation,
        )>(),
    )
    .expect("remote state is valid");

    (instance, plan(&desired, &stored, &remote))
}
