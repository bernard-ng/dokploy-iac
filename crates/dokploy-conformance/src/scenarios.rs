//! The scenarios of ADR 0015, generated from a kind's spec.

use dokploy_core::PlanDiagnosticCode;
use dokploy_engine::{ApplyError, RecoverError, RecoveryAction};
use dokploy_sim::{Fault, FaultKind};
use dokploy_spec::{Authority, Mutability, Shape};
use serde_json::{Value as Json, json};

use crate::case::Values;
use crate::world::{Check, World};

/// How a scenario ended.
pub(crate) enum Verdict {
    Pass,
    Skip(String),
}

macro_rules! ensure {
    ($condition:expr, $($message:tt)+) => {
        if !$condition {
            return Err(format!($($message)+));
        }
    };
}

fn skip(reason: &str) -> Check<Verdict> {
    Ok(Verdict::Skip(reason.to_owned()))
}

fn create_op(w: &World<'_>) -> String {
    w.case
        .spec
        .api
        .create
        .as_ref()
        .expect("a kind with a create")
        .op
        .clone()
}

/// The requests a create sends: the create operation, and then one write group for the fields
/// it does not accept (a project kind's create takes a name and the rest is updated).
fn creation_ops(w: &World<'_>, values: &Values) -> Vec<String> {
    let spec = &w.case.spec;
    let create = create_op(w);
    let contract = dokploy_api::request_contract(&create);
    let deferred = |name: &str| {
        let Some(field) = spec.fields.get(name) else {
            return false;
        };
        field.mutability != Mutability::Computed
            && values.contains_key(name)
            && contract
                .is_some_and(|contract| contract.body_field(field.request_name(name)).is_none())
    };
    let mut operations = vec![create];
    for group in &spec.write {
        match group {
            dokploy_spec::WriteGroup::Op { op, fields, .. }
                if fields.iter().any(|name| deferred(name)) =>
            {
                operations.push(op.clone());
            }
            // The operation of the arm the values name: the members are what the create left.
            dokploy_spec::WriteGroup::ByVariant {
                by_variant, ops, ..
            } => {
                let arm = values.iter().find_map(|(name, value)| {
                    let tag = w.case.fields.iter().find(|f| &f.name == name)?;
                    (name.starts_with(&format!("{by_variant}.")) && tag.group.is_some())
                        .then_some(())?;
                    match value {
                        crate::case::Val::Json(serde_json::Value::String(arm))
                            if ops.contains_key(arm) =>
                        {
                            Some(arm.clone())
                        }
                        _ => None,
                    }
                });
                let members_deferred = values.keys().any(|name| {
                    name.starts_with(&format!("{by_variant}."))
                        && w.case
                            .fields
                            .iter()
                            .any(|f| &f.name == name && f.name.matches('.').count() == 2)
                });
                if let (Some(arm), true) = (arm, members_deferred) {
                    operations.push(ops[&arm].clone());
                }
            }
            dokploy_spec::WriteGroup::Op { .. } => {}
        }
    }

    operations
}

fn remove_op(w: &World<'_>) -> String {
    w.case
        .spec
        .api
        .remove
        .as_ref()
        .expect("a kind with a remove")
        .op
        .clone()
}

/// The operation whose failure makes the kind's collection unreadable: its list, or the
/// parent's direct read when the collection is embedded in it.
fn list_op(w: &World<'_>) -> String {
    w.case
        .collection_source()
        .map(|(operation, _)| operation)
        .expect("a kind with a collection read")
}

/// The remote object holds what the values say.
fn holds(w: &World<'_>, values: &Values) -> Check {
    let object = w.only_object()?;
    for (wire, want) in w.case.expected_remote(values) {
        ensure!(
            object.get(&wire) == Some(&want),
            "remote `{wire}` is not what the document says"
        );
    }

    Ok(())
}

