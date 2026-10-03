//! Replacement, the adapter's mutation contract, and `ignore_changes`.

mod support;

use dokploy_core::{
    ChangeKind, ChangeOrigin, CheckpointValueRef, DesiredStateError, DriftKind, MoveAction,
    MoveDirective, MutationContract, MutationMode, PlanDiagnosticCode, PropertyMutation,
    PropertyObservation, ReplacementOrder, UnsupportedDirectiveKind, plan,
};
use serde_json::json;
use support::*;

fn replicas_remote(value: i64) -> dokploy_core::RemoteState {
    remote(vec![there(
        "widget.main",
        "w-1",
        vec![("replicas", known(json!(value)))],
    )])
}

#[test]
fn replacement_orders_an_existing_resource_but_not_its_first_creation() {
    let update = plan(
        &wanted(vec![
            want("widget.main")
                .prop("replicas", val(json!(2)))
                .replacing_on(&["replicas"]),
        ]),
        &stored(vec![have("widget.main", "w-1", json!({"replicas": 1}))]),
        &replicas_remote(1),
    );
    assert!(update.complete() && update.applyable(), "{update:?}");
    assert_eq!(update.changes()[0].kind(), ChangeKind::Replace);
    assert_eq!(
        update.changes()[0].replacement_order(),
        Some(ReplacementOrder::DeleteBeforeCreate)
    );

    let create = plan(
        &wanted(vec![
            creatable("widget.main")
                .prop("replicas", val(json!(2)))
                .replacing_on(&["replicas"]),
        ]),
        &nothing_stored(),
        &remote(vec![missing("widget.main")]),
    );
    assert_eq!(create.changes()[0].kind(), ChangeKind::Create);
    assert!(create.diagnostics().is_empty());
}

#[test]
fn a_create_only_property_that_changes_is_replaced_by_the_spec_alone() {
    let plan = plan(
        &wanted(vec![
            creatable("widget.main").prop("flavor", val(json!("large"))),
        ]),
        &stored(vec![created("widget.main", "w-1")]),
        &remote(vec![alive("widget.main", "w-1")]),
    );
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Replace);
}

#[test]
fn the_mutation_contract_decides_replacement_and_what_a_create_needs() {
    let contract = MutationContract::deny_all(ReplacementOrder::CreateBeforeDelete).with_property(
        path("widget", "replicas"),
        PropertyMutation::new(MutationMode::Replace, MutationMode::Unsupported),
    );
    let replacement = plan(
        &wanted(vec![want("widget.main").prop("replicas", val(json!(2)))]),
        &stored(vec![have("widget.main", "w-1", json!({"replicas": 1}))]),
        &remote_with_contract(
            vec![there(
                "widget.main",
                "w-1",
                vec![("replicas", known(json!(1)))],
            )],
            contract,
        ),
    );
    assert_eq!(replacement.changes()[0].kind(), ChangeKind::Replace);
    assert_eq!(
        replacement.changes()[0].replacement_order(),
        Some(ReplacementOrder::CreateBeforeDelete)
    );

    let requiring = MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
        .requiring(path("widget", "description"));
    let blocked = plan(
        &wanted(vec![want("widget.main")]),
        &nothing_stored(),
        &remote_with_contract(vec![missing("widget.main")], requiring),
    );
    assert!(blocked.changes().is_empty());
    assert_eq!(
        blocked.diagnostics()[0].code(),
        PlanDiagnosticCode::MissingCreateProperty
    );
    assert_eq!(
        blocked.diagnostics()[0].property(),
        Some(&path("widget", "description"))
    );
}

#[test]
fn a_property_allowed_on_create_may_stay_unsupported_afterwards() {
    for contract in [
        MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
            .requiring(path("widget", "description")),
        MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
            .allowing_on_create(path("widget", "description")),
    ] {
        let create = plan(
            &wanted(vec![
                want("widget.main").prop("description", val(json!("on-create"))),
            ]),
            &nothing_stored(),
            &remote_with_contract(vec![missing("widget.main")], contract),
        );
        assert!(create.applyable(), "{create:?}");
        assert_eq!(create.changes()[0].kind(), ChangeKind::Create);
        assert!(create.diagnostics().is_empty());
    }

    // Allowed is not required.
    let optional = plan(
        &wanted(vec![want("widget.main")]),
        &nothing_stored(),
        &remote_with_contract(
            vec![missing("widget.main")],
            MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
                .allowing_on_create(path("widget", "description")),
        ),
    );
    assert_eq!(optional.changes()[0].kind(), ChangeKind::Create);
}

