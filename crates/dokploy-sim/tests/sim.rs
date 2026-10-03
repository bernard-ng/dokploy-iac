//! The simulator behaves like the Dokploy the fixtures were recorded from.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dokploy_sdk::{Error, OperationRequest, Transport};
use dokploy_sim::{Fault, FaultKind, Sim};
use dokploy_spec::load_dir;
use serde_json::{Value, json};

const VERSION: &str = "v0.30.6";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixtures() -> PathBuf {
    root().join("fixtures/api/live").join(VERSION)
}

fn sim() -> Sim {
    let specs = load_dir(&root().join("specs")).expect("repository specs are valid");
    Sim::new(Arc::new(specs)).with_fixtures(&fixtures())
}

fn fixture(name: &str) -> Value {
    let text = std::fs::read_to_string(fixtures().join(name)).expect("fixture exists");
    serde_json::from_str(&text).expect("fixture is JSON")
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn registry_body(name: &str) -> Value {
    json!({
        "registryName": name, "username": "owner", "password": "canary-password",
        "registryUrl": "ghcr.io", "registryType": "cloud", "imagePrefix": null
    })
}

async fn call(
    sim: &Sim,
    operation: &str,
    query: &[(&str, &str)],
    body: Option<Value>,
) -> Result<Value, Error> {
    let mut request = OperationRequest::new(operation);
    for (name, value) in query {
        request = request.query(*name, *value);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    sim.call(request).await
}

fn status(error: &Error) -> u16 {
    error.dokploy().expect("a Dokploy error").status()
}

#[tokio::test]
async fn a_registry_lives_its_whole_life_with_the_recorded_shapes() {
    let sim = sim();

    let created = call(&sim, "registry.create", &[], Some(registry_body("ghcr")))
        .await
        .unwrap();
    assert_eq!(keys(&created), keys(&fixture("registry-create.owner.json")));
    assert_eq!(
        created["password"], "canary-password",
        "create returns the password, as live"
    );
    let id = created["registryId"].as_str().unwrap().to_owned();

    let listed = call(&sim, "registry.all", &[], None).await.unwrap();
    assert_eq!(
        keys(&listed[0]),
        keys(&fixture("registry-all.created.owner.json")[0])
    );
    assert_eq!(
        listed[0]["password"], "canary-password",
        "the collection lists the password"
    );

    let one = call(&sim, "registry.one", &[("registryId", &id)], None)
        .await
        .unwrap();
    assert_eq!(
        keys(&one),
        keys(&fixture("registry-one.created.owner.json"))
    );
    assert!(
        one.get("password").is_none(),
        "the direct read omits the password"
    );

    let patched = call(
        &sim,
        "registry.update",
        &[],
        Some(json!({"registryId": id, "imagePrefix": "team"})),
    )
    .await
    .unwrap();
    assert_eq!(
        patched,
        fixture("registry-update.owner.json"),
        "update answers true"
    );
    let after = call(&sim, "registry.one", &[("registryId", &id)], None)
        .await
        .unwrap();
    assert_eq!(after["imagePrefix"], "team");
    assert_eq!(
        after["username"], "owner",
        "an omitted field keeps its value"
    );

    let removed = call(
        &sim,
        "registry.remove",
        &[],
        Some(json!({"registryId": id})),
    )
    .await
    .unwrap();
    assert_eq!(keys(&removed), keys(&fixture("registry-remove.owner.json")));
    let gone = call(&sim, "registry.one", &[("registryId", &id)], None)
        .await
        .unwrap_err();
    assert_eq!(status(&gone), 404);
    assert_eq!(gone.dokploy().unwrap().message(), "Registry not found");
    assert_eq!(
        call(&sim, "registry.all", &[], None).await.unwrap(),
        json!([])
    );
}

#[tokio::test]
async fn a_tag_answers_with_objects_and_a_success_marker() {
    let sim = sim();
    let created = call(
        &sim,
        "tag.create",
        &[],
        Some(json!({"name": "blue", "color": "#0000ff"})),
    )
    .await
    .unwrap();
    assert_eq!(keys(&created), keys(&fixture("tag-create.owner.json")));
    let id = created["tagId"].as_str().unwrap().to_owned();

    let cleared = call(
        &sim,
        "tag.update",
        &[],
        Some(json!({"tagId": id, "color": null})),
    )
    .await
    .unwrap();
    assert_eq!(keys(&cleared), keys(&fixture("tag-update.owner.json")));
    assert_eq!(cleared["color"], Value::Null);
    assert_eq!(cleared["name"], "blue");

    let removed = call(&sim, "tag.remove", &[], Some(json!({"tagId": id})))
        .await
        .unwrap();
    assert_eq!(removed, fixture("tag-remove.owner.json"));
}

#[tokio::test]
async fn a_request_the_contract_does_not_allow_is_rejected_as_dokploy_would() {
    let sim = sim();
    let mut extra = registry_body("ghcr");
    extra["unknown"] = json!(1);
    let error = call(&sim, "registry.create", &[], Some(extra))
        .await
        .unwrap_err();
    assert_eq!(status(&error), 400);
    assert!(
        error
            .dokploy()
            .unwrap()
            .issues()
            .iter()
            .any(|issue| issue.contains("unknown"))
    );

    let mut missing = registry_body("ghcr");
    missing.as_object_mut().unwrap().remove("username");
    let error = call(&sim, "registry.create", &[], Some(missing))
        .await
        .unwrap_err();
    assert_eq!(status(&error), 400);

    let error = call(&sim, "registry.one", &[], None).await.unwrap_err();
    assert_eq!(status(&error), 400, "a required query parameter");
    assert!(sim.objects("registry").is_empty(), "nothing was stored");
}

#[tokio::test]
async fn a_dropped_response_still_applied_the_change_and_a_dropped_request_did_not() {
    let sim = sim();
    sim.inject(Fault::new("registry.create", FaultKind::DropBefore));
    let error = call(&sim, "registry.create", &[], Some(registry_body("a")))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::OutcomeUnknown { .. }));
    assert!(sim.objects("registry").is_empty());

    sim.inject(Fault::new("registry.create", FaultKind::DropAfter));
    let error = call(&sim, "registry.create", &[], Some(registry_body("b")))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::OutcomeUnknown { .. }));
    assert_eq!(sim.objects("registry").len(), 1, "the change was applied");

    sim.inject(Fault::new("registry.all", FaultKind::DropBefore));
    let error = call(&sim, "registry.all", &[], None).await.unwrap_err();
    assert!(
        matches!(error, Error::Request { .. }),
        "a read is never an unknown outcome"
    );
}