/// Planning the same document again changes nothing.
async fn converged(w: &World<'_>, values: &Values) -> Check {
    let plan = w.plan(values).await?;
    ensure!(
        plan.applyable(),
        "the plan after an apply is blocked: {:?}",
        plan.diagnostics()
    );
    ensure!(
        plan.changes().is_empty(),
        "the plan after an apply is not empty: {} change(s)",
        plan.changes().len()
    );

    Ok(())
}

pub(crate) async fn create(w: &World<'_>) -> Check<Verdict> {
    let values = w.case.full();
    let summary = w.apply_ok(&values).await?;
    ensure!(
        summary.applied() == 1,
        "applied {} changes",
        summary.applied()
    );
    holds(w, &values)?;
    ensure!(
        w.operations() == creation_ops(w, &values),
        "a create sends {:?}, sent {:?}",
        creation_ops(w, &values),
        w.operations()
    );
    ensure!(
        w.state_resources() == 1,
        "state does not record the resource"
    );
    converged(w, &values).await?;

    Ok(Verdict::Pass)
}

pub(crate) async fn create_minimal(w: &World<'_>) -> Check<Verdict> {
    let values = w.case.minimal();
    if values == w.case.full() {
        return skip("every field is required");
    }
    w.apply_ok(&values).await?;
    holds(w, &values)?;
    converged(w, &values).await?;

    Ok(Verdict::Pass)
}

pub(crate) async fn converges(w: &World<'_>) -> Check<Verdict> {
    let values = w.case.full();
    w.apply_ok(&values).await?;
    w.sim.clear_requests();
    let summary = w.apply_ok(&values).await?;
    ensure!(
        summary.applied() == 0,
        "a second apply changed {} resources",
        summary.applied()
    );
    ensure!(
        w.sim.mutations().is_empty(),
        "a second apply sent {:?}",
        w.operations()
    );

    Ok(Verdict::Pass)
}

pub(crate) async fn update(w: &World<'_>, name: &str) -> Check<Verdict> {
    let field = w.case.field(name);
    let Some(_) = field.b else {
        return skip("the field has only one possible value");
    };
    let Some((operation, shape, group_fields)) = field.group.clone() else {
        return skip("the field is in no write group");
    };
    let values = w.case.full();
    w.apply_ok(&values).await?;
    w.sim.clear_requests();

    let changed = w.case.changed(&values, name);
    w.apply_ok(&changed).await?;

    ensure!(
        w.operations() == [operation.clone()],
        "expected one `{operation}`, sent {:?}",
        w.operations()
    );
    let body = w.sim.mutations()[0].body().cloned().unwrap_or(Json::Null);
    let id = &w.case.spec.api.id;
    let mut allowed: Vec<String> = vec![id.clone()];
    match shape {
        Shape::Partial => allowed.push(field.wire.clone()),
        Shape::Full => {
            allowed.extend(
                group_fields
                    .iter()
                    .flat_map(|name| w.case.cases_of(name))
                    .map(|case| case.wire.clone()),
            );
            allowed.extend(w.case.group_wires(&group_fields));
        }
    }
    for key in body
        .as_object()
        .map(|o| o.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        ensure!(
            allowed.contains(&key),
            "the update sent `{key}`, which is outside its write group"
        );
    }
    ensure!(
        body.get(&field.wire) == field.b.as_ref().map(crate::case::Val::json).as_ref(),
        "the update did not carry the new value"
    );
    holds(w, &changed)?;
    converged(w, &changed).await?;

    Ok(Verdict::Pass)
}

