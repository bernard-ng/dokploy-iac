//! A union is planned per member: its tag and the members of each arm. These tests run an
//! application's `source` against the simulator: the arm it names is the operation that writes
//! it, a member the document leaves out is not managed, `fallback` fills what a switch of arm
//! has no value for, and a secret in an arm never reaches disk.

mod common;

use std::path::Path;

use common::project::{ProjectWorld, sim};
use common::{compile, engine};
use dokploy_engine::ApplyError;
use serde_json::Value;

const PASSWORD: &str = "docker-password-canary-1";

fn document(source: &str) -> String {
    format!(
        "version: 2\nproject:\n  slug: shop\n  environments:\n    production:\n      applications:\n        web:\n          source: {source}\n"
    )
}

/// A document with one resource of `section` under the environment, with `field` set.
fn under_environment(section: &str, key: &str, fields: &str) -> String {
    format!(
        "version: 2\nproject:\n  slug: shop\n  environments:\n    production:\n      {section}:\n        {key}:\n{fields}"
    )
}

type World = ProjectWorld;

impl World {
    async fn apply(&self, source: &str) -> Result<dokploy_engine::ApplySummary, ApplyError> {
        self.apply_text(&document(source), &[("DOCKER_TOKEN", PASSWORD)])
            .await
    }

    /// How many changes the next plan holds.
    async fn pending(&self, source: &str) -> usize {
        let plan = self
            .plan_text(&document(source), &[("DOCKER_TOKEN", PASSWORD)])
            .await;
        assert!(plan.applyable(), "{:?}", plan.diagnostics());
        plan.changes().len()
    }

    fn application(&self) -> Value {
        let mut all = self.sim.objects("application");
        assert_eq!(all.len(), 1, "exactly one application: {all:?}");
        all.remove(0)
    }

    /// The operations that changed something, in order.
    fn writes(&self) -> Vec<String> {
        self.sim
            .mutations()
            .iter()
            .map(|logged| logged.operation().to_owned())
            .collect()
    }
}

const GITHUB: &str = "{ type: github, owner: acme, repository: shop, branch: main }";
const DOCKER: &str =
    "{ type: docker, image: acme/shop, username: ci, password: { env: DOCKER_TOKEN } }";

#[tokio::test]
async fn an_application_is_written_through_the_operation_of_the_arm_its_source_names() {
    let world = World::new();

    world.apply(GITHUB).await.expect("applies");

    let remote = world.application();
    assert_eq!(remote["sourceType"], "github");
    assert_eq!(remote["owner"], "acme");
    assert_eq!(remote["repository"], "shop");
    assert_eq!(remote["branch"], "main");
    assert!(
        world
            .writes()
            .contains(&"application.saveGithubProvider".to_owned()),
        "the github arm is written by its own operation: {:?}",
        world.writes()
    );
    assert_eq!(world.pending(GITHUB).await, 0, "the next plan is empty");
}

#[tokio::test]
async fn a_fallback_fills_a_member_the_document_leaves_out() {
    let world = World::new();

    world.apply(GITHUB).await.expect("applies");

    let remote = world.application();
    assert_eq!(remote["buildPath"], "/", "the member's fallback");
    assert_eq!(remote["triggerType"], "push", "the member's fallback");
}

#[tokio::test]
async fn a_member_the_document_leaves_out_keeps_what_dokploy_holds_while_the_arm_stays() {
    let world = World::new();
    world.apply(GITHUB).await.expect("applies");
    let id = world.application()["applicationId"]
        .as_str()
        .unwrap()
        .to_owned();
    // Someone sets the build path in Dokploy: the document does not own it.
    world.sim.patch(
        "application",
        &id,
        &serde_json::json!({ "buildPath": "/web" }),
    );

    assert_eq!(
        world.pending(GITHUB).await,
        0,
        "an unmanaged member is not drift"
    );

    // Changing a member the document does own is sent without disturbing the unmanaged one.
    let moved = "{ type: github, owner: acme, repository: shop, branch: release }";
    world.apply(moved).await.expect("applies");
    let remote = world.application();
    assert_eq!(remote["branch"], "release");
    assert_eq!(remote["buildPath"], "/web", "what Dokploy holds is kept");
}

#[tokio::test]
async fn switching_the_arm_writes_the_new_arm_and_the_secret_once() {
    let world = World::new();
    world.apply(GITHUB).await.expect("applies");
    world.sim.clear_requests();

    world.apply(DOCKER).await.expect("applies");

    let remote = world.application();
    assert_eq!(remote["sourceType"], "docker");
    assert_eq!(remote["dockerImage"], "acme/shop");
    assert_eq!(remote["username"], "ci");
    assert_eq!(remote["password"], PASSWORD, "the secret reached Dokploy");
    let sends = world
        .writes()
        .iter()
        .filter(|operation| *operation == "application.saveDockerProvider")
        .count();
    assert_eq!(sends, 1, "{:?}", world.writes());
    assert_eq!(world.pending(DOCKER).await, 0, "the next plan is empty");
}

