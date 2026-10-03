//! How the planner treats the properties of one resource: ownership, three-way comparison,
//! secrets, and what it refuses to conclude.

mod support;

use dokploy_core::{
    ChangeKind, ChangeOrigin, CheckpointValueRef, DriftKind, OwnedValue, PlanDiagnosticCode,
    PropertyObservation, RemoteFailureKind, RemoteObservation, ValueState, plan,
};
use serde_json::json;
use support::*;

#[test]
fn a_matching_secret_receipt_is_stable_while_the_remote_value_is_write_only() {
    let plan = plan(
        &wanted(vec![want("cache.main").prop("password", secret(0xa5))]),
        &stored(vec![
            have("cache.main", "c-1", json!({})).secret("password", 0xa5),
        ]),
        &remote(vec![there(
            "cache.main",
            "c-1",
            vec![("password", unreadable())],
        )]),
    );

    assert!(plan.complete() && plan.applyable(), "{plan:?}");
    assert!(plan.changes().is_empty() && plan.drift().is_empty());
    assert!(plan.diagnostics().is_empty());
}

#[test]
fn a_different_receipt_is_a_configuration_change_and_never_shows_the_key() {
    let rotated = OwnedValue::Sensitive(dokploy_core::SensitiveIntent::from_fingerprint(
        dokploy_state::SensitiveFingerprint::new_v1(key_id(2), [0xa5; 32]),
    ));
    let plan = plan(
        &wanted(vec![want("cache.main").prop("password", rotated)]),
        &stored(vec![
            have("cache.main", "c-1", json!({})).secret("password", 0xa5),
        ]),
        &remote(vec![there(
            "cache.main",
            "c-1",
            vec![("password", unreadable())],
        )]),
    );

    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(plan.changes()[0].origin(), ChangeOrigin::Config);
    assert_eq!(
        plan.changes()[0].fields()[0].desired(),
        ValueState::Sensitive
    );
    assert_eq!(plan.changes()[0].fields()[0].remote(), ValueState::Unknown);
    let shown = format!("{}{plan:?}", json_of(&plan));
    assert!(!shown.contains("11111111"), "the key id leaked: {shown}");
}

