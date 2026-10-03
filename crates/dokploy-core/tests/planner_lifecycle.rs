//! Protection, dependency ordering, moves, and removals.

mod support;

use dokploy_core::{
    ChangeKind, ChangeOrigin, CheckpointValueRef, DriftKind, MetadataChangeKind, MoveAction,
    MoveDirective, OwnedValue, PlanDiagnosticCode, RemoteFailureKind, RemoteObservation,
    RemovalDirective, plan,
};
use serde_json::json;
use support::*;

fn kinds(plan: &dokploy_core::Plan) -> Vec<(String, ChangeKind)> {
    changes_of(plan)
}

#[test]
fn protection_is_a_state_only_checkpoint_and_delete_reads_the_stored_value() {
    let observed = || remote(vec![alive("widget.main", "w-1")]);

    let protect = plan(
        &wanted(vec![creatable("widget.main").protected(true)]),
        &stored(vec![created("widget.main", "w-1")]),
        &observed(),
    );
    let change = &protect.changes()[0];
    assert_eq!(change.kind(), ChangeKind::NoOp);
    assert_eq!(change.origin(), ChangeOrigin::Config);
    assert_eq!(change.metadata(), &[MetadataChangeKind::Protection]);
    assert!(change.checkpoint().present().unwrap().protected());

    let protected = stored(vec![created("widget.main", "w-1").protected()]);
    let unprotect = plan(
        &wanted(vec![creatable("widget.main").protected(false)]),
        &protected,
        &observed(),
    );
    assert_eq!(
        unprotect.changes()[0].metadata(),
        &[MetadataChangeKind::Protection]
    );
    assert!(
        !unprotect.changes()[0]
            .checkpoint()
            .present()
            .unwrap()
            .protected()
    );

    // Dropping a protected resource from the document is refused until protection is lifted
    // in state; the same removal of an unprotected one is a delete.
    let blocked = plan(&wanted(vec![]), &protected, &observed());
    assert!(!blocked.applyable());
    assert_eq!(
        blocked.diagnostics()[0].code(),
        PlanDiagnosticCode::ProtectedDelete
    );

    let deletion = plan(
        &wanted(vec![]),
        &stored(vec![created("widget.main", "w-1")]),
        &observed(),
    );
    assert!(deletion.applyable());
    assert_eq!(deletion.changes()[0].kind(), ChangeKind::Delete);
    assert!(deletion.changes()[0].checkpoint().is_absent());
}

#[test]
fn restating_the_stored_protection_needs_no_checkpoint() {
    let plan = plan(
        &wanted(vec![creatable("widget.main").protected(false)]),
        &stored(vec![created("widget.main", "w-1")]),
        &remote(vec![alive("widget.main", "w-1")]),
    );
    assert!(plan.changes().is_empty());
}

#[test]
fn a_dependency_change_is_a_state_only_checkpoint() {
    let existing = || {
        stored(vec![
            created("widget.main", "w-1").depends_on(&["cache.old"]),
            have("cache.old", "c-1", json!({})),
            have("cache.new", "c-2", json!({})),
        ])
    };
    let observed = || {
        remote(vec![
            alive("widget.main", "w-1"),
            there("cache.old", "c-1", vec![]),
            there("cache.new", "c-2", vec![]),
        ])
    };
    let changed = plan(
        &wanted(vec![
            creatable("widget.main").depends_on(&["cache.new"]),
            want("cache.old"),
            want("cache.new"),
        ]),
        &existing(),
        &observed(),
    );
    let change = changed
        .changes()
        .iter()
        .find(|c| c.address() == &addr("widget.main"))
        .unwrap();
    assert_eq!(change.kind(), ChangeKind::NoOp);
    assert_eq!(change.metadata(), &[MetadataChangeKind::Dependencies]);
    assert_eq!(
        change.checkpoint().present().unwrap().dependencies(),
        &[addr("cache.new")]
    );

    let unchanged = plan(
        &wanted(vec![
            creatable("widget.main").depends_on(&["cache.old"]),
            want("cache.old"),
            want("cache.new"),
        ]),
        &existing(),
        &observed(),
    );
    assert!(unchanged.changes().is_empty(), "{unchanged:?}");
}

