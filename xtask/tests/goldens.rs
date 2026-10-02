use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;
use xtask::{repository_root, run_goldens_check, run_goldens_extract};

const LEGACY_TEST: &str = r##"
const BODY: &str = r#"{"redirectId":"redirect-1"}"#;
const FIXTURE: &str = include_str!("../../../fixtures/api/live/one.json");

fn helper() -> &'static str {
    "GET /api/application.one?applicationId=a HTTP/1.1"
}

#[tokio::test]
async fn redirect_create_sends_one_request() {
    let first = "POST /api/redirects.create HTTP/1.1\r\n";
    let _ = (first, BODY, FIXTURE, helper());
}

#[test]
#[ignore = "needs a local Dokploy"]
fn live_redirect_converges() {}

mod nested {
    #[test]
    fn drift_is_detected() {}
}

fn not_a_test() {}
"##;

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn sources(entries: Value, excluded: Value) -> String {
    json!({"sources": entries, "excluded": excluded}).to_string()
}

fn tree() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "crates/dokploy-cli/tests/redirect_remote.rs",
        LEGACY_TEST,
    );
    write(
        root.path(),
        "goldens/sources.json",
        &sources(
            json!([{
                "path": "crates/dokploy-cli/tests/redirect_remote.rs",
                "bucket": "redirect",
                "layer": "cli-remote"
            }]),
            json!([]),
        ),
    );
    root
}

fn ledger(root: &Path, bucket: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(root.join(format!("goldens/{bucket}.json"))).unwrap())
        .unwrap()
}

fn scenario<'a>(ledger: &'a Value, test: &str) -> &'a Value {
    ledger["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scenario| scenario["test"] == test)
        .unwrap_or_else(|| panic!("no scenario for {test}"))
}

fn problems(root: &Path) -> Vec<String> {
    run_goldens_check(root).unwrap().failures
}

#[test]
fn extraction_mines_requests_fixtures_data_and_nested_tests() {
    let root = tree();
    let report = run_goldens_extract(root.path()).unwrap();
    assert_eq!((report.scenarios, report.added, report.files), (3, 3, 1));

    let ledger = ledger(root.path(), "redirect");
    assert_eq!(ledger["bucket"], "redirect");

    let create = scenario(&ledger, "redirect_create_sends_one_request");
    assert_eq!(
        create["id"],
        "dokploy-cli/tests/redirect_remote::redirect_create_sends_one_request"
    );
    assert_eq!(
        create["requests"],
        json!([
            "POST /api/redirects.create HTTP/1.1",
            "GET /api/application.one?applicationId=a HTTP/1.1"
        ]),
        "helper literals are inherited"
    );
    assert_eq!(
        create["operations"],
        json!(["application.one", "redirects.create"])
    );
    assert_eq!(create["fixtures"], json!(["fixtures/api/live/one.json"]));
    assert_eq!(create["data"], json!(["BODY"]));
    assert_eq!(create["category"], "create");
    assert_eq!(create["status"], "pending");
    assert_eq!(create["live"], false);

    let live = scenario(&ledger, "live_redirect_converges");
    assert_eq!(
        (live["live"].clone(), live["category"].clone()),
        (json!(true), json!("live"))
    );

    let nested = scenario(&ledger, "drift_is_detected");
    assert_eq!(
        nested["id"],
        "dokploy-cli/tests/redirect_remote::nested::drift_is_detected"
    );
    assert_eq!(nested["category"], "drift");
}

#[test]
fn a_fresh_extraction_passes_the_check() {
    let root = tree();
    run_goldens_extract(root.path()).unwrap();
    let report = run_goldens_check(root.path()).unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(report.table.contains("redirect"));
}

#[test]
fn re_extraction_keeps_the_classification() {
    let root = tree();
    run_goldens_extract(root.path()).unwrap();
    let path = root.path().join("goldens/redirect.json");
    let mut value = ledger(root.path(), "redirect");
    for scenario in value["scenarios"].as_array_mut().unwrap() {
        if scenario["test"] == "drift_is_detected" {
            scenario["status"] = json!("covered");
            scenario["covered_by"] = json!("conformance.redirect.drift");
            scenario["category"] = json!("noop");
        }
    }
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    write(
        root.path(),
        "crates/dokploy-cli/tests/redirect_remote.rs",
        &format!("{LEGACY_TEST}\n#[test]\nfn added_later() {{}}\n"),
    );
    let report = run_goldens_extract(root.path()).unwrap();
    assert_eq!((report.scenarios, report.added), (4, 1));

    let value = ledger(root.path(), "redirect");
    let kept = scenario(&value, "drift_is_detected");
    assert_eq!(kept["status"], "covered");
    assert_eq!(kept["covered_by"], "conformance.redirect.drift");
    assert_eq!(kept["category"], "noop");
    assert_eq!(scenario(&value, "added_later")["status"], "pending");
    assert!(problems(root.path()).is_empty());
}

