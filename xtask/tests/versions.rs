use std::fs;
use std::path::Path;

use tempfile::TempDir;
use xtask::{repository_root, run_versions_check, version_image};

const DIGEST: &str = "1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8";
const CANDIDATE_DIGEST: &str = "62e4354a49ee686ea749d3541d93113d494501befe81f5226c7f859f201e3a7c";

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn tree() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "specs/versions.yaml",
        &format!(
            "versions:
  - version: \"0.30.6\"
    status: supported
    image: dokploy/dokploy:v0.30.6@sha256:{DIGEST}
    openapi: openapi/dokploy.json
  - version: \"0.30.7\"
    status: candidate
    image: dokploy/dokploy:v0.30.7@sha256:{CANDIDATE_DIGEST}
"
        ),
    );
    write(root.path(), "openapi/dokploy.json", "{}");
    write(root.path(), "fixtures/api/live/v0.30.6/metadata.json", "{}");
    write(
        root.path(),
        "compose.integration.yaml",
        &format!("image: ${{DOKPLOY_IMAGE:-dokploy/dokploy:v0.30.6@sha256:{DIGEST}}}\n"),
    );
    write(
        root.path(),
        "scripts/integration/version.sh",
        &format!(
            "dokploy_default_version=\"v0.30.6\"\ndokploy_default_image=\"dokploy/dokploy:v0.30.6@sha256:{DIGEST}\"\n"
        ),
    );
    root
}

#[test]
fn a_consistent_tree_passes_and_lists_every_version() {
    let root = tree();
    let report = run_versions_check(root.path()).unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(report.table.contains("0.30.6") && report.table.contains("candidate"));
}

#[test]
fn the_image_of_a_candidate_is_resolvable() {
    let root = tree();
    assert_eq!(
        version_image(root.path(), "0.30.7").unwrap(),
        format!("dokploy/dokploy:v0.30.7@sha256:{CANDIDATE_DIGEST}")
    );
    assert!(version_image(root.path(), "1.0.0").is_err());
}

#[test]
fn a_drifted_default_pin_is_reported_for_each_place_that_repeats_it() {
    let root = tree();
    write(
        root.path(),
        "compose.integration.yaml",
        "image: dokploy/dokploy:v0.30.6@sha256:deadbeef\n",
    );
    write(
        root.path(),
        "scripts/integration/version.sh",
        &format!("dokploy_default_version=\"v0.30.5\"\n# {DIGEST}\n"),
    );
    // version.sh still names the digest in a comment, so only the tag is wrong there.
    let failures = run_versions_check(root.path()).unwrap().failures;
    assert!(
        failures
            .iter()
            .any(|f| f.starts_with("compose.integration.yaml"))
    );
    assert!(
        failures
            .iter()
            .any(|f| f.contains("dokploy_default_version is not v0.30.6"))
    );
}

#[test]
fn a_supported_version_without_fixtures_or_openapi_fails() {
    let root = tree();
    fs::remove_dir_all(root.path().join("fixtures")).unwrap();
    fs::remove_file(root.path().join("openapi/dokploy.json")).unwrap();
    let failures = run_versions_check(root.path()).unwrap().failures;
    assert!(
        failures
            .iter()
            .any(|f| f.contains("has no captured fixtures"))
    );
    assert!(failures.iter().any(|f| f.contains("does not exist")));
}

#[test]
fn the_repository_versions_are_consistent() {
    let report = run_versions_check(&repository_root()).unwrap();
    assert!(report.failures.is_empty(), "{:#?}", report.failures);
}
