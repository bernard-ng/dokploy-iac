#[path = "support/backup.rs"]
mod fake;

use std::fs;
use std::path::Path;
use std::process::Command;

use fake::{Fake, NIGHTLY, SECRET_CANARY, config, seed_workspace};

fn fingerprint_key() -> String {
    format!("0199a0c8-2351-7c31-8899-2c8f81983ea5:{}", "0b".repeat(32))
}

fn run(directory: &Path, url: &str, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dokploy"))
        .current_dir(directory)
        .env("DOKPLOY_FINGERPRINT_KEY", fingerprint_key())
        .args(["--url", url, "--api-key", "test-api-key"])
        .args(arguments)
        .output()
        .expect("dokploy runs")
}

fn workspace(fake: &Fake) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(directory.path(), fake.instance(), Vec::new());
    fs::write(directory.path().join("dokploy.yaml"), config(NIGHTLY)).unwrap();
    directory
}

fn assert_no_external_material(text: &str) {
    for needle in [
        SECRET_CANARY,
        "destination-1",
        "destination-2",
        "destination-7",
    ] {
        assert!(!text.contains(needle), "{needle}");
    }
    assert!(
        !text.contains("offsite"),
        "destination names stay out of plans and diagnostics"
    );
}

fn output_text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_saved_plan_applies_while_the_destination_resolution_is_unchanged() {
    let fake = Fake::start();
    let directory = workspace(&fake);
    let url = fake.url.clone();

    let planned = run(directory.path(), &url, &["plan", "--out", "saved.json"]);
    assert!(planned.status.success(), "{}", output_text(&planned));
    let envelope = fs::read_to_string(directory.path().join("saved.json")).unwrap();
    assert_no_external_material(&envelope);
    assert_no_external_material(&output_text(&planned));
    assert_eq!(
        fake.count("POST /api/backup.create"),
        0,
        "planning is read-only"
    );

    let applied = run(
        directory.path(),
        &url,
        &["apply", "saved.json", "--auto-approve"],
    );

    assert!(applied.status.success(), "{}", output_text(&applied));
    assert!(String::from_utf8_lossy(&applied.stdout).contains("Apply complete: 1 change(s)."));
    assert_eq!(fake.count("POST /api/backup.create"), 1);
    assert!(
        fake.matching("POST /api/backup.create")[0].contains(r#""destinationId":"destination-1""#)
    );
    assert_no_external_material(&output_text(&applied));
    fake.assert_inert();
}

#[test]
fn a_saved_plan_is_refused_when_the_destination_is_recreated_under_the_same_name() {
    let fake = Fake::start();
    let directory = workspace(&fake);
    let url = fake.url.clone();
    let planned = run(directory.path(), &url, &["plan", "--out", "saved.json"]);
    assert!(planned.status.success(), "{}", output_text(&planned));
    // The byte-identical plan now names a different physical destination.
    fake.world().destinations[0].0 = "destination-7".to_owned();

    let applied = run(
        directory.path(),
        &url,
        &["apply", "saved.json", "--auto-approve"],
    );

    assert!(!applied.status.success());
    assert_no_external_material(&output_text(&applied));
    assert_eq!(fake.count("POST /api/backup.create"), 0);
    assert!(
        fake.requests()
            .iter()
            .all(|request| request.starts_with("GET /api/")),
        "a stale saved plan performs no mutation"
    );
    let state = fs::read_to_string(directory.path().join(".dokploy/state.json")).unwrap();
    assert!(!state.contains("backup.nightly"));
}

#[test]
fn a_saved_plan_is_refused_when_the_destination_becomes_ambiguous_or_disappears() {
    for change in [
        (|fake: &Fake| {
            fake.world()
                .destinations
                .push(("destination-9".to_owned(), "offsite".to_owned()));
        }) as fn(&Fake),
        |fake: &Fake| fake.world().destinations.clear(),
    ] {
        let fake = Fake::start();
        let directory = workspace(&fake);
        let url = fake.url.clone();
        let planned = run(directory.path(), &url, &["plan", "--out", "saved.json"]);
        assert!(planned.status.success(), "{}", output_text(&planned));
        change(&fake);

        let applied = run(
            directory.path(),
            &url,
            &["apply", "saved.json", "--auto-approve"],
        );

        assert!(!applied.status.success());
        assert_no_external_material(&output_text(&applied));
        assert_eq!(fake.count("POST /api/backup.create"), 0);
    }
}

#[test]
fn an_unresolved_destination_makes_planning_report_a_value_free_blocker() {
    let fake = Fake::start();
    fake.world().destinations.clear();
    let directory = workspace(&fake);
    let url = fake.url.clone();

    let planned = run(directory.path(), &url, &["plan", "--json"]);

    let text = output_text(&planned);
    assert!(text.contains("DOKPLAN019"), "{text}");
    assert!(text.contains("unmatched"), "{text}");
    assert_no_external_material(&text);
}
