//! Every top-level kind with a full spec passes the conformance suite against the simulator.

use std::path::Path;

use dokploy_conformance::{Outcome, Suite, report};
use dokploy_spec::load_dir;

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[tokio::test]
async fn every_kind_with_a_full_spec_conforms() {
    let specs = load_dir(&root().join("specs")).expect("repository specs are valid");
    let suite = Suite::new(specs, root().join("fixtures/api/live/v0.30.6"));

    let results = suite.run_all().await;
    println!("{}", report(&results));

    let failures: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Fail(_)))
        .collect();
    assert!(failures.is_empty(), "{}", report(&results));
    for kind in suite.kinds() {
        let ran = results
            .iter()
            .filter(|r| {
                (r.kind == kind || r.kind.starts_with(&format!("{kind}@")))
                    && r.outcome == Outcome::Pass
            })
            .count();
        assert!(
            ran >= 10,
            "`{kind}` ran only {ran} scenarios: {}",
            report(&results)
        );
    }
}
