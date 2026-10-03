//! The tags of a project are a relation Dokploy changes one member at a time: each tag the
//! document names is assigned with its own request, each it stops naming is removed with its own,
//! a tag assigned elsewhere is left alone, and a read that does not look like what was captured
//! blocks planning instead of being guessed at.

mod common;

use common::project::ProjectWorld;
use dokploy_core::PlanDiagnosticCode;
use serde_json::json;

fn project(tags: &str) -> String {
    format!("version: 2\nproject:\n  slug: shop\n{tags}  environments:\n    production: {{}}\n")
}

fn tags(names: &[&str]) -> String {
    let entries: Vec<String> = names
        .iter()
        .map(|name| format!("{{ name: {name} }}"))
        .collect();
    format!("  tags: [{}]\n", entries.join(", "))
}

fn tag(world: &ProjectWorld, name: &str) -> String {
    world.sim.seed("tag", &json!({ "name": name }))
}

fn project_id(world: &ProjectWorld) -> String {
    world.sim.objects("project").remove(0)["projectId"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn assigned(world: &ProjectWorld) -> Vec<String> {
    let mut ids: Vec<String> = world.sim.objects("project").remove(0)["projectTags"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["tagId"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

fn operations(world: &ProjectWorld) -> Vec<String> {
    world
        .sim
        .mutations()
        .iter()
        .map(|logged| logged.operation().to_owned())
        .collect()
}

#[tokio::test]
async fn each_tag_the_document_names_is_assigned_with_a_request_of_its_own() {
    let world = ProjectWorld::new();
    let prod = tag(&world, "prod");
    let critical = tag(&world, "critical");
    let document = project(&tags(&["prod", "critical"]));

    world.apply_text(&document, &[]).await.expect("applies");

    let mut expected = vec![prod, critical];
    expected.sort();
    assert_eq!(assigned(&world), expected);
    assert_eq!(
        operations(&world),
        ["tag.assignToProject", "tag.assignToProject"]
    );
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.applyable(), "{:?}", plan.diagnostics());
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn a_tag_the_document_stops_naming_is_removed_and_a_new_one_is_assigned() {
    let world = ProjectWorld::new();
    let prod = tag(&world, "prod");
    let critical = tag(&world, "critical");
    let beta = tag(&world, "beta");
    world
        .apply_text(&project(&tags(&["prod", "critical"])), &[])
        .await
        .expect("applies");
    world.sim.clear_requests();

    let document = project(&tags(&["prod", "beta"]));
    world.apply_text(&document, &[]).await.expect("applies");

    let held = assigned(&world);
    assert!(held.contains(&prod) && held.contains(&beta), "{held:?}");
    assert!(!held.contains(&critical), "{held:?}");
    assert_eq!(
        operations(&world),
        ["tag.removeFromProject", "tag.assignToProject"],
        "one removal and one addition, and `prod` is not touched"
    );
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn a_tag_assigned_elsewhere_is_not_the_documents_to_remove() {
    let world = ProjectWorld::new();
    let prod = tag(&world, "prod");
    let legacy = tag(&world, "legacy");
    let document = project(&tags(&["prod"]));
    world.apply_text(&document, &[]).await.expect("applies");
    world.sim.patch(
        "project",
        &project_id(&world),
        &json!({ "projectTags": [{ "tagId": prod }, { "tagId": legacy }] }),
    );

    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.changes().is_empty(), "not drift: {plan:?}");

    let beta = tag(&world, "beta");
    world
        .apply_text(&project(&tags(&["prod", "beta"])), &[])
        .await
        .expect("applies");
    let held = assigned(&world);
    assert!(
        held.contains(&legacy),
        "what the document does not own stays: {held:?}"
    );
    assert!(held.contains(&beta), "{held:?}");
}

#[tokio::test]
async fn an_empty_list_removes_every_tag() {
    let world = ProjectWorld::new();
    tag(&world, "prod");
    tag(&world, "critical");
    world
        .apply_text(&project(&tags(&["prod", "critical"])), &[])
        .await
        .expect("applies");
    world.sim.clear_requests();

    let document = project("  tags: []\n");
    world.apply_text(&document, &[]).await.expect("applies");

    assert!(assigned(&world).is_empty());
    assert_eq!(
        operations(&world),
        ["tag.removeFromProject", "tag.removeFromProject"]
    );
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn a_tag_that_does_not_exist_blocks_the_plan() {
    let world = ProjectWorld::new();

    let plan = world.plan_text(&project(&tags(&["nowhere"])), &[]).await;

    assert!(!plan.applyable());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|d| d.code() == PlanDiagnosticCode::UnresolvedExternalSelector),
        "{:?}",
        plan.diagnostics()
    );
    assert!(world.sim.mutations().is_empty());
}

#[tokio::test]
async fn an_assigned_tag_that_does_not_look_like_the_capture_blocks_planning_and_writes_nothing() {
    let world = ProjectWorld::new();
    tag(&world, "prod");
    // A shape the capture never showed: the id is not under `tagId`.
    world.sim.patch(
        "project",
        &project_id(&world),
        &json!({ "projectTags": [{ "somethingElse": "x" }] }),
    );

    let plan = world.plan_text(&project(&tags(&["prod"])), &[]).await;

    assert!(!plan.applyable(), "{plan:?}");
    assert!(world.sim.mutations().is_empty());
}
