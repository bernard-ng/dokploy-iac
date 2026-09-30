use std::collections::BTreeMap;
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use dokploy_cli::saved_plan::{SavedPlan, SavedPlanError, read, write_new};
use dokploy_core::{ConfigDigest, DesiredState, RemoteState, StoredState, plan};
use dokploy_state::InstanceIdentity;

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