#[test]
fn an_ignored_property_keeps_its_stored_baseline_through_an_unrelated_update() {
    let plan = plan(
        &wanted(vec![
            want("widget.main")
                .prop("description", val(json!("new")))
                .prop("replicas", val(json!(2)))
                .ignoring(&["replicas"]),
        ]),
        &stored(vec![have(
            "widget.main",
            "w-1",
            json!({"description": "old", "replicas": 1}),
        )]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("description", known(json!("old")))],
        )]),
    );

    assert!(plan.complete() && plan.applyable() && plan.drift().is_empty());
    let change = &plan.changes()[0];
    assert_eq!(change.kind(), ChangeKind::Update);
    assert_eq!(change.origin(), ChangeOrigin::Config);
    assert_eq!(field_names(change), ["description"]);
    assert_eq!(change.preserved_paths(), &[path("widget", "replicas")]);
    assert!(matches!(
        change.checkpoint().present().and_then(|t| t.property(&path("widget", "replicas"))),
        Some(CheckpointValueRef::NonSensitive(value)) if value == &json!(1)
    ));
}

#[test]
fn differences_in_an_ignored_property_need_no_observation_change_drift_or_checkpoint() {
    let desired = wanted(vec![
        want("widget.main")
            .prop("replicas", val(json!(2)))
            .ignoring(&["replicas"]),
    ]);
    let existing = stored(vec![have("widget.main", "w-1", json!({"replicas": 1}))]);

    for properties in [
        vec![],
        vec![("replicas", PropertyObservation::KnownAbsent)],
        vec![("replicas", not_returned())],
        vec![("replicas", known(json!(9)))],
    ] {
        let ignored = plan(
            &desired,
            &existing,
            &remote(vec![there("widget.main", "w-1", properties)]),
        );
        assert!(ignored.complete() && ignored.applyable());
        assert!(ignored.changes().is_empty() && ignored.drift().is_empty());
    }
}

#[test]
fn recreating_a_deleted_resource_uses_only_the_stored_ignored_ownership() {
    let plan = plan(
        &wanted(vec![
            creatable("widget.main")
                .prop("description", val(json!("new")))
                .prop("replicas", val(json!(2)))
                .prop("limits.cpu", val(json!("1")))
                .ignoring(&["replicas", "limits.cpu"]),
        ]),
        &stored(vec![created_with(
            "widget.main",
            json!({"flavor": "small", "description": "old", "replicas": 1}),
        )]),
        &remote(vec![missing("widget.main")]),
    );

    assert!(plan.applyable(), "{plan:?}");
    let change = &plan.changes()[0];
    assert_eq!(change.kind(), ChangeKind::Create);
    assert_eq!(change.origin(), ChangeOrigin::ConfigAndDrift);
    assert!(change.preserved_paths().is_empty());
    assert!(
        change
            .fields()
            .iter()
            .all(|f| !matches!(f.key().as_str(), "replicas" | "limits.cpu"))
    );
    let checkpoint = change
        .checkpoint()
        .present()
        .expect("a recreate has a checkpoint");
    // What state baselined is kept; what it never owned is not adopted from the document.
    assert!(matches!(
        checkpoint.property(&path("widget", "replicas")),
        Some(CheckpointValueRef::NonSensitive(value)) if value == &json!(1)
    ));
    assert!(checkpoint.property(&path("widget", "limits.cpu")).is_none());
}

fn created_with(address: &str, managed: serde_json::Value) -> Have {
    have(address, "w-1", managed).secret("token", 7)
}

#[test]
fn a_first_creation_uses_the_document_value_and_never_adopts_a_collision() {
    let desired = wanted(vec![
        creatable("widget.main")
            .prop("replicas", val(json!(2)))
            .ignoring(&["replicas"]),
    ]);

    let create = plan(
        &desired,
        &nothing_stored(),
        &remote(vec![missing("widget.main")]),
    );
    assert!(create.applyable());
    let change = &create.changes()[0];
    assert_eq!(change.kind(), ChangeKind::Create);
    assert!(change.preserved_paths().is_empty());
    assert!(matches!(
        change.checkpoint().present().and_then(|t| t.property(&path("widget", "replicas"))),
        Some(CheckpointValueRef::NonSensitive(value)) if value == &json!(2)
    ));

    let collision = plan(
        &desired,
        &nothing_stored(),
        &remote(vec![there("widget.main", "w-1", vec![])]),
    );
    assert!(collision.changes().is_empty());
    assert_eq!(
        collision.diagnostics()[0].code(),
        PlanDiagnosticCode::UnmanagedAddressCollision
    );
}

