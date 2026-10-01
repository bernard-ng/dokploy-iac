use std::io::Write;

use dokploy_core::{ChangeKind, ChangeOrigin, Plan};
/// Renders a value-free human plan summary.
pub fn render(plan: &Plan, output: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        output,
        "Plan: {} change(s), {} drift record(s)",
        plan.changes().len(),
        plan.drift().len()
    )?;

    for change in plan.changes() {
        writeln!(
            output,
            "  {} {} ({})",
            change_kind(change.kind()),
            change.address(),
            change_origin(change.origin())
        )?;
        for field in change.fields() {
            writeln!(output, "    property: {}", field.key())?;
        }
        for metadata in change.metadata() {
            writeln!(output, "    metadata: {metadata:?}")?;
        }
    }

    for diagnostic in plan.diagnostics() {
        write!(output, "  {}", diagnostic.code().as_str())?;
        if let Some(address) = diagnostic.address() {
            write!(output, " {address}")?;
        }
        if let Some(property) = diagnostic.property() {
            write!(output, " property={property}")?;
        }
        if let Some(failure) = diagnostic.selector_failure() {
            write!(output, " selector={}", failure.as_str())?;
        }
        writeln!(output)?;
    }

    Ok(())
}

fn change_kind(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Create => "create",
        ChangeKind::Update => "update",
        ChangeKind::Replace => "replace",
        ChangeKind::Reparent => "reparent",
        ChangeKind::Delete => "delete",
        ChangeKind::Move => "move",
        ChangeKind::Forget => "forget",
        ChangeKind::NoOp => "checkpoint",
    }
}

fn change_origin(origin: ChangeOrigin) -> &'static str {
    match origin {
        ChangeOrigin::Config => "config",
        ChangeOrigin::Drift => "drift",
        ChangeOrigin::ConfigAndDrift => "config_and_drift",
    }
}