#[test]
fn a_new_secret_is_part_of_a_create_and_of_an_update_without_exposing_its_receipt() {
    let created = plan(
        &wanted(vec![want("cache.main").prop("password", secret(0xb6))]),
        &nothing_stored(),
        &remote(vec![missing("cache.main")]),
    );
    assert_eq!(created.changes()[0].kind(), ChangeKind::Create);
    assert_eq!(
        created.changes()[0].fields()[0].desired(),
        ValueState::Sensitive
    );
    assert_eq!(created.changes()[0].fields()[0].remote(), ValueState::Null);
    assert!(!json_of(&created).contains("b6b6"));

    let updated = plan(
        &wanted(vec![want("cache.main").prop("password", secret(0xb6))]),
        &stored(vec![have("cache.main", "c-1", json!({}))]),
        &remote(vec![there(
            "cache.main",
            "c-1",
            vec![("password", unreadable())],
        )]),
    );
    assert_eq!(updated.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(
        updated.changes()[0].fields()[0].stored(),
        ValueState::Unmanaged
    );
    assert!(matches!(
        updated.changes()[0]
            .checkpoint()
            .present()
            .unwrap()
            .property(&path("cache", "password")),
        Some(CheckpointValueRef::Sensitive)
    ));
}

#[test]
fn a_secret_conclusively_absent_remotely_is_drift_to_restore() {
    let plan = plan(
        &wanted(vec![want("cache.main").prop("password", secret(0xa5))]),
        &stored(vec![
            have("cache.main", "c-1", json!({})).secret("password", 0xa5),
        ]),
        &remote(vec![there(
            "cache.main",
            "c-1",
            vec![("password", PropertyObservation::KnownAbsent)],
        )]),
    );

    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(plan.changes()[0].origin(), ChangeOrigin::Drift);
    assert_eq!(plan.changes()[0].fields()[0].remote(), ValueState::Null);
    assert_eq!(plan.drift()[0].properties(), &[path("cache", "password")]);
}

#[test]
fn clearing_a_secret_is_distinct_from_having_one_and_stable_once_recorded() {
    let clear = plan(
        &wanted(vec![want("cache.main").prop("password", OwnedValue::Null)]),
        &stored(vec![
            have("cache.main", "c-1", json!({})).secret("password", 0xa5),
        ]),
        &remote(vec![there(
            "cache.main",
            "c-1",
            vec![("password", unreadable())],
        )]),
    );
    assert_eq!(clear.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(
        clear.changes()[0].fields()[0].stored(),
        ValueState::Sensitive
    );
    assert_eq!(clear.changes()[0].fields()[0].desired(), ValueState::Null);

    // Once the clear is recorded, state holds an explicit null and nothing is left to do.
    let stable = plan(
        &wanted(vec![want("cache.main").prop("password", OwnedValue::Null)]),
        &stored(vec![have("cache.main", "c-1", json!({"password": null}))]),
        &remote(vec![there(
            "cache.main",
            "c-1",
            vec![("password", unreadable())],
        )]),
    );
    assert!(
        stable.changes().is_empty() && stable.drift().is_empty(),
        "{stable:?}"
    );
}

#[test]
fn an_omitted_property_is_relinquished_without_a_remote_update_or_drift() {
    let plan = plan(
        &wanted(vec![want("widget.main").prop("replicas", val(json!(1)))]),
        &stored(vec![have(
            "widget.main",
            "w-1",
            json!({"description": "old", "replicas": 1}),
        )]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("replicas", known(json!(1)))],
        )]),
    );

    assert!(plan.complete() && plan.applyable() && plan.drift().is_empty());
    let change = &plan.changes()[0];
    assert_eq!(change.kind(), ChangeKind::NoOp);
    assert_eq!(change.origin(), ChangeOrigin::Config);
    assert_eq!(field_names(change), ["description"]);
    let checkpoint = change.checkpoint().present().expect("still managed");
    assert!(
        checkpoint
            .property(&path("widget", "description"))
            .is_none()
    );
    assert!(checkpoint.property(&path("widget", "replicas")).is_some());
}

#[test]
fn omitting_one_member_or_entry_relinquishes_only_that_one() {
    let member = plan(
        &wanted(vec![
            want("widget.main").prop("limits.cpu", val(json!("1"))),
        ]),
        &stored(vec![have(
            "widget.main",
            "w-1",
            json!({"limits": {"cpu": "1", "memory": "2g"}}),
        )]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("limits.cpu", known(json!("1")))],
        )]),
    );
    assert_eq!(member.changes()[0].kind(), ChangeKind::NoOp);
    assert_eq!(field_names(&member.changes()[0]), ["limits.memory"]);

    // Environment values are secret by default, so an entry is owned through its receipt.
    let entry = plan(
        &wanted(vec![want("widget.main").prop("environment.LOG", secret(1))]),
        &stored(vec![
            have("widget.main", "w-1", json!({}))
                .secret("environment.LOG", 1)
                .secret("environment.DEBUG", 2),
        ]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("environment.LOG", unreadable())],
        )]),
    );
    assert_eq!(entry.changes()[0].kind(), ChangeKind::NoOp);
    assert_eq!(field_names(&entry.changes()[0]), ["environment.DEBUG"]);
    let checkpoint = entry.changes()[0].checkpoint().present().unwrap();
    assert!(
        checkpoint
            .property(&path("widget", "environment.LOG"))
            .is_some()
    );
    assert!(
        checkpoint
            .property(&path("widget", "environment.DEBUG"))
            .is_none()
    );
}