pub(crate) async fn replace(w: &World<'_>, name: &str) -> Check<Verdict> {
    let field = w.case.field(name);
    let Some(_) = field.b else {
        return skip("the field has only one possible value");
    };
    let values = w.case.full();
    w.apply_ok(&values).await?;
    let first = w.only_object()?[&w.case.spec.api.id].clone();
    w.sim.clear_requests();

    let changed = w.case.changed(&values, name);
    w.apply_ok(&changed).await?;

    let expected: Vec<String> = std::iter::once(remove_op(w))
        .chain(creation_ops(w, &changed))
        .collect();
    ensure!(
        w.operations() == expected,
        "expected {expected:?}, sent {:?}",
        w.operations()
    );
    ensure!(
        w.only_object()?[&w.case.spec.api.id] != first,
        "the replacement kept the identity"
    );
    holds(w, &changed)?;
    converged(w, &changed).await?;

    Ok(Verdict::Pass)
}

pub(crate) async fn drift(w: &World<'_>, name: &str) -> Check<Verdict> {
    let field = w.case.field(name);
    let Some(other) = field.b.clone() else {
        return skip("the field has only one possible value");
    };
    let values = w.case.full();
    w.apply_ok(&values).await?;
    let id = w.only_object()?[&w.case.spec.api.id]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    w.sim.patch(
        &w.case.spec.kind,
        &id,
        &json!({ field.wire.clone(): other.json() }),
    );

    let plan = w.plan(&values).await?;
    ensure!(
        plan.applyable(),
        "drift blocked the plan: {:?}",
        plan.diagnostics()
    );
    ensure!(
        plan.changes().len() == 1,
        "drift planned {} changes",
        plan.changes().len()
    );
    ensure!(
        plan.changes()[0]
            .fields()
            .iter()
            .any(|f| f.key().to_string() == name),
        "the plan does not name `{name}`"
    );
    w.apply_ok(&values).await?;
    holds(w, &values)?;
    converged(w, &values).await?;

    Ok(Verdict::Pass)
}

pub(crate) async fn delete(w: &World<'_>) -> Check<Verdict> {
    let values = w.case.full();
    w.apply_ok(&values).await?;
    w.sim.clear_requests();

    let summary = w
        .apply_text(&w.case.without_child(), Default::default())
        .await?
        .map_err(|error| format!("delete failed: {error}"))?;

    ensure!(
        summary.applied() == 1,
        "applied {} changes",
        summary.applied()
    );
    ensure!(
        w.operations() == [remove_op(w)],
        "sent {:?}",
        w.operations()
    );
    ensure!(w.objects().is_empty(), "the object is still there");
    ensure!(w.state_resources() == 0, "state still records it");

    Ok(Verdict::Pass)
}

pub(crate) async fn protected_delete(w: &World<'_>) -> Check<Verdict> {
    let values = w.case.full();
    let text = w.case.document(&values, true);
    w.apply_text(&text, w.case.environment(&values))
        .await?
        .map_err(|error| format!("create failed: {error}"))?;
    w.sim.clear_requests();

    let plan = w
        .plan_text(&w.case.without_child(), Default::default())
        .await?;
    ensure!(
        plan.diagnostics()
            .iter()
            .any(|d| d.code() == PlanDiagnosticCode::ProtectedDelete),
        "removing a protected resource is not refused"
    );
    let result = w
        .apply_text(&w.case.without_child(), Default::default())
        .await?;
    ensure!(
        matches!(result, Err(ApplyError::Blocked { .. })),
        "the apply was not blocked"
    );
    ensure!(
        w.sim.mutations().is_empty(),
        "a protected resource was changed"
    );
    ensure!(w.objects().len() == 1, "the protected object is gone");

    Ok(Verdict::Pass)
}