#[test]
fn creates_are_dependency_first_with_lexical_ties() {
    let plan = plan(
        &wanted(vec![
            creatable("widget.a").depends_on(&["widget.c"]),
            creatable("widget.b").depends_on(&["widget.c"]),
            creatable("widget.c"),
            creatable("widget.d").depends_on(&["widget.a", "widget.b"]),
            creatable("widget.e"),
        ]),
        &nothing_stored(),
        &remote(
            ["a", "b", "c", "d", "e"]
                .map(|n| missing(&format!("widget.{n}")))
                .into_iter()
                .collect(),
        ),
    );

    let order: Vec<_> = kinds(&plan)
        .into_iter()
        .map(|(address, _)| address)
        .collect();
    assert_eq!(
        order,
        ["widget.c", "widget.a", "widget.b", "widget.d", "widget.e"]
    );
    assert_eq!(json_of(&plan), json_of(&plan));
}

#[test]
fn a_desired_dependency_cycle_blocks_the_plan_without_an_order() {
    let plan = plan(
        &wanted(vec![
            creatable("widget.a").depends_on(&["widget.b"]),
            creatable("widget.b").depends_on(&["widget.a"]),
        ]),
        &nothing_stored(),
        &remote(vec![missing("widget.a"), missing("widget.b")]),
    );
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::DesiredDependencyCycle
    );
}

#[test]
fn a_stored_dependency_cycle_blocks_removal() {
    let plan = plan(
        &wanted(vec![]),
        &stored(vec![
            created("widget.a", "w-1").depends_on(&["widget.b"]),
            created("widget.b", "w-2").depends_on(&["widget.a"]),
        ]),
        &remote(vec![alive("widget.a", "w-1"), alive("widget.b", "w-2")]),
    );
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::StoredDependencyCycle
    );
}

#[test]
fn removals_are_dependent_first_and_follow_all_other_changes() {
    let plan = plan(
        &wanted(vec![creatable("widget.new")]),
        &stored(vec![
            created("widget.base", "w-1"),
            created("widget.leaf", "w-2").depends_on(&["widget.base"]),
            created("widget.mid", "w-3").depends_on(&["widget.base"]),
        ]),
        &remote(vec![
            alive("widget.base", "w-1"),
            alive("widget.leaf", "w-2"),
            alive("widget.mid", "w-3"),
            missing("widget.new"),
        ]),
    );

    assert_eq!(
        kinds(&plan),
        [
            ("widget.new".to_owned(), ChangeKind::Create),
            ("widget.leaf".to_owned(), ChangeKind::Delete),
            ("widget.mid".to_owned(), ChangeKind::Delete),
            ("widget.base".to_owned(), ChangeKind::Delete),
        ]
    );
}

#[test]
fn a_child_is_deleted_before_its_parent_and_created_after_it() {
    let removed = plan(
        &wanted(vec![]),
        &stored(vec![
            created("widget.main", "w-1"),
            have("widget.main/gadget.g", "g-1", json!({})),
        ]),
        &remote(vec![
            alive("widget.main", "w-1"),
            there("widget.main/gadget.g", "g-1", vec![]),
        ]),
    );
    assert_eq!(
        kinds(&removed),
        [
            ("widget.main/gadget.g".to_owned(), ChangeKind::Delete),
            ("widget.main".to_owned(), ChangeKind::Delete),
        ]
    );

    let made = plan(
        &wanted(vec![creatable("widget.main"), want("widget.main/gadget.g")]),
        &nothing_stored(),
        &remote(vec![
            missing("widget.main"),
            missing("widget.main/gadget.g"),
        ]),
    );
    assert_eq!(
        kinds(&made),
        [
            ("widget.main".to_owned(), ChangeKind::Create),
            ("widget.main/gadget.g".to_owned(), ChangeKind::Create),
        ]
    );
}

fn moved_from(old: &str, new: &str) -> dokploy_core::DesiredState {
    wanted(vec![creatable(new)]).with_moves(vec![MoveDirective::new(addr(old), addr(new))])
}

#[test]
fn a_move_keeps_identity_and_is_one_state_only_change_at_the_target() {
    let plan = plan(
        &moved_from("widget.old", "widget.new"),
        &stored(vec![created("widget.old", "w-1").protected()]),
        &remote(vec![alive("widget.old", "w-1"), missing("widget.new")]),
    );

    assert!(plan.complete() && plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes().len(), 1);
    let change = &plan.changes()[0];
    assert_eq!(change.kind(), ChangeKind::Move);
    assert_eq!(change.address(), &addr("widget.new"));
    assert_eq!(change.previous_address(), Some(&addr("widget.old")));
    assert_eq!(change.move_action(), Some(MoveAction::StateOnly));
    assert!(change.fields().is_empty() && change.metadata().is_empty());
    assert!(
        change
            .checkpoint()
            .move_target()
            .is_some_and(|target| target.protected())
    );
    assert!(change.checkpoint().present().is_none());
    assert_eq!(change.checkpoint().move_from(), Some(&addr("widget.old")));

    let shown = format!("{}{plan:?}", json_of(&plan));
    assert!(shown.contains("\"previousAddress\":\"widget.old\""));
    assert!(!shown.contains("w-1"));
}

