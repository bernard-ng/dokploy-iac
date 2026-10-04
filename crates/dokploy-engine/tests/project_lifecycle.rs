//! The root of a project document: the generic conformance suite cannot write a project root
//! document, so its lifecycle is exercised here against the simulator: create with its variables
//! and tags, converge, update in place, and destroy.

mod common;

use common::project::ProjectWorld;
use common::{compile, engine};
use serde_json::json;

fn project(description: &str, env: &str, tags: &str) -> String {
    format!(
        "version: 2\nproject:\n  slug: shop\n  description: {description}\n{env}{tags}  environments:\n    staging: {{}}\n"
    )
}

#[tokio::test]
async fn a_project_is_created_with_its_variables_and_tags_and_the_next_plan_is_empty() {
    let world = ProjectWorld::bare();
    let prod = world.sim.seed("tag", &json!({ "name": "prod" }));
    let document = project(
        "Storefront",
        "  env:\n    LOG_LEVEL: info\n",
        "  tags: [{ name: prod }]\n",
    );

    let summary = world.apply_text(&document, &[]).await.expect("applies");

    assert!(summary.applied() >= 2, "the project and its environment");
    let created = world.sim.objects("project").remove(0);
    assert_eq!(created["name"], "shop");
    assert_eq!(created["description"], "Storefront");
    assert!(
        created["env"].as_str().unwrap().contains("LOG_LEVEL=info"),
        "{created}"
    );
    assert_eq!(created["projectTags"], json!([{ "tagId": prod }]));
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.applyable(), "{:?}", plan.diagnostics());
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn a_changed_description_is_one_update_that_carries_only_the_description() {
    let world = ProjectWorld::bare();
    let before = project("Storefront", "", "");
    world.apply_text(&before, &[]).await.expect("applies");
    world.sim.clear_requests();

    let after = project("The shop", "", "");
    world.apply_text(&after, &[]).await.expect("applies");

    let mutations = world.sim.mutations();
    assert_eq!(mutations.len(), 1, "{:?}", world.sim.requests().len());
    assert_eq!(mutations[0].operation(), "project.update");
    let body = mutations[0].body().cloned().unwrap();
    let mut keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["description", "projectId"], "{body}");
    assert_eq!(world.sim.objects("project")[0]["description"], "The shop");
}

#[tokio::test]
async fn a_cleared_document_destroys_the_project_and_what_state_tracks() {
    let world = ProjectWorld::bare();
    let document = project("Storefront", "", "");
    world.apply_text(&document, &[]).await.expect("applies");
    assert_eq!(world.sim.objects("project").len(), 1);

    let engine = engine(&world.sim);
    let store = world.store(&engine);
    let compiled = compile(&engine, &document, &[]);
    engine
        .apply(&compiled.cleared(), &store, |_| true)
        .await
        .expect("destroys");

    assert!(world.sim.objects("project").is_empty());
    assert!(store.inspect().unwrap().unwrap().resources().is_empty());
}

fn with_environments(environments: &str) -> String {
    format!("version: 2\nproject:\n  slug: shop\n  environments:\n{environments}")
}

fn environment_creates(world: &ProjectWorld) -> usize {
    world
        .sim
        .mutations()
        .iter()
        .filter(|logged| logged.operation() == "environment.create")
        .count()
}