pub(crate) async fn unmanaged_collision(w: &World<'_>, copies: usize) -> Check<Verdict> {
    if w.case.spec.identity.collision.is_empty() {
        return skip("the kind has no collision fields");
    }
    let values = w.case.full();
    let existing = w.case.collision_object(&values);
    if existing.as_object().is_some_and(serde_json::Map::is_empty) {
        return skip("no collision field has a value");
    }
    for _ in 0..copies {
        w.seed_child(&existing);
    }

    let plan = w.plan(&values).await?;
    ensure!(
        !plan.applyable(),
        "an existing unmanaged resource did not block the plan"
    );
    ensure!(
        plan.changes().is_empty(),
        "the blocked plan still has changes"
    );
    let result = w.apply(&values).await?;
    ensure!(
        matches!(result, Err(ApplyError::Blocked { .. })),
        "the apply was not blocked"
    );
    ensure!(w.sim.mutations().is_empty(), "something was changed");
    ensure!(
        w.objects().len() == copies,
        "the existing objects were touched"
    );

    Ok(Verdict::Pass)
}

pub(crate) async fn declined(w: &World<'_>) -> Check<Verdict> {
    let values = w.case.full();
    let result = w.apply_declined(&values).await?;
    ensure!(
        matches!(result, Err(ApplyError::Declined)),
        "a declined plan was not reported as declined"
    );
    ensure!(
        w.sim.mutations().is_empty(),
        "a declined plan changed something"
    );
    ensure!(w.objects().is_empty(), "a declined plan created something");

    Ok(Verdict::Pass)
}

pub(crate) async fn rejected_create(w: &World<'_>) -> Check<Verdict> {
    let values = w.case.full();
    w.sim
        .inject(Fault::new(create_op(w), FaultKind::Reject { status: 400 }));
    let result = w.apply(&values).await?;
    ensure!(
        matches!(result, Err(ApplyError::Rejected { .. })),
        "expected a rejection"
    );
    ensure!(w.objects().is_empty(), "a rejected create left an object");
    ensure!(w.state_resources() == 0, "a rejected create was recorded");
    let closed = w.recover(&values).await?;
    ensure!(
        matches!(closed, Ok(RecoveryAction::ResolveOperation)),
        "recovering a failed step: {closed:?}"
    );
    w.apply_ok(&values).await?;
    holds(w, &values)?;

    Ok(Verdict::Pass)
}

pub(crate) async fn read_failure(w: &World<'_>) -> Check<Verdict> {
    let values = w.case.full();
    w.sim.inject(Fault::new(list_op(w), FaultKind::Unavailable));
    let result = w.apply(&values).await?;
    ensure!(
        matches!(result, Err(ApplyError::Blocked { .. })),
        "an unreadable remote did not block the apply"
    );
    ensure!(w.sim.mutations().is_empty(), "something was changed");

    Ok(Verdict::Pass)
}

pub(crate) async fn partial_authority(w: &World<'_>) -> Check<Verdict> {
    let authority = w.case.spec.api.read.list.as_ref().map(|l| l.authority);
    if authority != Some(Authority::Partial) {
        return skip("the collection is authoritative");
    }
    let values = w.case.full();
    let id = w
        .sim
        .seed(&w.case.spec.kind, &w.case.collision_object(&values));
    w.sim.hide_from_listing(&w.case.spec.kind, &id);
    let plan = w.plan(&values).await?;
    ensure!(
        !plan.applyable(),
        "absence from a partial collection was taken as proof"
    );

    Ok(Verdict::Pass)
}

#[derive(Clone, Copy)]
pub(crate) enum Step {
    Create,
    Update,
    Delete,
}