#[test]
fn a_move_that_also_changes_a_property_updates_the_remote() {
    let plan = plan(
        &wanted(vec![
            creatable("widget.new").prop("replicas", val(json!(2))),
        ])
        .with_moves(vec![MoveDirective::new(
            addr("widget.old"),
            addr("widget.new"),
        )]),
        &stored(vec![created("widget.old", "w-1")]),
        &remote(vec![
            there(
                "widget.old",
                "w-1",
                vec![
                    ("flavor", known(json!("small"))),
                    ("token", unreadable()),
                    ("replicas", known(json!(1))),
                ],
            ),
            missing("widget.new"),
        ]),
    );

    let change = &plan.changes()[0];
    assert_eq!(change.kind(), ChangeKind::Move);
    assert_eq!(change.move_action(), Some(MoveAction::Update));
    assert!(field_names(change).contains(&"replicas".to_owned()));
    assert!(matches!(
        change.checkpoint().move_target().and_then(|t| t.property(&path("widget", "replicas"))),
        Some(CheckpointValueRef::NonSensitive(value)) if value == &json!(2)
    ));
}

#[test]
fn a_move_into_another_parent_is_an_address_move() {
    let plan = plan(
        &wanted(vec![
            creatable("widget.a"),
            creatable("widget.b"),
            want("widget.b/gadget.g"),
        ])
        .with_moves(vec![MoveDirective::new(
            addr("widget.a/gadget.g"),
            addr("widget.b/gadget.g"),
        )]),
        &stored(vec![
            created("widget.a", "w-1"),
            created("widget.b", "w-2"),
            have("widget.a/gadget.g", "g-1", json!({})),
        ]),
        &remote(vec![
            alive("widget.a", "w-1"),
            alive("widget.b", "w-2"),
            there("widget.a/gadget.g", "g-1", vec![]),
            missing("widget.b/gadget.g"),
        ]),
    );

    let moved = plan
        .changes()
        .iter()
        .find(|c| c.kind() == ChangeKind::Move)
        .expect("a move");
    assert_eq!(moved.address(), &addr("widget.b/gadget.g"));
    assert_eq!(moved.previous_address(), Some(&addr("widget.a/gadget.g")));
    assert_eq!(moved.move_action(), Some(MoveAction::StateOnly));
}

#[test]
fn a_persisted_move_declaration_is_idempotent() {
    let plan = plan(
        &moved_from("widget.old", "widget.new"),
        &stored(vec![created("widget.new", "w-1")]),
        &remote(vec![alive("widget.new", "w-1")]),
    );
    assert!(plan.complete() && plan.applyable());
    assert!(plan.changes().is_empty());
}

#[test]
fn directive_and_observation_order_do_not_change_the_plan() {
    // One state: every `state()` call starts a new lineage, which the plan carries.
    let existing = stored(vec![
        created("widget.old-b", "w-2"),
        created("widget.old-a", "w-1"),
    ]);
    let build = |reverse: bool| {
        let mut moves = vec![
            MoveDirective::new(addr("widget.old-a"), addr("widget.new-a")),
            MoveDirective::new(addr("widget.old-b"), addr("widget.new-b")),
        ];
        let mut observed = vec![
            alive("widget.old-a", "w-1"),
            alive("widget.old-b", "w-2"),
            missing("widget.new-a"),
            missing("widget.new-b"),
        ];
        if reverse {
            moves.reverse();
            observed.reverse();
        }
        plan(
            &wanted(vec![creatable("widget.new-a"), creatable("widget.new-b")]).with_moves(moves),
            &existing,
            &remote(observed),
        )
    };
    assert_eq!(build(false).to_json_bytes(), build(true).to_json_bytes());
}