#[test]
fn an_explicit_null_is_owned_and_is_not_an_omitted_property() {
    let owned = plan(
        &wanted(vec![
            want("widget.main").prop("description", OwnedValue::Null),
        ]),
        &stored(vec![have(
            "widget.main",
            "w-1",
            json!({"description": "x"}),
        )]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("description", known(json!("x")))],
        )]),
    );
    assert_eq!(owned.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(owned.changes()[0].fields()[0].desired(), ValueState::Null);

    let omitted = plan(
        &wanted(vec![want("widget.main")]),
        &stored(vec![have(
            "widget.main",
            "w-1",
            json!({"description": "x"}),
        )]),
        &remote(vec![there("widget.main", "w-1", vec![])]),
    );
    assert_eq!(omitted.changes()[0].kind(), ChangeKind::NoOp);
    assert_eq!(
        omitted.changes()[0].fields()[0].desired(),
        ValueState::Unmanaged
    );
}

#[test]
fn the_three_way_comparison_keeps_config_drift_and_both_apart() {
    // (stored, desired, remote) -> what the plan says.
    let cases = [
        (1, 2, 1, ChangeKind::Update, ChangeOrigin::Config, false),
        (1, 1, 2, ChangeKind::Update, ChangeOrigin::Drift, true),
        (
            1,
            2,
            3,
            ChangeKind::Update,
            ChangeOrigin::ConfigAndDrift,
            true,
        ),
        (
            1,
            2,
            2,
            ChangeKind::NoOp,
            ChangeOrigin::ConfigAndDrift,
            true,
        ),
    ];
    for (stored_value, desired_value, remote_value, kind, origin, drifted) in cases {
        let plan = plan(
            &wanted(vec![
                want("widget.main").prop("replicas", val(json!(desired_value))),
            ]),
            &stored(vec![have(
                "widget.main",
                "w-1",
                json!({"replicas": stored_value}),
            )]),
            &remote(vec![there(
                "widget.main",
                "w-1",
                vec![("replicas", known(json!(remote_value)))],
            )]),
        );
        assert_eq!(
            plan.changes()[0].kind(),
            kind,
            "{stored_value} {desired_value} {remote_value}"
        );
        assert_eq!(plan.changes()[0].origin(), origin);
        assert_eq!(!plan.drift().is_empty(), drifted);
    }
}

#[test]
fn a_property_the_remote_did_not_return_makes_the_plan_incomplete() {
    let unreturned = plan(
        &wanted(vec![want("widget.main").prop("replicas", val(json!(2)))]),
        &stored(vec![have("widget.main", "w-1", json!({"replicas": 1}))]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("replicas", not_returned())],
        )]),
    );
    assert!(!unreturned.complete() && !unreturned.applyable());
    assert!(unreturned.changes().is_empty());
    assert_eq!(
        unreturned.diagnostics()[0].code(),
        PlanDiagnosticCode::UnknownPropertyObservation
    );

    let unobserved = plan(
        &wanted(vec![want("cache.main").prop("password", secret(0xa5))]),
        &stored(vec![
            have("cache.main", "c-1", json!({})).secret("password", 0xa5),
        ]),
        &remote(vec![there("cache.main", "c-1", vec![])]),
    );
    assert!(!unobserved.complete() && unobserved.changes().is_empty());
    assert_eq!(
        unobserved.diagnostics()[0].code(),
        PlanDiagnosticCode::MissingPropertyObservation
    );
    assert_eq!(
        unobserved.diagnostics()[0].property(),
        Some(&path("cache", "password"))
    );
}