/// The mutation is interrupted (the request never arrives, or the response is lost) and
/// recovery settles it from fresh evidence.
pub(crate) async fn interrupted(w: &World<'_>, step: Step, after: bool) -> Check<Verdict> {
    let full = w.case.full();
    let (operation, document_values, expected_action, expect_converged_on) = match step {
        Step::Create => (
            create_op(w),
            full.clone(),
            if after {
                RecoveryAction::AdoptCreatedResource
            } else {
                RecoveryAction::ConfirmNoChange
            },
            full.clone(),
        ),
        Step::Update => {
            let Some(name) = w.case.fields.iter().find(|f| {
                !f.secret
                    && f.b.is_some()
                    && f.group.is_some()
                    && f.mutability == Mutability::InPlace
            }) else {
                return skip("no public in-place field has a second value");
            };
            let name = name.name.clone();
            w.apply_ok(&full).await?;
            let changed = w.case.changed(&full, &name);
            let op = w.case.field(&name).group.clone().expect("a group").0;
            (
                op,
                changed.clone(),
                if after {
                    RecoveryAction::CheckpointConfirmedSuccess
                } else {
                    RecoveryAction::ConfirmNoChange
                },
                changed,
            )
        }
        Step::Delete => {
            w.apply_ok(&full).await?;
            (
                remove_op(w),
                Values::new(),
                if after {
                    RecoveryAction::CheckpointConfirmedSuccess
                } else {
                    RecoveryAction::ConfirmNoChange
                },
                Values::new(),
            )
        }
    };
    let kind = if after {
        FaultKind::DropAfter
    } else {
        FaultKind::DropBefore
    };
    w.sim.inject(Fault::new(operation.clone(), kind));
    w.sim.clear_requests();

    let result = if matches!(step, Step::Delete) {
        w.apply_text(&w.case.without_child(), Default::default())
            .await?
    } else {
        w.apply(&document_values).await?
    };
    ensure!(
        matches!(result, Err(ApplyError::OutcomeUnknown { .. })),
        "expected an unknown outcome"
    );
    let sent = w.operations().iter().filter(|op| **op == operation).count();
    ensure!(sent == 1, "the mutation was sent {sent} times");
    ensure!(!w.recovery_clean(), "an unknown outcome left no open step");

    let recovered = if matches!(step, Step::Delete) {
        // Recovery needs the document that was being applied: the empty one.
        w.recover_text(&w.case.without_child(), Default::default())
            .await?
    } else {
        w.recover(&document_values).await?
    };
    let action: RecoveryAction =
        recovered.map_err(|error: RecoverError| format!("recovery failed: {error}"))?;
    ensure!(
        action == expected_action,
        "recovery chose {action:?}, expected {expected_action:?}"
    );
    ensure!(w.recovery_clean(), "recovery left the journal open");
    let sent_after = w.operations().iter().filter(|op| **op == operation).count();
    ensure!(sent_after == 1, "recovery repeated the mutation");

    // Whatever recovery decided, one more apply brings the remote to the document.
    if matches!(step, Step::Delete) {
        w.apply_text(&w.case.without_child(), Default::default())
            .await?
            .map_err(|error| format!("the apply after recovery failed: {error}"))?;
        ensure!(w.objects().is_empty(), "the object survived");
        ensure!(w.state_resources() == 0, "state still records it");
    } else {
        w.apply_ok(&expect_converged_on).await?;
        holds(w, &expect_converged_on)?;
        converged(w, &expect_converged_on).await?;
    }

    Ok(Verdict::Pass)
}

/// A selector that names nothing, or two things, is not guessed at: the plan is blocked with a
/// typed diagnostic and nothing is changed.
pub(crate) async fn selector_unresolved(w: &World<'_>, ambiguous: bool) -> Check<Verdict> {
    let Some(field) = w
        .case
        .fields
        .iter()
        .find(|field| matches!(field.a, crate::case::Val::Selector { .. }))
    else {
        return skip("the kind has no selector the suite can seed");
    };
    let crate::case::Val::Selector { kind, name, .. } = field.a.clone() else {
        unreachable!("matched above");
    };
    let mut values = w.case.full();
    if ambiguous {
        w.seed_target(&kind, "twin-of-the-target", &name);
    } else {
        values.insert(
            field.name.clone(),
            crate::case::Val::Selector {
                kind,
                name: "nobody-has-this-name".to_owned(),
                id: "no-such-id".to_owned(),
            },
        );
    }

    let plan = w.plan(&values).await?;
    ensure!(
        !plan.applyable(),
        "an unresolved selector did not block the plan"
    );
    ensure!(
        plan.diagnostics()
            .iter()
            .any(|d| d.code() == PlanDiagnosticCode::UnresolvedExternalSelector),
        "the plan does not say which selector is unresolved: {:?}",
        plan.diagnostics()
    );
    let result = w.apply(&values).await?;
    ensure!(
        matches!(result, Err(ApplyError::Blocked { .. })),
        "the apply was not blocked"
    );
    ensure!(w.sim.mutations().is_empty(), "something was changed");

    Ok(Verdict::Pass)
}