#[tokio::test]
async fn a_production_environment_in_a_new_project_is_the_one_dokploy_made() {
    let world = ProjectWorld::bare();
    let document = with_environments(
        "    production:\n      description: Storefront\n      env:\n        REGION: eu\n    staging: {}\n",
    );

    world.apply_text(&document, &[]).await.expect("applies");

    let environments = world.sim.objects("environment");
    let names: Vec<&str> = environments
        .iter()
        .map(|environment| environment["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "no second `production`: {names:?}");
    assert!(names.contains(&"production") && names.contains(&"staging"));
    assert_eq!(
        environment_creates(&world),
        1,
        "only `staging` is created; `production` is taken"
    );
    let production = environments
        .iter()
        .find(|environment| environment["name"] == "production")
        .unwrap();
    assert_eq!(production["isDefault"], true, "it is the one Dokploy made");
    assert_eq!(production["description"], "Storefront", "and it is written");
    assert!(production["env"].as_str().unwrap().contains("REGION=eu"));
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.applyable(), "{:?}", plan.diagnostics());
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn production_added_to_a_tracked_project_takes_the_default_one_instead_of_blocking() {
    let world = ProjectWorld::bare();
    let before = with_environments("    staging: {}\n");
    world.apply_text(&before, &[]).await.expect("applies");
    assert_eq!(
        world.sim.objects("environment").len(),
        2,
        "staging and the default"
    );
    world.sim.clear_requests();

    let after = with_environments("    production:\n      description: Live\n    staging: {}\n");
    let plan = world.plan_text(&after, &[]).await;
    assert!(
        plan.applyable(),
        "the default environment is not an unmanaged collision: {:?}",
        plan.diagnostics()
    );
    world.apply_text(&after, &[]).await.expect("applies");

    assert_eq!(environment_creates(&world), 0, "nothing is created");
    assert_eq!(world.sim.objects("environment").len(), 2);
    let production = world
        .sim
        .objects("environment")
        .into_iter()
        .find(|environment| environment["name"] == "production")
        .unwrap();
    assert_eq!(production["description"], "Live");
    assert!(world.plan_text(&after, &[]).await.changes().is_empty());
}

#[tokio::test]
async fn an_environment_that_is_not_the_default_still_blocks_when_it_is_not_ours() {
    let world = ProjectWorld::bare();
    world
        .apply_text(&with_environments("    staging: {}\n"), &[])
        .await
        .expect("applies");
    let project = world.sim.objects("project").remove(0)["projectId"].clone();
    // Someone makes `qa` in the dashboard: it is not what Dokploy made with the project.
    world.sim.seed(
        "environment",
        &json!({ "name": "qa", "projectId": project, "isDefault": false }),
    );

    let plan = world
        .plan_text(&with_environments("    qa: {}\n    staging: {}\n"), &[])
        .await;

    assert!(!plan.applyable(), "an unmanaged `qa` blocks the plan");
}

#[tokio::test]
async fn a_project_without_production_leaves_the_default_environment_alone() {
    let world = ProjectWorld::bare();
    let document = with_environments("    staging: {}\n");

    world.apply_text(&document, &[]).await.expect("applies");

    assert_eq!(environment_creates(&world), 1, "only `staging`");
    assert!(world.plan_text(&document, &[]).await.changes().is_empty());
}

#[tokio::test]
async fn the_default_environment_is_forgotten_not_deleted_and_goes_with_its_project() {
    let world = ProjectWorld::bare();
    let both = with_environments("    production: {}\n    staging: {}\n");
    world.apply_text(&both, &[]).await.expect("applies");
    world.sim.clear_requests();

    // `production` leaves the document: Dokploy would refuse to delete it, so it is only forgotten.
    let only_staging = with_environments("    staging: {}\n");
    world.apply_text(&only_staging, &[]).await.expect("applies");

    assert_eq!(
        world
            .sim
            .mutations()
            .iter()
            .filter(|logged| logged.operation() == "environment.remove")
            .count(),
        0,
        "no delete was sent"
    );
    assert_eq!(
        world.sim.objects("environment").len(),
        2,
        "it is still there"
    );
    assert!(
        world
            .plan_text(&only_staging, &[])
            .await
            .changes()
            .is_empty()
    );

    // Destroying the project removes everything below it.
    let engine = engine(&world.sim);
    let store = world.store(&engine);
    let compiled = compile(&engine, &both, &[]);
    engine
        .apply(&compiled.cleared(), &store, |_| true)
        .await
        .expect("destroys");
    assert!(world.sim.objects("project").is_empty());
    assert!(world.sim.objects("environment").is_empty());
}