#[test]
fn invalid_move_declarations_block_the_whole_plan() {
    let existing = || stored(vec![created("widget.old", "w-1")]);
    let nothing_observed = || remote(vec![]);
    let block = |desired: dokploy_core::DesiredState| {
        let blocked = plan(&desired, &existing(), &nothing_observed());
        assert!(blocked.changes().is_empty(), "{blocked:?}");
        assert!(!blocked.applyable());
        blocked
    };
    let invalid = |plan: &dokploy_core::Plan| {
        plan.diagnostics()
            .iter()
            .any(|d| d.code() == PlanDiagnosticCode::InvalidMoveDirective)
    };

    // The target is not in the document.
    let missing_target = block(wanted(vec![]).with_moves(vec![MoveDirective::new(
        addr("widget.old"),
        addr("widget.new"),
    )]));
    assert_eq!(
        missing_target.diagnostics()[0].code(),
        PlanDiagnosticCode::InvalidMoveDirective
    );
    assert_eq!(
        missing_target.diagnostics()[0].address(),
        Some(&addr("widget.old"))
    );
    assert_eq!(
        missing_target.diagnostics()[0].related_address(),
        Some(&addr("widget.new"))
    );

    // A move never changes the kind.
    let wrong_kind = block(
        wanted(vec![want("cache.new")]).with_moves(vec![MoveDirective::new(
            addr("widget.old"),
            addr("cache.new"),
        )]),
    );
    assert!(invalid(&wrong_kind));

    // Declared twice, chained, or alongside a removal of the same source.
    let target = || vec![creatable("widget.new")];
    assert!(invalid(&block(wanted(target()).with_moves(vec![
        MoveDirective::new(addr("widget.old"), addr("widget.new")),
        MoveDirective::new(addr("widget.old"), addr("widget.new")),
    ]))));
    let chain = block(
        wanted(vec![creatable("widget.new"), creatable("widget.last")]).with_moves(vec![
            MoveDirective::new(addr("widget.old"), addr("widget.new")),
            MoveDirective::new(addr("widget.new"), addr("widget.last")),
        ]),
    );
    assert!(chain.diagnostics().iter().all(|d| {
        d.code() == PlanDiagnosticCode::InvalidMoveDirective
            && d.address().is_some()
            && d.related_address().is_some()
    }));
    assert!(invalid(&block(
        wanted(target())
            .with_moves(vec![MoveDirective::new(
                addr("widget.old"),
                addr("widget.new")
            )])
            .with_removals(vec![RemovalDirective::new(addr("widget.old"), false)]),
    )));
}

#[test]
fn a_move_needs_a_managed_source_a_free_target_and_conclusive_probes() {
    let desired = || moved_from("widget.old", "widget.new");

    let no_source = plan(&desired(), &nothing_stored(), &remote(vec![]));
    assert_eq!(
        no_source.diagnostics()[0].code(),
        PlanDiagnosticCode::MoveSourceMissing
    );
    assert_eq!(
        no_source.diagnostics()[0].related_address(),
        Some(&addr("widget.new"))
    );

    let both_stored = plan(
        &desired(),
        &stored(vec![
            created("widget.old", "w-1"),
            created("widget.new", "w-2"),
        ]),
        &remote(vec![]),
    );
    assert_eq!(
        both_stored.diagnostics()[0].code(),
        PlanDiagnosticCode::MoveTargetCollision
    );

    let existing = || stored(vec![created("widget.old", "w-1")]);
    let source_gone = plan(
        &desired(),
        &existing(),
        &remote(vec![missing("widget.old"), missing("widget.new")]),
    );
    assert_eq!(
        source_gone.diagnostics()[0].code(),
        PlanDiagnosticCode::MoveSourceMissing
    );

    let other_id = plan(
        &desired(),
        &existing(),
        &remote(vec![
            alive("widget.old", "different"),
            missing("widget.new"),
        ]),
    );
    assert_eq!(
        other_id.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteIdentityMismatch
    );

    let unavailable = plan(
        &desired(),
        &existing(),
        &remote(vec![
            alive("widget.old", "w-1"),
            (
                addr("widget.new"),
                RemoteObservation::Unavailable(RemoteFailureKind::Unavailable),
            ),
        ]),
    );
    assert!(!unavailable.complete());
    assert_eq!(
        unavailable.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteUnavailable
    );
    assert_eq!(
        unavailable.diagnostics()[0].address(),
        Some(&addr("widget.old"))
    );
    assert_eq!(
        unavailable.diagnostics()[0].related_address(),
        Some(&addr("widget.new"))
    );

    let taken = plan(
        &desired(),
        &existing(),
        &remote(vec![
            alive("widget.old", "w-1"),
            alive("widget.new", "other"),
        ]),
    );
    assert!(taken.changes().is_empty());
    assert_eq!(
        taken.diagnostics()[0].code(),
        PlanDiagnosticCode::MoveTargetCollision
    );
    assert_eq!(taken.diagnostics()[0].address(), Some(&addr("widget.old")));
    assert_eq!(
        taken.diagnostics()[0].related_address(),
        Some(&addr("widget.new"))
    );
}