/// A create that Dokploy accepts but whose follow-up write fails or is interrupted: the
/// resource exists and state records what the create wrote, nothing is repeated, and one more
/// apply finishes the job.
pub(crate) async fn follow_up(w: &World<'_>, how: FollowUp) -> Check<Verdict> {
    let values = w.case.full();
    let operations = creation_ops(w, &values);
    // The follow-up is one request per write group, in order. A response lost on the last of
    // them means everything was applied; lost on an earlier one means only a part was, which
    // recovery rightly does not call a success.
    let Some(operation) = (match how {
        FollowUp::LostAfter => operations.last().filter(|_| operations.len() > 1),
        FollowUp::Rejected | FollowUp::LostBefore => operations.get(1),
    })
    .cloned() else {
        return skip("the create carries every field");
    };
    let fault = match how {
        FollowUp::Rejected => FaultKind::Reject { status: 400 },
        FollowUp::LostBefore => FaultKind::DropBefore,
        FollowUp::LostAfter => FaultKind::DropAfter,
    };
    w.sim.inject(Fault::new(operation.clone(), fault));

    let result = w.apply(&values).await?;
    match how {
        FollowUp::Rejected => ensure!(
            matches!(result, Err(ApplyError::Rejected { .. })),
            "expected a rejection, got {result:?}"
        ),
        FollowUp::LostBefore | FollowUp::LostAfter => ensure!(
            matches!(result, Err(ApplyError::OutcomeUnknown { .. })),
            "expected an unknown outcome, got {result:?}"
        ),
    }
    ensure!(w.objects().len() == 1, "the create did not happen");
    ensure!(
        w.state_resources() == 1,
        "state does not record what the create wrote"
    );
    let creates = w
        .operations()
        .iter()
        .filter(|op| **op == operations[0])
        .count();
    ensure!(creates == 1, "the create was sent {creates} times");

    let recovered = w.recover(&values).await?;
    let action = recovered.map_err(|error: RecoverError| format!("recovery failed: {error}"))?;
    let expected: &[RecoveryAction] = match how {
        FollowUp::Rejected => &[RecoveryAction::ResolveOperation],
        // Nothing was applied; when Dokploy's own defaults already equal the document, the
        // remote already shows the target and that is a success all the same.
        FollowUp::LostBefore => &[
            RecoveryAction::ConfirmNoChange,
            RecoveryAction::CheckpointConfirmedSuccess,
        ],
        FollowUp::LostAfter => &[RecoveryAction::CheckpointConfirmedSuccess],
    };
    ensure!(
        expected.contains(&action),
        "recovery chose {action:?}, expected one of {expected:?}"
    );
    ensure!(w.recovery_clean(), "recovery left the journal open");
    let creates = w
        .operations()
        .iter()
        .filter(|op| **op == operations[0])
        .count();
    ensure!(creates == 1, "recovery repeated the create");

    w.apply_ok(&values).await?;
    ensure!(
        w.objects().len() == 1,
        "finishing the job created a second object"
    );
    holds(w, &values)?;
    converged(w, &values).await?;

    Ok(Verdict::Pass)
}

#[derive(Clone, Copy)]
pub(crate) enum FollowUp {
    Rejected,
    LostBefore,
    LostAfter,
}