#[test]
fn a_collection_root_is_cleared_or_declared_empty_explicitly() {
    let clear = plan(
        &wanted(vec![
            want("widget.main").prop("environment", OwnedValue::Null),
        ]),
        &stored(vec![
            have("widget.main", "w-1", json!({})).secret("environment.LOG", 1),
        ]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("environment", known(json!({"LOG": "x"})))],
        )]),
    );
    assert!(clear.complete(), "{clear:?}");
    assert_eq!(clear.changes()[0].kind(), ChangeKind::Update);
    assert!(matches!(
        clear.changes()[0]
            .checkpoint()
            .present()
            .unwrap()
            .property(&path("widget", "environment")),
        Some(CheckpointValueRef::Null)
    ));

    let empty = plan(
        &wanted(vec![
            want("widget.main").prop("environment", OwnedValue::EmptyCollection),
        ]),
        &stored(vec![have(
            "widget.main",
            "w-1",
            json!({"environment": null}),
        )]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("environment", known(json!({})))],
        )]),
    );
    // The remote is already empty: only the declaration is recorded.
    assert_eq!(empty.changes()[0].kind(), ChangeKind::NoOp);
    assert!(matches!(
        empty.changes()[0]
            .checkpoint()
            .present()
            .unwrap()
            .property(&path("widget", "environment")),
        Some(CheckpointValueRef::EmptyCollection)
    ));
}

#[test]
fn declaring_a_collection_empty_updates_a_remote_that_still_has_entries() {
    let plan = plan(
        &wanted(vec![
            want("widget.main").prop("environment", OwnedValue::EmptyCollection),
        ]),
        &stored(vec![have(
            "widget.main",
            "w-1",
            json!({"environment": null}),
        )]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("environment", known(json!({"LOG": "x"})))],
        )]),
    );
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(plan.changes()[0].origin(), ChangeOrigin::ConfigAndDrift);
}

#[test]
fn a_resource_is_created_only_after_explicit_remote_absence() {
    let desired = wanted(vec![creatable("widget.main")]);

    let unobserved = plan(
        &desired,
        &nothing_stored(),
        &remote(vec![(
            addr("widget.main"),
            RemoteObservation::Unavailable(RemoteFailureKind::Unavailable),
        )]),
    );
    assert!(!unobserved.complete() && unobserved.changes().is_empty());

    let created = plan(
        &desired,
        &nothing_stored(),
        &remote(vec![missing("widget.main")]),
    );
    assert!(created.complete());
    assert_eq!(created.changes()[0].kind(), ChangeKind::Create);
    assert_eq!(created.changes()[0].origin(), ChangeOrigin::Config);
    assert!(created.changes()[0].checkpoint().present().is_some());
}

#[test]
fn a_managed_resource_deleted_remotely_is_recreated_or_forgotten_by_whether_it_is_still_wanted() {
    let existing = || {
        stored(vec![
            have("widget.main", "w-1", json!({"flavor": "small"})).secret("token", 7),
        ])
    };

    let recreate = plan(
        &wanted(vec![creatable("widget.main")]),
        &existing(),
        &remote(vec![missing("widget.main")]),
    );
    assert_eq!(recreate.changes()[0].kind(), ChangeKind::Create);
    assert_eq!(recreate.changes()[0].origin(), ChangeOrigin::Drift);
    assert_eq!(recreate.drift()[0].kind(), DriftKind::Deleted);

    let forget = plan(
        &wanted(vec![]),
        &existing(),
        &remote(vec![missing("widget.main")]),
    );
    assert_eq!(forget.changes()[0].kind(), ChangeKind::Forget);
    assert!(forget.changes()[0].checkpoint().is_absent());
}

#[test]
fn a_different_resource_at_the_recorded_address_or_instance_fails_closed() {
    let desired = wanted(vec![want("widget.main").prop("replicas", val(json!(1)))]);
    let existing = || stored(vec![have("widget.main", "w-1", json!({"replicas": 1}))]);

    let other_id = plan(
        &desired,
        &existing(),
        &remote(vec![there(
            "widget.main",
            "w-2",
            vec![("replicas", known(json!(1)))],
        )]),
    );
    assert_eq!(
        other_id.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteIdentityMismatch
    );
    assert!(other_id.changes().is_empty());

    let unavailable = plan(
        &desired,
        &existing(),
        &remote(vec![(
            addr("widget.main"),
            RemoteObservation::Unavailable(RemoteFailureKind::Unauthorized),
        )]),
    );
    assert!(!unavailable.complete());
    assert_eq!(
        unavailable.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteUnavailable
    );
    assert_eq!(
        unavailable.diagnostics()[0].remote_failure(),
        Some(RemoteFailureKind::Unauthorized)
    );
}