#[test]
fn unsafe_ignore_metadata_is_refused_when_the_desired_state_is_built() {
    // A secret is owned through its receipt: ignoring it would hide a rotation.
    let secret_ignored = try_wanted(vec![
        want("cache.main")
            .prop("password", secret(0xa5))
            .ignoring(&["password"]),
    ]);
    assert!(matches!(
        secret_ignored,
        Err(DesiredStateError::InvalidIgnoredProperty { .. })
    ));

    // A collection root is a structural intent, not a value.
    let root_ignored = try_wanted(vec![
        want("widget.main")
            .prop("environment", dokploy_core::OwnedValue::Null)
            .ignoring(&["environment"]),
    ]);
    assert!(matches!(
        root_ignored,
        Err(DesiredStateError::InvalidIgnoredProperty { .. })
    ));

    // Ignoring and replacing the same property contradict each other.
    let both = try_wanted(vec![
        want("widget.main")
            .prop("replicas", val(json!(1)))
            .ignoring(&["replicas"])
            .replacing_on(&["replicas"]),
    ]);
    assert!(matches!(
        both,
        Err(DesiredStateError::ConflictingLifecyclePaths { .. })
    ));
}

#[test]
fn a_lifecycle_only_ignore_is_a_write_exclusion_and_never_enters_state() {
    let plan = plan(
        &wanted(vec![
            want("widget.main")
                .prop("replicas", val(json!(2)))
                .ignoring(&["status"]),
        ]),
        &stored(vec![have("widget.main", "w-1", json!({"replicas": 1}))]),
        &replicas_remote(1),
    );

    assert!(plan.complete() && plan.applyable());
    let change = &plan.changes()[0];
    assert_eq!(change.kind(), ChangeKind::Update);
    assert_eq!(change.preserved_paths(), &[path("widget", "status")]);
    assert!(
        change
            .checkpoint()
            .present()
            .is_some_and(|target| target.property(&path("widget", "status")).is_none())
    );
}

#[test]
fn ownership_of_an_ignored_property_comes_only_from_the_stored_baseline() {
    let desired = || {
        wanted(vec![
            want("widget.main")
                .prop("description", val(json!("new")))
                .prop("replicas", val(json!(2)))
                .ignoring(&["replicas"]),
        ])
    };
    let unmanaged = plan(
        &desired(),
        &stored(vec![have(
            "widget.main",
            "w-1",
            json!({"description": "old"}),
        )]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![("description", known(json!("old")))],
        )]),
    );
    let checkpoint = unmanaged.changes()[0].checkpoint().present().unwrap();
    assert!(checkpoint.property(&path("widget", "replicas")).is_none());

    // With only metadata changing, the baseline is carried into the checkpoint untouched.
    let preserved = plan(
        &wanted(vec![
            want("widget.main").protected(true).ignoring(&["replicas"]),
        ]),
        &stored(vec![have("widget.main", "w-1", json!({"replicas": 1}))]),
        &remote(vec![there("widget.main", "w-1", vec![])]),
    );
    let change = &preserved.changes()[0];
    assert!(matches!(
        change.checkpoint().present().unwrap().property(&path("widget", "replicas")),
        Some(CheckpointValueRef::NonSensitive(value)) if value == &json!(1)
    ));
    assert_eq!(change.preserved_paths(), &[path("widget", "replicas")]);
}

#[test]
fn a_pending_move_keeps_the_ignored_source_baseline_without_a_remote_write() {
    let plan = plan(
        &wanted(vec![
            want("widget.new")
                .prop("replicas", val(json!(2)))
                .ignoring(&["replicas"]),
        ])
        .with_moves(vec![MoveDirective::new(
            addr("widget.old"),
            addr("widget.new"),
        )]),
        &stored(vec![
            have("widget.old", "w-1", json!({"replicas": 1})).protected(),
        ]),
        &remote(vec![
            there("widget.old", "w-1", vec![]),
            missing("widget.new"),
        ]),
    );

    assert!(plan.complete() && plan.applyable() && plan.drift().is_empty());
    let change = &plan.changes()[0];
    assert_eq!(change.kind(), ChangeKind::Move);
    assert_eq!(change.move_action(), Some(MoveAction::StateOnly));
    assert!(change.fields().is_empty());
    assert_eq!(change.preserved_paths(), &[path("widget", "replicas")]);
    assert!(matches!(
        change.checkpoint().move_target().and_then(|t| t.property(&path("widget", "replicas"))),
        Some(CheckpointValueRef::NonSensitive(value)) if value == &json!(1)
    ));
}