/// Dokploy acknowledges a removal and does nothing: the step fails and the resource stays
/// tracked, so the next plan removes it again.
pub(crate) async fn removal_not_applied(w: &World<'_>) -> Check<Verdict> {
    let values = w.case.full();
    w.apply_ok(&values).await?;
    w.sim.inject(Fault::new(remove_op(w), FaultKind::Swallow));

    let result = w
        .apply_text(&w.case.without_child(), Default::default())
        .await?;
    ensure!(
        matches!(result, Err(ApplyError::NotRemoved { .. })),
        "an unapplied removal was reported as done"
    );
    ensure!(
        w.state_resources() == 1,
        "state forgot a resource that is still there"
    );
    ensure!(w.objects().len() == 1, "the object disappeared");
    // A failed step leaves the journal for recovery to close before the next apply.
    let closed = w
        .recover_text(&w.case.without_child(), Default::default())
        .await?;
    ensure!(
        matches!(closed, Ok(RecoveryAction::ResolveOperation)),
        "recovering a failed step: {closed:?}"
    );
    w.apply_text(&w.case.without_child(), Default::default())
        .await?
        .map_err(|error| format!("the retry failed: {error}"))?;
    ensure!(w.objects().is_empty(), "the retry did not remove it");

    Ok(Verdict::Pass)
}

/// Another client creates the same thing at the same moment: the engine cannot tell which
/// object its create made, and says so instead of picking one.
pub(crate) async fn identity_ambiguous(w: &World<'_>) -> Check<Verdict> {
    use dokploy_spec::CreateIdentity;
    if !matches!(
        w.case.spec.api.create_identity,
        Some(CreateIdentity::DiffCollection { .. })
    ) {
        return skip("the identity comes from the create response");
    }
    let values = w.case.full();
    w.sim.inject(Fault::new(create_op(w), FaultKind::Duplicate));

    let result = w.apply(&values).await?;
    ensure!(
        matches!(result, Err(ApplyError::OutcomeUnknown { .. })),
        "an ambiguous identity was guessed"
    );
    ensure!(w.objects().len() == 2, "expected the create and its twin");
    ensure!(
        w.state_resources() == 0,
        "state recorded one of two candidates"
    );
    let recovered = w.recover(&values).await?;
    ensure!(
        matches!(recovered, Err(RecoverError::ManualIntervention)),
        "recovery chose between twins"
    );

    Ok(Verdict::Pass)
}

/// A kind's collection that names another parent's items, or a direct read that names another
/// parent, is not this parent's child: nothing may be concluded from it.
pub(crate) async fn foreign_parent(w: &World<'_>, direct: bool) -> Check<Verdict> {
    let Some(attach) = w.case.parent_id_field() else {
        return skip("the kind has no parent");
    };
    let values = w.case.full();
    let id_field = w.case.spec.api.id.clone();
    if direct {
        w.apply_ok(&values).await?;
        let one = w
            .case
            .spec
            .api
            .read
            .one
            .as_ref()
            .expect("a direct read")
            .op
            .clone();
        w.sim.tamper(&one, move |response| {
            response[attach.clone()] = json!("another-parent")
        });
    } else {
        let Some((operation, pointer)) = w.case.collection_source() else {
            return skip("no collection");
        };
        let stranger = {
            let mut object = w.case.collision_object(&values);
            object[&id_field] = json!("a-stranger");
            object[&attach] = json!("another-parent");
            object
        };
        w.sim.tamper(&operation, move |response| {
            let items = if pointer.is_empty() {
                Some(&mut *response)
            } else {
                response.pointer_mut(&pointer)
            };
            if let Some(Json::Array(items)) = items {
                items.push(stranger.clone());
            }
        });
    }

    let plan = w.plan(&values).await?;
    ensure!(
        !plan.applyable(),
        "a response naming another parent was trusted"
    );
    let result = w.apply(&values).await?;
    ensure!(
        matches!(result, Err(ApplyError::Blocked { .. })),
        "the apply was not blocked"
    );

    Ok(Verdict::Pass)
}