#[tokio::test]
async fn switching_to_an_arm_takes_the_fallback_of_the_members_it_does_not_name() {
    let world = World::new();
    world.apply(DOCKER).await.expect("applies");

    let gitlab = "{ type: gitlab, owner: acme, repository: shop, branch: main }";
    world.apply(gitlab).await.expect("applies");

    let remote = world.application();
    assert_eq!(remote["sourceType"], "gitlab");
    assert_eq!(remote["gitlabBranch"], "main");
    assert_eq!(
        remote["gitlabBuildPath"], "/",
        "the fallback of a member the new arm has no value for"
    );
    assert_eq!(world.pending(gitlab).await, 0, "the next plan is empty");
}

#[tokio::test]
async fn a_secret_in_an_arm_reaches_neither_state_nor_the_journal() {
    let world = World::new();

    world.apply(DOCKER).await.expect("applies");

    let mut everything = String::new();
    fn walk(path: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                out.push_str(&text);
            }
        }
    }
    walk(world.directory.path(), &mut everything);
    assert!(!everything.is_empty(), "something was written");
    assert!(
        !everything.contains(PASSWORD),
        "the secret reached the workspace"
    );
}

#[test]
fn a_member_of_another_arm_is_refused_before_anything_is_planned() {
    let sim = sim();
    let engine = engine(&sim);

    let wrong = document("{ type: docker, image: acme/shop, owner: acme }");
    let error = engine
        .parse(&wrong)
        .expect_err("`owner` is a github member");
    assert!(error.to_string().contains("owner"), "{error}");

    let unknown = document("{ type: svn }");
    assert!(engine.parse(&unknown).is_err(), "`svn` is not an arm");
}

/// A union written by the same operation as the rest of the kind (no operation per arm): the
/// switch is a plain update that carries the tag and the members of the new arm.
#[tokio::test]
async fn a_union_written_by_the_plain_update_switches_arm_with_the_new_members() {
    let world = World::new();
    let primary = under_environment(
        "libsql",
        "edge",
        "          username: edge\n          password: { env: DOCKER_TOKEN }\n          image: ghcr.io/tursodatabase/libsql-server:latest\n          enable_namespaces: false\n          node: { type: primary }\n",
    );
    let replica = under_environment(
        "libsql",
        "edge",
        "          username: edge\n          password: { env: DOCKER_TOKEN }\n          image: ghcr.io/tursodatabase/libsql-server:latest\n          enable_namespaces: false\n          node: { type: replica, primary_url: \"https://primary.internal\" }\n",
    );
    let engine = world.engine();
    let store = world.store(&engine);

    let compiled = compile(&engine, &primary, &[("DOCKER_TOKEN", PASSWORD)]);
    engine
        .apply(&compiled, &store, |_| true)
        .await
        .expect("applies");
    let remote = world.sim.objects("libsql").remove(0);
    assert_eq!(remote["sqldNode"], "primary");
    assert!(
        remote["sqldPrimaryUrl"].is_null(),
        "a primary has no upstream"
    );

    let compiled = compile(&engine, &replica, &[("DOCKER_TOKEN", PASSWORD)]);
    engine
        .apply(&compiled, &store, |_| true)
        .await
        .expect("applies");
    let remote = world.sim.objects("libsql").remove(0);
    assert_eq!(remote["sqldNode"], "replica");
    assert_eq!(remote["sqldPrimaryUrl"], "https://primary.internal");
    assert_eq!(
        world.sim.objects("libsql").len(),
        1,
        "the switch updated the database, it did not replace it"
    );

    let plan = engine
        .plan(&compiled, store.inspect().unwrap().as_ref())
        .await
        .unwrap();
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn a_compose_source_is_switched_from_the_compose_file_to_a_repository() {
    let world = World::new();
    let raw = under_environment(
        "compose",
        "stack",
        "          source: { type: raw, document: { file: stack.yaml } }\n",
    );
    let github = under_environment(
        "compose",
        "stack",
        "          source: { type: github, owner: acme, repository: shop, branch: main }\n          compose_path: ./deploy/compose.yml\n",
    );
    let engine = world.engine();
    let store = world.store(&engine);
    let workspace = std::env::temp_dir();
    std::fs::write(workspace.join("stack.yaml"), "services: {}\n").unwrap();

    let compiled = compile(&engine, &raw, &[]);
    engine
        .apply(&compiled, &store, |_| true)
        .await
        .expect("applies");
    assert_eq!(world.sim.objects("compose")[0]["sourceType"], "raw");

    let compiled = compile(&engine, &github, &[]);
    engine
        .apply(&compiled, &store, |_| true)
        .await
        .expect("applies");
    let remote = world.sim.objects("compose").remove(0);
    assert_eq!(remote["sourceType"], "github");
    assert_eq!(remote["owner"], "acme");
    assert_eq!(remote["repository"], "shop");
    assert_eq!(remote["composePath"], "./deploy/compose.yml");
}