#[test]
fn a_satisfied_move_plans_normally_and_keeps_an_ignored_null_baseline() {
    let plan = plan(
        &wanted(vec![
            want("widget.new")
                .prop("replicas", val(json!(2)))
                .protected(true)
                .ignoring(&["replicas"]),
        ])
        .with_moves(vec![MoveDirective::new(
            addr("widget.old"),
            addr("widget.new"),
        )]),
        &stored(vec![have("widget.new", "w-1", json!({"replicas": null}))]),
        &remote(vec![there("widget.new", "w-1", vec![])]),
    );

    assert!(plan.complete() && plan.applyable());
    let change = &plan.changes()[0];
    assert_eq!(change.kind(), ChangeKind::NoOp);
    assert!(change.previous_address().is_none());
    assert_eq!(change.preserved_paths(), &[path("widget", "replicas")]);
    assert!(matches!(
        change
            .checkpoint()
            .present()
            .and_then(|t| t.property(&path("widget", "replicas"))),
        Some(CheckpointValueRef::Null)
    ));
}

#[test]
fn replacement_blocks_a_move_instead_of_degrading_to_a_move_or_update() {
    let plan = plan(
        &wanted(vec![
            want("widget.new")
                .prop("replicas", val(json!(2)))
                .replacing_on(&["replicas"]),
        ])
        .with_moves(vec![MoveDirective::new(
            addr("widget.old"),
            addr("widget.new"),
        )]),
        &stored(vec![have("widget.old", "w-1", json!({"replicas": 1}))]),
        &remote(vec![
            there("widget.old", "w-1", vec![("replicas", known(json!(1)))]),
            missing("widget.new"),
        ]),
    );

    assert!(plan.changes().is_empty());
    let diagnostic = &plan.diagnostics()[0];
    assert_eq!(diagnostic.address(), Some(&addr("widget.old")));
    assert_eq!(diagnostic.related_address(), Some(&addr("widget.new")));
    assert_eq!(
        diagnostic.unsupported(),
        Some(UnsupportedDirectiveKind::Replacement)
    );
}

#[test]
fn ignored_paths_are_ordered_deterministically_and_stored_values_stay_redacted() {
    let existing = stored(vec![have(
        "widget.main",
        "w-1",
        json!({"description": "ignored-secret-canary", "replicas": 1}),
    )]);
    let observed = replicas_remote(1);
    let build = |ignored: &[&str]| {
        plan(
            &wanted(vec![
                want("widget.main")
                    .prop("description", val(json!("desired-secret-canary")))
                    .prop("replicas", val(json!(2)))
                    .ignoring(ignored),
            ]),
            &existing,
            &observed,
        )
    };

    let a = build(&["status", "description"]);
    let b = build(&["description", "status"]);

    assert_eq!(a.to_json_bytes(), b.to_json_bytes());
    let preserved = a.changes()[0].preserved_paths();
    assert_eq!(
        preserved,
        &[path("widget", "description"), path("widget", "status")]
    );
    let shown = format!("{}{a:?}", json_of(&a));
    for canary in ["ignored-secret-canary", "desired-secret-canary"] {
        assert!(!shown.contains(canary));
    }
}

#[test]
fn removing_an_ignore_restores_convergence_and_drift_reporting() {
    let plan = plan(
        &wanted(vec![want("widget.main").prop("replicas", val(json!(2)))]),
        &stored(vec![have("widget.main", "w-1", json!({"replicas": 1}))]),
        &replicas_remote(3),
    );

    assert!(plan.complete() && plan.applyable());
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(plan.changes()[0].fields().len(), 1);
    assert_eq!(plan.drift()[0].kind(), DriftKind::Updated);
    assert_eq!(plan.drift()[0].properties(), &[path("widget", "replicas")]);
}