#[test]
fn a_new_legacy_test_must_enter_the_ledger() {
    let root = tree();
    run_goldens_extract(root.path()).unwrap();
    write(
        root.path(),
        "crates/dokploy-cli/tests/redirect_remote.rs",
        &format!("{LEGACY_TEST}\n#[test]\nfn added_later() {{}}\n"),
    );
    let failures = problems(root.path());
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("added_later") && failures[0].contains("missing from the ledger"));
}

#[test]
fn a_changed_test_makes_its_entry_stale() {
    let root = tree();
    run_goldens_extract(root.path()).unwrap();
    write(
        root.path(),
        "crates/dokploy-cli/tests/redirect_remote.rs",
        &LEGACY_TEST.replace("redirects.create", "redirects.update"),
    );
    let failures = problems(root.path());
    assert!(
        failures.iter().any(|f| f.contains("out of date")),
        "{failures:?}"
    );
}

#[test]
fn a_test_file_nobody_mapped_fails() {
    let root = tree();
    run_goldens_extract(root.path()).unwrap();
    write(
        root.path(),
        "crates/dokploy-sdk/tests/new.rs",
        "#[test]\nfn x() {}\n",
    );
    let failures = problems(root.path());
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].starts_with("crates/dokploy-sdk/tests/new.rs"));

    write(
        root.path(),
        "goldens/sources.json",
        &sources(
            json!([{
                "path": "crates/dokploy-cli/tests/redirect_remote.rs",
                "bucket": "redirect",
                "layer": "cli-remote"
            }]),
            json!([{"path": "crates/dokploy-sdk/tests/", "reason": "kept"}]),
        ),
    );
    assert!(problems(root.path()).is_empty());
}

#[test]
fn deleting_a_pending_test_is_refused_until_it_is_classified() {
    let root = tree();
    run_goldens_extract(root.path()).unwrap();
    fs::remove_file(
        root.path()
            .join("crates/dokploy-cli/tests/redirect_remote.rs"),
    )
    .unwrap();

    let failures = problems(root.path());
    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(failures.iter().all(|f| f.contains("still pending")));
    assert!(run_goldens_extract(root.path()).is_err());

    let path = root.path().join("goldens/redirect.json");
    let mut value = ledger(root.path(), "redirect");
    for (index, scenario) in value["scenarios"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        if index == 0 {
            scenario["status"] = json!("covered");
            scenario["covered_by"] = json!("conformance.redirect.create");
        } else {
            scenario["status"] = json!("dropped");
            scenario["reason"] = json!("live-only wiring the simulator replaces");
        }
    }
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    assert!(problems(root.path()).is_empty());
    assert_eq!(run_goldens_extract(root.path()).unwrap().scenarios, 3);
}

#[test]
fn classification_fields_must_be_consistent() {
    let root = tree();
    run_goldens_extract(root.path()).unwrap();
    let path = root.path().join("goldens/redirect.json");
    let mut value = ledger(root.path(), "redirect");
    let scenarios = value["scenarios"].as_array_mut().unwrap();
    scenarios[0]["status"] = json!("covered");
    scenarios[1]["status"] = json!("dropped");
    scenarios[2]["category"] = json!("nonsense");
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let failures = problems(root.path());
    assert!(
        failures
            .iter()
            .any(|f| f.contains("covered without `covered_by`")),
        "{failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|f| f.contains("dropped without a `reason`")),
        "{failures:?}"
    );
    assert!(
        failures.iter().any(|f| f.contains("unknown category")),
        "{failures:?}"
    );
}

#[test]
fn the_repository_ledger_is_current() {
    let report = run_goldens_check(&repository_root()).unwrap();
    assert!(report.failures.is_empty(), "{:#?}", report.failures);
}
