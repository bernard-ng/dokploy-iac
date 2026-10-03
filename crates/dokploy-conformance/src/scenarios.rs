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

fn list_op(w: &World<'_>) -> String {
    w.case
        .spec
        .api
        .read
        .list
        .as_ref()
        .and_then(|list| list.op.clone())
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
        w.operations() == [create_op(w)],
        "a create sends one request, sent {:?}",
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
    let allowed: Vec<&str> = std::iter::once(id.as_str())
        .chain(match shape {
            Shape::Partial => vec![field.wire.as_str()],
            Shape::Full => group_fields
                .iter()
                .map(|name| w.case.field(name).wire.as_str())
                .collect(),
        })
        .collect();
    for key in body
        .as_object()
        .map(|o| o.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        ensure!(
            allowed.contains(&key.as_str()),
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

    ensure!(
        w.operations() == [remove_op(w), create_op(w)],
        "expected remove then create, sent {:?}",
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
        .apply_text(crate::case::Case::empty_document(), Default::default())
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
        .plan_text(crate::case::Case::empty_document(), Default::default())
        .await?;
    ensure!(
        plan.diagnostics()
            .iter()
            .any(|d| d.code() == PlanDiagnosticCode::ProtectedDelete),
        "removing a protected resource is not refused"
    );
    let result = w
        .apply_text(crate::case::Case::empty_document(), Default::default())
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
        w.sim.seed(&w.case.spec.kind, &existing);
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
        w.apply_text(crate::case::Case::empty_document(), Default::default())
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
        w.recover_text(crate::case::Case::empty_document(), Default::default())
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
        w.apply_text(crate::case::Case::empty_document(), Default::default())
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
