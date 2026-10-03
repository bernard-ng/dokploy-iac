//! A project kind passes the same suite as a settings kind: ancestors are seeded, the create
//! carries what the create operation accepts, and the rest is written by an update right after.
//!
//! The `application` is exercised once per arm of its `source` union: the suite writes through
//! one arm at a time, so each arm's operation and members (and the secret of the Docker arm)
//! get the whole suite.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dokploy_conformance::{Outcome, Suite, report};
use dokploy_spec::{KindSpec, SpecRegistry, load_dir, parse_spec};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The repository's application with only one arm of its source.
fn application_with_arm(arm: &str) -> KindSpec {
    let text = std::fs::read_to_string(root().join("specs/project/application.yaml"))
        .expect("the spec exists");
    let mut spec = parse_spec(&text).expect("the spec parses");
    spec.coverage = dokploy_spec::Coverage::Full;
    let source = spec.fields.get_mut("source").expect("a source");
    source.arms.retain(|name, _| name == arm);
    assert_eq!(source.arms.len(), 1, "`{arm}` is an arm of the source");
    spec.write.iter_mut().for_each(|group| {
        if let dokploy_spec::WriteGroup::ByVariant { ops, .. } = group {
            ops.retain(|name, _| name == arm);
        }
    });
    spec
}

#[tokio::test]
async fn an_application_conforms_through_each_arm_of_its_source() {
    for arm in ["github", "gitlab", "bitbucket", "gitea", "git", "docker"] {
        conforms_through(arm).await;
    }
}

async fn conforms_through(arm: &str) {
    let repository = load_dir(&root().join("specs")).expect("repository specs are valid");
    let mut kinds: Vec<KindSpec> = repository
        .kinds()
        .filter(|spec| spec.kind != "application")
        .cloned()
        .collect();
    kinds.push(application_with_arm(arm));
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

/// Prototype kinds (partial coverage) that already conform for the fields they carry. They are
/// run by name because the suite enrols full-coverage kinds on its own.
#[tokio::test]
async fn the_environment_and_the_databases_conform_for_the_fields_they_carry() {
    let specs = load_dir(&root().join("specs")).expect("repository specs are valid");
    let suite = Suite::new(specs, root().join("fixtures/api/live/v0.30.6"));

    let mut results = Vec::new();
    for kind in [
        "environment",
        "postgres",
        "mysql",
        "mariadb",
        "mongo",
        "redis",
    ] {
        results.extend(suite.run(kind).await);
    }

    println!("{}", report(&results));
    let failures: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Fail(_)))
        .collect();
    assert!(failures.is_empty(), "{}", report(&results));
    for kind in [
        "environment",
        "postgres",
        "mysql",
        "mariadb",
        "mongo",
        "redis",
    ] {
        let ran = results
            .iter()
            .filter(|r| r.kind == kind && r.outcome == Outcome::Pass)
            .count();
        assert!(
            ran >= 25,
            "`{kind}` ran only {ran} scenarios: {}",
            report(&results)
        );
    }
}

/// A kind that lives under several parents is exercised under each of them.
#[tokio::test]
async fn a_mount_conforms_under_every_parent() {
    let specs = load_dir(&root().join("specs")).expect("repository specs are valid");
    let suite = Suite::new(specs, root().join("fixtures/api/live/v0.30.6"));

    let results = suite.run("mount").await;

    println!("{}", report(&results));
    let failures: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Fail(_)))
        .collect();
    assert!(failures.is_empty(), "{}", report(&results));
    for parent in [
        "application",
        "postgres",
        "mysql",
        "mariadb",
        "mongo",
        "redis",
    ] {
        let kind = format!("mount@{parent}");
        assert!(
            results
                .iter()
                .any(|r| r.kind == kind && r.outcome == Outcome::Pass),
            "`{kind}` did not run: {}",
            report(&results)
        );
    }
}
