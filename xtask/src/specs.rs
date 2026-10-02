//! `cargo xtask specs --check`: load every kind spec and verify it against the
//! vendored OpenAPI document (the request side of the coverage ledger, ADR 0002).

use std::fmt::Write as _;
use std::path::Path;

use dokploy_spec::{Coverage, KindLedger, OperationIndex, check_ledger, load_dir};

/// The outcome of a specs check.
#[derive(Debug)]
pub struct SpecsReport {
    /// A printable per-kind table.
    pub table: String,
    /// Failures; empty when the check passed.
    pub failures: Vec<String>,
}

/// Loads `specs/` and checks it against `openapi/dokploy.json` below `root`.
pub fn run_specs_check(root: &Path) -> Result<SpecsReport, Box<dyn std::error::Error>> {
    let registry = match load_dir(&root.join("specs")) {
        Ok(registry) => registry,
        Err(error) => {
            return Ok(SpecsReport {
                table: String::new(),
                failures: vec![error.to_string()],
            });
        }
    };
    let openapi: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("openapi/dokploy.json"))?)?;
    let report = check_ledger(&registry, &OperationIndex::from_openapi(&openapi));

    let mut table = String::new();
    writeln!(
        table,
        "{:<14} {:<8} {:>6} {:>8} {:>6} {:>7} {:>7} {:>13}",
        "kind", "coverage", "mapped", "readonly", "action", "ignored", "derived", "unclassified"
    )?;
    for ledger in &report.kinds {
        write_row(&mut table, ledger)?;
    }
    writeln!(
        table,
        "response side: not checked (no recorded live fixtures yet, ADR 0007)"
    )?;

    Ok(SpecsReport {
        table,
        failures: report.issues.iter().map(ToString::to_string).collect(),
    })
}

fn write_row(table: &mut String, ledger: &KindLedger) -> std::fmt::Result {
    let coverage = match ledger.coverage {
        Coverage::Full => "full",
        Coverage::Partial => "partial",
    };
    writeln!(
        table,
        "{:<14} {:<8} {:>6} {:>8} {:>6} {:>7} {:>7} {:>13}",
        ledger.kind,
        coverage,
        ledger.mapped,
        ledger.readonly,
        ledger.action,
        ledger.ignored,
        ledger.derived,
        ledger.unclassified.len()
    )
}