#[test]
fn a_move_that_replaces_on_a_changed_property_is_blocked_not_degraded() {
    let plan = plan(
        &wanted(vec![
            want("widget.new")
                .prop("flavor", val(json!("large")))
                .prop("token", secret(7))
                .replacing_on(&["flavor"]),
        ])
        .with_moves(vec![MoveDirective::new(
            addr("widget.old"),
            addr("widget.new"),
        )]),
        &stored(vec![created("widget.old", "w-1")]),
        &remote(vec![alive("widget.old", "w-1"), missing("widget.new")]),
    );
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty(), "{plan:?}");
    assert!(!plan.diagnostics().is_empty());
}

#[test]
fn a_retained_removal_forgets_without_reading_properties() {
    let desired =
        || wanted(vec![]).with_removals(vec![RemovalDirective::new(addr("widget.old"), false)]);
    let existing = || stored(vec![created("widget.old", "w-1")]);

    let present = plan(
        &desired(),
        &existing(),
        &remote(vec![there("widget.old", "w-1", vec![])]),
    );
    assert!(present.applyable());
    assert_eq!(present.changes()[0].kind(), ChangeKind::Forget);
    assert!(present.drift().is_empty());

    let gone = plan(
        &desired(),
        &existing(),
        &remote(vec![missing("widget.old")]),
    );
    assert_eq!(gone.changes()[0].kind(), ChangeKind::Forget);
    assert_eq!(gone.drift()[0].kind(), DriftKind::Deleted);
}

#[test]
fn a_destroying_removal_honors_protection() {
    let desired =
        wanted(vec![]).with_removals(vec![RemovalDirective::new(addr("widget.old"), true)]);
    let observed = remote(vec![there("widget.old", "w-1", vec![])]);

    let blocked = plan(
        &desired,
        &stored(vec![created("widget.old", "w-1").protected()]),
        &observed,
    );
    assert!(!blocked.applyable());
    assert_eq!(
        blocked.diagnostics()[0].code(),
        PlanDiagnosticCode::ProtectedDelete
    );

    let deletion = plan(
        &desired,
        &stored(vec![created("widget.old", "w-1")]),
        &observed,
    );
    assert!(deletion.applyable());
    assert_eq!(deletion.changes()[0].kind(), ChangeKind::Delete);
}

#[test]
fn a_persisted_removal_is_idempotent_once_state_forgot_the_resource() {
    for destroy in [false, true] {
        let plan = plan(
            &wanted(vec![]).with_removals(vec![RemovalDirective::new(addr("widget.old"), destroy)]),
            &nothing_stored(),
            &remote(vec![there("widget.old", "w-1", vec![])]),
        );
        assert!(plan.complete() && plan.applyable());
        assert!(plan.changes().is_empty() && plan.diagnostics().is_empty());
    }
}

#[test]
fn removals_that_could_delete_the_wrong_thing_are_refused() {
    let desired =
        || wanted(vec![]).with_removals(vec![RemovalDirective::new(addr("widget.old"), false)]);
    let ambiguous = plan(
        &desired(),
        &stored(vec![created("widget.old", "w-1")]),
        &remote(vec![there("widget.old", "someone-else", vec![])]),
    );
    assert!(!ambiguous.applyable());
    assert_eq!(
        ambiguous.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteIdentityMismatch
    );

    let twice = plan(
        &wanted(vec![]).with_removals(vec![
            RemovalDirective::new(addr("widget.old"), true),
            RemovalDirective::new(addr("widget.old"), false),
        ]),
        &stored(vec![created("widget.old", "w-1")]),
        &remote(vec![there("widget.old", "w-1", vec![])]),
    );
    assert!(!twice.applyable());
    assert!(twice.changes().is_empty());
    assert_eq!(
        twice.diagnostics()[0].code(),
        PlanDiagnosticCode::InvalidRemovalDirective
    );
}

#[test]
fn explicit_removals_follow_stored_dependencies_dependent_first() {
    let plan = plan(
        &wanted(vec![]).with_removals(vec![
            RemovalDirective::new(addr("widget.base"), true),
            RemovalDirective::new(addr("widget.leaf"), true),
        ]),
        &stored(vec![
            created("widget.base", "w-1"),
            created("widget.leaf", "w-2").depends_on(&["widget.base"]),
        ]),
        &remote(vec![
            alive("widget.base", "w-1"),
            alive("widget.leaf", "w-2"),
        ]),
    );
    assert_eq!(
        kinds(&plan),
        [
            ("widget.leaf".to_owned(), ChangeKind::Delete),
            ("widget.base".to_owned(), ChangeKind::Delete),
        ]
    );
}

#[allow(dead_code)]
fn unused(_: OwnedValue) {}