#[test]
fn an_existing_unmanaged_resource_blocks_its_address_and_other_remote_resources_are_ignored() {
    let plan = plan(
        &wanted(vec![
            want("widget.main").prop("flavor", val(json!("small"))),
        ]),
        &nothing_stored(),
        &remote(vec![
            there(
                "widget.main",
                "w-1",
                vec![("flavor", known(json!("small")))],
            ),
            there("widget.other", "w-9", vec![]),
        ]),
    );

    assert!(!plan.applyable());
    assert!(plan.changes().is_empty() && plan.drift().is_empty());
    assert_eq!(plan.diagnostics().len(), 1);
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::UnmanagedAddressCollision
    );
}

#[test]
fn a_no_op_checkpoint_is_exact_and_the_plan_never_contains_values_or_identities() {
    let canary = "desired-checkpoint-canary";
    let desired = wanted(vec![
        want("widget.main")
            .prop("description", val(json!(canary)))
            .protected(true),
    ]);
    let observed = remote(vec![there(
        "widget.main",
        "remote-id-canary",
        vec![("description", known(json!(canary)))],
    )]);
    let existing = stored(vec![have(
        "widget.main",
        "remote-id-canary",
        json!({"description": canary}),
    )]);

    let first = plan(&desired, &existing, &observed);
    let second = plan(&desired, &existing, &observed);

    let change = &first.changes()[0];
    assert_eq!(
        change.kind(),
        ChangeKind::NoOp,
        "protection is a state-only checkpoint"
    );
    let checkpoint = change.checkpoint().present().unwrap();
    assert!(checkpoint.protected());
    assert!(matches!(
        checkpoint.property(&path("widget", "description")),
        Some(CheckpointValueRef::NonSensitive(value)) if value == &json!(canary)
    ));
    assert_eq!(
        first.to_json_bytes(),
        second.to_json_bytes(),
        "planning is deterministic"
    );
    let shown = format!("{}{first:?}", json_of(&first));
    assert!(
        !shown.contains(canary) && !shown.contains("remote-id-canary"),
        "{shown}"
    );
}

#[test]
fn diagnostic_codes_and_the_plan_envelope_are_stable() {
    use PlanDiagnosticCode::*;
    let codes = [
        (InstanceMismatch, "DOKPLAN001"),
        (MissingObservation, "DOKPLAN002"),
        (RemoteUnavailable, "DOKPLAN003"),
        (RemoteIdentityMismatch, "DOKPLAN004"),
        (UnmanagedAddressCollision, "DOKPLAN005"),
        (ProtectedDelete, "DOKPLAN006"),
        (UnsupportedDirective, "DOKPLAN007"),
        (MissingPropertyObservation, "DOKPLAN008"),
        (UnknownPropertyObservation, "DOKPLAN009"),
        (DesiredDependencyCycle, "DOKPLAN010"),
        (StoredDependencyCycle, "DOKPLAN011"),
        (InvalidMoveDirective, "DOKPLAN012"),
        (InvalidRemovalDirective, "DOKPLAN013"),
        (MoveSourceMissing, "DOKPLAN014"),
        (MoveTargetCollision, "DOKPLAN015"),
        (UnsupportedMutation, "DOKPLAN017"),
        (MissingCreateProperty, "DOKPLAN018"),
        (UnresolvedExternalSelector, "DOKPLAN019"),
    ];
    for (code, text) in codes {
        assert_eq!(code.as_str(), text);
    }

    let desired = wanted(vec![]);
    let existing = nothing_stored();
    let empty = plan(&desired, &existing, &remote(vec![]));
    assert_eq!(empty.format_version(), 1);
    assert_eq!(empty.lineage(), existing.lineage());
    assert_eq!(empty.state_serial(), existing.serial());
    assert_eq!(empty.config_digest(), desired.digest());
}
