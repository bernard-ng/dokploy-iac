//! A set of selectors is planned per member: the networks a service attaches to are each a
//! property of their own, resolved to the id Dokploy holds, and written as one array of ids that
//! keeps every attachment the document does not own.

mod common;

use common::project::ProjectWorld;
use dokploy_core::PlanDiagnosticCode;
use serde_json::{Value, json};

fn app(networks: &str) -> String {
    format!(
        "version: 2\nproject:\n  slug: shop\n  environments:\n    production:\n      applications:\n        web:\n          replicas: 1\n{networks}"
    )
}

fn networks(names: &[&str]) -> String {
    let entries: Vec<String> = names
        .iter()
        .map(|name| format!("{{ name: {name} }}"))
        .collect();
    format!("          networks: [{}]\n", entries.join(", "))
}

/// Puts a network in Dokploy and returns its id.
fn network(world: &ProjectWorld, name: &str) -> String {
    world.sim.seed("network", &json!({ "name": name }))
}

fn held(world: &ProjectWorld) -> Vec<String> {
    let application = world.sim.objects("application").remove(0);
    application["networkIds"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .map(|id| id.as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_networks_a_document_names_are_attached_by_id_and_the_next_plan_is_empty() {
    let world = ProjectWorld::new();
    let backend = network(&world, "backend");
    let data = network(&world, "data");
    let document = app(&networks(&["backend", "data"]));

    world.apply_text(&document, &[]).await.expect("applies");

    let mut ids = held(&world);
    ids.sort();
    let mut expected = vec![backend, data];
    expected.sort();
    assert_eq!(ids, expected);
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.applyable(), "{:?}", plan.diagnostics());
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn changing_the_set_attaches_the_new_member_and_detaches_the_removed_one() {
    let world = ProjectWorld::new();
    let backend = network(&world, "backend");
    let data = network(&world, "data");
    let edge = network(&world, "edge");
    world
        .apply_text(&app(&networks(&["backend", "data"])), &[])
        .await
        .expect("applies");

    let document = app(&networks(&["backend", "edge"]));
    world.apply_text(&document, &[]).await.expect("applies");

    let ids = held(&world);
    assert!(ids.contains(&backend) && ids.contains(&edge), "{ids:?}");
    assert!(
        !ids.contains(&data),
        "the removed member is detached: {ids:?}"
    );
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn an_attachment_the_document_does_not_name_is_kept() {
    let world = ProjectWorld::new();
    let backend = network(&world, "backend");
    let legacy = network(&world, "legacy");
    let document = app(&networks(&["backend"]));
    world.apply_text(&document, &[]).await.expect("applies");
    // Someone attaches the service to another network in Dokploy.
    let id = world.sim.objects("application").remove(0)["applicationId"]
        .as_str()
        .unwrap()
        .to_owned();
    world.sim.patch(
        "application",
        &id,
        &json!({ "networkIds": [backend, legacy] }),
    );

    let plan = world.plan_text(&document, &[]).await;
    assert!(
        plan.changes().is_empty(),
        "an unmanaged attachment is not drift: {plan:?}"
    );

    // Adding a member does not detach the unmanaged one.
    let edge = network(&world, "edge");
    world
        .apply_text(&app(&networks(&["backend", "edge"])), &[])
        .await
        .expect("applies");
    let ids = held(&world);
    assert!(
        ids.contains(&legacy),
        "what the document does not own stays: {ids:?}"
    );
    assert!(ids.contains(&edge), "{ids:?}");
}

#[tokio::test]
async fn an_empty_set_detaches_every_network() {
    let world = ProjectWorld::new();
    network(&world, "backend");
    world
        .apply_text(&app(&networks(&["backend"])), &[])
        .await
        .expect("applies");
    assert_eq!(held(&world).len(), 1);

    let document = app("          networks: []\n");
    world.apply_text(&document, &[]).await.expect("applies");

    assert!(held(&world).is_empty());
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn a_network_that_does_not_exist_blocks_the_plan_and_nothing_is_sent() {
    let world = ProjectWorld::new();
    let document = app(&networks(&["nowhere"]));

    let plan = world.plan_text(&document, &[]).await;

    assert!(!plan.applyable());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|d| d.code() == PlanDiagnosticCode::UnresolvedExternalSelector),
        "{:?}",
        plan.diagnostics()
    );
}

#[tokio::test]
async fn a_member_that_drifted_away_is_planned_back_in() {
    let world = ProjectWorld::new();
    network(&world, "backend");
    let document = app(&networks(&["backend"]));
    world.apply_text(&document, &[]).await.expect("applies");
    let id = world.sim.objects("application").remove(0)["applicationId"]
        .as_str()
        .unwrap()
        .to_owned();
    world.sim.patch(
        "application",
        &id,
        &json!({ "networkIds": Value::Array(vec![]) }),
    );

    let plan = world.plan_text(&document, &[]).await;

    assert!(plan.applyable(), "{:?}", plan.diagnostics());
    assert_eq!(plan.changes().len(), 1, "{plan:?}");
}

/// A Compose stack with the networks of its services.
fn stack(services: &str) -> String {
    format!(
        "version: 2\nproject:\n  slug: shop\n  environments:\n    production:\n      compose:\n        stack:\n          service_networks:\n{services}"
    )
}

fn elements(world: &ProjectWorld) -> Vec<Value> {
    let compose = world.sim.objects("compose").remove(0);
    compose["serviceNetworks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn element<'a>(elements: &'a [Value], service: &str) -> &'a Value {
    elements
        .iter()
        .find(|element| element["serviceName"] == service)
        .unwrap_or_else(|| panic!("no element for `{service}` in {elements:?}"))
}

fn ids_of(element: &Value) -> Vec<String> {
    let mut ids: Vec<String> = element["networkIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn the_networks_of_each_compose_service_are_written_as_one_element_per_service() {
    let world = ProjectWorld::new();
    let backend = network(&world, "backend");
    let data = network(&world, "data");
    let document = stack(
        "            api: [{ name: backend }]\n            worker: [{ name: backend }, { name: data }]\n",
    );

    world.apply_text(&document, &[]).await.expect("applies");

    let held = elements(&world);
    assert_eq!(held.len(), 2, "{held:?}");
    assert_eq!(ids_of(element(&held, "api")), [backend.clone()]);
    let mut both = vec![backend, data];
    both.sort();
    assert_eq!(ids_of(element(&held, "worker")), both);
    assert_eq!(
        element(&held, "api")["detachDokployNetwork"],
        false,
        "a service the document adds starts attached to Dokploy's own network"
    );
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.applyable(), "{:?}", plan.diagnostics());
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn a_service_the_document_stops_attaching_to_a_network_is_detached_from_it() {
    let world = ProjectWorld::new();
    let backend = network(&world, "backend");
    let data = network(&world, "data");
    world
        .apply_text(
            &stack("            worker: [{ name: backend }, { name: data }]\n"),
            &[],
        )
        .await
        .expect("applies");

    let document = stack("            worker: [{ name: backend }]\n");
    world.apply_text(&document, &[]).await.expect("applies");

    let held = elements(&world);
    assert_eq!(ids_of(element(&held, "worker")), [backend]);
    assert!(!ids_of(element(&held, "worker")).contains(&data));
    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn another_service_and_the_flag_dokploy_holds_are_kept() {
    let world = ProjectWorld::new();
    let backend = network(&world, "backend");
    let other = network(&world, "other");
    let document = stack("            api: [{ name: backend }]\n");
    world.apply_text(&document, &[]).await.expect("applies");
    // Another service attached elsewhere, and the flag of `api` set in Dokploy.
    let id = world.sim.objects("compose").remove(0)["composeId"]
        .as_str()
        .unwrap()
        .to_owned();
    world.sim.patch(
        "compose",
        &id,
        &json!({ "serviceNetworks": [
            { "serviceName": "api", "networkIds": [backend], "detachDokployNetwork": true },
            { "serviceName": "db", "networkIds": [other], "detachDokployNetwork": false }
        ] }),
    );

    let plan = world.plan_text(&document, &[]).await;
    assert!(plan.changes().is_empty(), "not drift: {plan:?}");

    let data = network(&world, "data");
    world
        .apply_text(
            &stack("            api: [{ name: backend }, { name: data }]\n"),
            &[],
        )
        .await
        .expect("applies");
    let held = elements(&world);
    assert_eq!(held.len(), 2, "{held:?}");
    assert_eq!(element(&held, "db")["networkIds"], json!([other]));
    assert_eq!(
        element(&held, "api")["detachDokployNetwork"],
        true,
        "the flag is Dokploy's, not the document's"
    );
    assert!(ids_of(element(&held, "api")).contains(&data));
}

#[tokio::test]
async fn a_service_network_that_does_not_exist_blocks_the_plan() {
    let world = ProjectWorld::new();

    let plan = world
        .plan_text(&stack("            api: [{ name: nowhere }]\n"), &[])
        .await;

    assert!(!plan.applyable());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|d| d.code() == PlanDiagnosticCode::UnresolvedExternalSelector),
        "{:?}",
        plan.diagnostics()
    );
}
