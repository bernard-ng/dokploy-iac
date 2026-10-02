//! The repository's own specs must load and satisfy the request-side ledger.

use std::path::Path;

use dokploy_spec::{OperationIndex, check_ledger, load_dir};

fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate lives two levels below the repository root")
}

#[test]
fn repository_specs_load_and_pass_the_request_ledger() {
    let root = repository_root();
    let registry = load_dir(&root.join("specs")).expect("repository specs are valid");
    let openapi: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("openapi/dokploy.json")).expect("OpenAPI is vendored"),
    )
    .expect("OpenAPI is JSON");
    let report = check_ledger(&registry, &OperationIndex::from_openapi(&openapi));

    assert!(
        report.is_clean(),
        "ledger issues:\n{}",
        report
            .issues
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    let registry_ledger = report
        .kinds
        .iter()
        .find(|kind| kind.kind == "registry")
        .expect("registry is covered");
    assert!(
        registry_ledger.unclassified.is_empty(),
        "a full-coverage kind classifies every request field"
    );
}
