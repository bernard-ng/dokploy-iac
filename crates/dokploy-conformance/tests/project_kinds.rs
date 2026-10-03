//! A project kind passes the same suite as a settings kind: ancestors are seeded, the create
//! carries what the create operation accepts, and the rest is written by an update right after.
//!
//! The repository's `application` spec is partial until the composite values (union, env) are
//! written by the executor, so this runs a trimmed copy of it: the same operations and fields
//! minus those two. It goes away when `application` itself conforms.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dokploy_conformance::{Outcome, Suite, report};
use dokploy_spec::{KindSpec, SpecRegistry, load_dir, parse_spec};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn trimmed_application() -> KindSpec {
    let text = std::fs::read_to_string(root().join("specs/project/application.yaml"))
        .expect("the spec exists");
    let mut spec = parse_spec(&text).expect("the spec parses");
    spec.coverage = dokploy_spec::Coverage::Full;
    for composite in ["environment", "source"] {
        spec.fields.remove(composite);
    }
    spec.write.retain(|group| match group {
        dokploy_spec::WriteGroup::Op { fields, .. } => {
            !fields.iter().any(|f| f == "environment" || f == "source")
        }
        dokploy_spec::WriteGroup::ByVariant { .. } => false,
    });
    spec.ledger.derived.clear();
    spec
}

#[tokio::test]
async fn an_application_conforms_with_a_follow_up_update_after_its_create() {
    let repository = load_dir(&root().join("specs")).expect("repository specs are valid");
    let mut kinds: Vec<KindSpec> = repository
        .kinds()
        .filter(|spec| spec.kind != "application")
        .cloned()
        .collect();
    kinds.push(trimmed_application());
    let specs = SpecRegistry::from_specs(kinds, &BTreeSet::new()).expect("consistent specs");
    let suite = Suite::new(specs, root().join("fixtures/api/live/v0.30.6"));

    let results = suite.run("application").await;

    println!("{}", report(&results));
    let failures: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Fail(_)))
        .collect();
    assert!(failures.is_empty(), "{}", report(&results));
    for scenario in [
        "create",
        "follow_up_rejected",
        "follow_up_lost_before",
        "follow_up_lost_after",
        "update:registry",
        "drift:build_registry",
        "selector_unmatched",
        "selector_ambiguous",
    ] {
        assert!(
            results
                .iter()
                .any(|r| r.scenario == scenario && r.outcome == Outcome::Pass),
            "`{scenario}` did not run: {}",
            report(&results)
        );
    }
}
