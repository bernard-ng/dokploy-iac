//! Read-only connection and selector checks for `dokploy doctor`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use dokploy_sdk::{OperationRequest, Transport};
use dokploy_spec::{Field, FieldType, SpecRegistry, VersionCompatibility, parse_type};
use miette::{IntoDiagnostic, Result};

use crate::CommandStatus;
use crate::compatibility;

/// Runs every diagnostic check without mutating the instance.
pub async fn run(
    transport: &impl Transport,
    specs: &SpecRegistry,
    output: &mut dyn Write,
) -> Result<CommandStatus> {
    let mut failed = false;
    writeln!(output, "ok  URL: {}", transport.base_url()).into_diagnostic()?;

    match compatibility::probe(transport).await {
        Ok(version) => {
            let label = match version.compatibility {
                VersionCompatibility::Supported => "supported",
                VersionCompatibility::NewerPatch => "newer patch",
                VersionCompatibility::Unsupported => {
                    failed = true;
                    "unsupported"
                }
            };
            writeln!(output, "ok  API key and version: {} ({label})", version.raw)
                .into_diagnostic()?;
        }
        Err(error) => {
            failed = true;
            writeln!(output, "fail API key or version: {error}").into_diagnostic()?;
        }
    }

    match transport
        .call(OperationRequest::new("organization.active"))
        .await
    {
        Ok(organization) => {
            let name = organization
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unnamed");
            let id = organization
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown id");
            writeln!(output, "ok  Organization: {name} ({id})").into_diagnostic()?;
        }
        Err(error) => {
            failed = true;
            writeln!(output, "fail Organization and read rights: {error}").into_diagnostic()?;
        }
    }

    match check_selector_catalogs(transport, specs).await {
        Ok((catalogs, names, skipped)) => writeln!(
            output,
            "ok  Selector catalogs: {catalogs} checked, {names} names, {skipped} scoped or virtual"
        )
        .into_diagnostic()?,
        Err(error) => {
            failed = true;
            writeln!(output, "fail Selector catalogs: {error}").into_diagnostic()?;
        }
    }

    Ok(if failed {
        CommandStatus::Failure
    } else {
        CommandStatus::Success
    })
}

async fn check_selector_catalogs(
    transport: &impl Transport,
    specs: &SpecRegistry,
) -> Result<(usize, usize, usize)> {
    let targets = selector_targets(specs);
    let mut catalogs = 0_usize;
    let mut names = 0_usize;
    let mut skipped = 0_usize;
    for target in targets {
        let Some(spec) = specs.get(&target) else {
            skipped += 1;
            continue;
        };
        let Some(list) = spec.api.read.list.as_ref() else {
            skipped += 1;
            continue;
        };
        let Some(operation) = list.op.as_ref() else {
            skipped += 1;
            continue;
        };
        if list.scope.is_some() {
            skipped += 1;
            continue;
        }
        let response = transport
            .call(OperationRequest::new(operation))
            .await
            .into_diagnostic()?;
        let rows = response
            .as_array()
            .ok_or_else(|| miette::miette!("{operation} returned a non-list selector catalog"))?;
        let key = spec
            .identity
            .key
            .as_ref()
            .and_then(|field| {
                spec.fields
                    .get(field)
                    .map(|entry| entry.request_name(field))
            })
            .ok_or_else(|| miette::miette!("{} has no selector identity field", spec.kind))?;
        let mut seen = BTreeSet::new();
        for row in rows {
            let name = row
                .get(key)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    miette::miette!("{operation} returned a selector without `{key}`")
                })?;
            if !seen.insert(name) {
                return Err(miette::miette!(
                    "{operation} returned duplicate selector name `{name}`"
                ));
            }
        }
        catalogs += 1;
        names += rows.len();
    }

    Ok((catalogs, names, skipped))
}

fn selector_targets(specs: &SpecRegistry) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for spec in specs.kinds() {
        for field in spec.fields.values() {
            collect_selector_targets(field, &mut targets);
        }
    }
    targets
}

fn collect_selector_targets(field: &Field, targets: &mut BTreeSet<String>) {
    if let Ok(ty) = parse_type(&field.ty) {
        match ty {
            FieldType::Selector(kind) => {
                targets.insert(kind);
            }
            _ => {
                if let Some(kind) = ty.selector_set_kind() {
                    targets.insert(kind.to_owned());
                }
            }
        }
    }
    for member in field.members.values() {
        collect_selector_targets(member, targets);
    }
    for member in field.arms.values().flat_map(BTreeMap::values) {
        collect_selector_targets(member, targets);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dokploy_sim::Sim;

    use super::*;

    #[tokio::test]
    async fn healthy_simulator_passes_every_check() {
        let specs = Arc::new(dokploy_spec::embedded().unwrap());
        let sim = Sim::new(Arc::clone(&specs));
        let mut output = Vec::new();

        let status = run(&sim, &specs, &mut output).await.unwrap();

        assert_eq!(status, CommandStatus::Success);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("API key and version: v0.30.6 (supported)"));
        assert!(output.contains("Organization:"));
        assert!(output.contains("Selector catalogs:"));
    }
}
