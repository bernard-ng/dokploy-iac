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
