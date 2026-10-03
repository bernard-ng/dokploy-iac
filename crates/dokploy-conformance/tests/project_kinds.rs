//! A project kind passes the same suite as a settings kind: ancestors are seeded, the create
//! carries what the create operation accepts, and the rest is written by an update right after.
//!
//! A kind with a union is exercised once per arm of it: the suite writes through one arm at a
//! time, so each arm's members (and the secret of the Docker arm of an application's `source`, and
//! the operation of each arm) get the whole suite.

use std::path::{Path, PathBuf};

use dokploy_conformance::{Outcome, Suite, report};
use dokploy_spec::load_dir;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[tokio::test]
async fn an_application_conforms_through_each_arm_of_its_source() {
    for arm in ["github", "gitlab", "bitbucket", "gitea", "git", "docker"] {
        conforms_through("application", "source", arm, &APPLICATION_SCENARIOS).await;
    }
}

#[tokio::test]
async fn an_application_conforms_through_each_arm_of_its_build() {
    for arm in [
        "dockerfile",
        "heroku_buildpacks",
        "paketo_buildpacks",
        "nixpacks",
        "static",
        "railpack",
    ] {
        conforms_through("application", "build", arm, &BUILD_SCENARIOS).await;
    }
}

#[tokio::test]
async fn a_compose_conforms_through_each_arm_of_its_source() {
    for arm in ["raw", "github", "gitlab", "bitbucket", "gitea", "git"] {
        conforms_through("compose", "source", arm, &COMPOSE_SCENARIOS).await;
    }
}

#[tokio::test]
async fn a_libsql_conforms_through_each_arm_of_its_node() {
    conforms_through("libsql", "node", "primary", &LIBSQL_SCENARIOS).await;
    let replica: Vec<&str> = LIBSQL_SCENARIOS
        .iter()
        .chain(&LIBSQL_REPLICA_SCENARIOS)
        .copied()
        .collect();
    conforms_through("libsql", "node", "replica", &replica).await;
}

const APPLICATION_SCENARIOS: [&str; 8] = [
    "create",
    "follow_up_rejected",
    "follow_up_lost_before",
    "follow_up_lost_after",
    "update:registry",
    "drift:build_registry",
    "selector_unmatched",
    "selector_ambiguous",
];

const BUILD_SCENARIOS: [&str; 5] = [
    "create",
    "follow_up_rejected",
    "follow_up_lost_before",
    "follow_up_lost_after",
    "update:registry",
];

const COMPOSE_SCENARIOS: [&str; 5] = [
    "create",
    "follow_up_rejected",
    "follow_up_lost_before",
    "follow_up_lost_after",
    "delete",
];

const LIBSQL_SCENARIOS: [&str; 5] = [
    "create",
    "follow_up_rejected",
    "follow_up_lost_before",
    "follow_up_lost_after",
    "identity_ambiguous",
];

/// What only the replica arm has: a member to change and to drift.
const LIBSQL_REPLICA_SCENARIOS: [&str; 2] = [
    "update:node.replica.primary_url",
    "drift:node.replica.primary_url",
];

async fn conforms_through(kind: &str, field: &str, arm: &str, required: &[&str]) {
    let specs = load_dir(&root().join("specs")).expect("repository specs are valid");
    let suite = Suite::new(specs, root().join("fixtures/api/live/v0.30.6")).exercising(field, arm);

    let results = suite.run(kind).await;

    println!("{}", report(&results));
    let failures: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Fail(_)))
        .collect();
    assert!(failures.is_empty(), "{}", report(&results));
    for scenario in required {
        assert!(
            results
                .iter()
                .any(|r| r.scenario == *scenario && r.outcome == Outcome::Pass),
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
        "libsql",
        "compose",
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
        "libsql",
        "compose",
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
        "libsql",
        "compose",
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