#[tokio::test]
async fn a_rejection_after_saving_persists_the_change_like_a_failed_registry_login() {
    let sim = sim();
    let id = sim.seed(
        "registry",
        &json!({"registryName": "r", "registryUrl": "ghcr.io"}),
    );
    sim.inject(Fault::new(
        "registry.update",
        FaultKind::RejectAfter { status: 400 },
    ));
    let error = call(
        &sim,
        "registry.update",
        &[],
        Some(json!({"registryId": id, "registryUrl": "elsewhere"})),
    )
    .await
    .unwrap_err();
    assert_eq!(status(&error), 400);
    assert_eq!(
        sim.object("registry", &id).unwrap()["registryUrl"],
        "elsewhere"
    );

    sim.inject(Fault::new(
        "registry.update",
        FaultKind::Reject { status: 500 },
    ));
    let error = call(
        &sim,
        "registry.update",
        &[],
        Some(json!({"registryId": id, "registryUrl": "never"})),
    )
    .await
    .unwrap_err();
    assert_eq!(status(&error), 500);
    assert_eq!(
        sim.object("registry", &id).unwrap()["registryUrl"],
        "elsewhere"
    );
}

#[tokio::test]
async fn a_fault_can_target_the_nth_call() {
    let sim = sim();
    sim.inject(Fault::new("tag.all", FaultKind::Unavailable).on_call(2));
    assert!(call(&sim, "tag.all", &[], None).await.is_ok());
    assert_eq!(
        status(&call(&sim, "tag.all", &[], None).await.unwrap_err()),
        503
    );
    assert!(
        call(&sim, "tag.all", &[], None).await.is_ok(),
        "the fault fires once"
    );
}

#[tokio::test]
async fn a_hidden_object_is_missing_from_the_collection_but_not_from_a_direct_read() {
    let sim = sim();
    let id = sim.seed("tag", &json!({"name": "hidden"}));
    sim.hide_from_listing("tag", &id);
    assert_eq!(call(&sim, "tag.all", &[], None).await.unwrap(), json!([]));
    assert!(call(&sim, "tag.one", &[("tagId", &id)], None).await.is_ok());
}

#[tokio::test]
async fn a_child_needs_its_parent_and_is_embedded_in_it_and_goes_with_it() {
    let sim = sim();
    let missing = call(
        &sim,
        "redirects.create",
        &[],
        Some(json!({"applicationId": "nope", "regex": "^/a", "replacement": "/b", "permanent": true})),
    )
    .await
    .unwrap_err();
    assert_eq!(status(&missing), 404);

    let application = sim.seed("application", &json!({"name": "api"}));
    let created = call(
        &sim,
        "redirects.create",
        &[],
        Some(json!({"applicationId": application, "regex": "^/a", "replacement": "/b", "permanent": true})),
    )
    .await
    .unwrap();
    assert_eq!(
        created,
        fixture("redirect-create.owner.json"),
        "create answers true"
    );

    let parent = call(
        &sim,
        "application.one",
        &[("applicationId", &application)],
        None,
    )
    .await
    .unwrap();
    let redirects = parent["redirects"].as_array().expect("embedded collection");
    assert_eq!(redirects.len(), 1);
    assert_eq!(redirects[0]["regex"], "^/a");
    let id = redirects[0]["redirectId"].as_str().unwrap().to_owned();

    let one = call(&sim, "redirects.one", &[("redirectId", &id)], None)
        .await
        .unwrap();
    assert_eq!(one["applicationId"], application);

    sim.delete("application", &application);
    assert!(
        sim.objects("redirect").is_empty(),
        "children go with their parent"
    );
}

#[tokio::test]
async fn requests_are_logged_and_never_print_their_values() {
    let sim = sim();
    call(&sim, "registry.create", &[], Some(registry_body("ghcr")))
        .await
        .unwrap();
    call(&sim, "registry.all", &[], None).await.unwrap();

    let mutations = sim.mutations();
    assert_eq!(mutations.len(), 1);
    assert_eq!(mutations[0].operation(), "registry.create");
    assert_eq!(sim.requests().len(), 2);
    assert!(!format!("{:?}", sim.requests()).contains("canary-password"));
    assert!(!format!("{sim:?}").contains("canary-password"));
}
