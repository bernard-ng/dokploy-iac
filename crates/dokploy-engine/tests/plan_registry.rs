//! Milestone M1: a settings document containing a registry is planned against a canned
//! remote, with no first-engine code and no network.

mod common;

use common::{Canned, checkpointed, compile, empty_settings_state, engine};
use dokploy_core::{ChangeKind, PlanDiagnosticCode};
use dokploy_engine::EngineError;
use dokploy_sdk::Error;
use serde_json::{Value, json};

const DOCUMENT: &str = "\
version: 2
settings:
  registries:
    ghcr:
      type: cloud
      url: ghcr.io
      username: ci
      password:
        env: GHCR_TOKEN
      image_prefix: acme
";

const TOKEN: &str = "ghcr-token-canary-1";
const REMOTE_PASSWORD: &str = "remote-password-canary-2";

/// What Dokploy returns for the registry. It embeds the password, as the real API does.
fn registry(url: &str, username: &str) -> Value {
    json!({
        "registryId": "reg-1",
        "registryName": "ghcr",
        "registryType": "cloud",
        "registryUrl": url,
        "username": username,
        "password": REMOTE_PASSWORD,
        "imagePrefix": "acme",
        "serverId": null,
        "createdAt": "2026-10-03T00:00:00.000Z",
        "organizationId": "org-1"
    })
}

fn remote_with(item: Value) -> Canned {
    let one = item.clone();
    Canned::new()
        .serving("registry.all", json!([item]))
        .on("registry.one", move |_| Ok(one.clone()))
}

fn names(plan: &dokploy_core::Plan) -> Vec<String> {
    plan.changes()[0]
        .fields()
        .iter()
        .map(|field| field.key().to_string())
        .collect()
}

#[tokio::test]
async fn a_registry_document_plans_a_create_against_an_empty_remote() {
    let transport = Canned::new().serving("registry.all", json!([]));
    let engine = engine(transport.clone());
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, None).await.expect("plans");

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes().len(), 1);
    let change = &plan.changes()[0];
    assert_eq!(change.kind(), ChangeKind::Create);
    assert_eq!(change.address().to_string(), "registry.ghcr");
    assert_eq!(
        names(&plan),
        [
            "image_prefix",
            "name",
            "password",
            "type",
            "url",
            "username"
        ],
        "the name defaults to the key"
    );
    assert_eq!(
        transport.calls(),
        ["registry.all"],
        "one list, nothing else"
    );

    // The plan shows paths, never values, and the secret was never kept.
    let shown = format!("{plan:?}{}", String::from_utf8_lossy(&plan.to_json_bytes()));
    for value in ["ghcr.io", TOKEN, "acme"] {
        assert!(!shown.contains(value), "{value} leaked: {shown}");
    }
}

#[tokio::test]
async fn after_an_apply_the_next_plan_is_empty() {
    // First plan and apply.
    let empty = Canned::new().serving("registry.all", json!([]));
    let engine_before = engine(empty);
    let compiled = compile(&engine_before, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);
    let created = engine_before.plan(&compiled, None).await.unwrap();
    let state = checkpointed(
        &created,
        &[("registry.ghcr", "reg-1")],
        empty_settings_state(&engine_before),
    );

    // The registry now exists remotely; plan again against it.
    let transport = remote_with(registry("ghcr.io", "ci"));
    let engine = engine_with(transport.clone());
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);
    let plan = engine.plan(&compiled, Some(&state)).await.unwrap();

    assert!(plan.applyable(), "{plan:?}");
    assert!(plan.changes().is_empty(), "{plan:?}");
    assert!(plan.drift().is_empty(), "{plan:?}");
    assert_eq!(
        transport.calls(),
        ["registry.all", "registry.one"],
        "a direct read only for the resource that matched"
    );
}

fn engine_with(transport: Canned) -> dokploy_engine::Engine<Canned> {
    engine(transport)
}

async fn applied_state() -> dokploy_state::StateFile {
    let empty = Canned::new().serving("registry.all", json!([]));
    let engine = engine(empty);
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);
    let created = engine.plan(&compiled, None).await.unwrap();

    checkpointed(
        &created,
        &[("registry.ghcr", "reg-1")],
        empty_settings_state(&engine),
    )
}

#[tokio::test]
async fn a_changed_property_plans_an_in_place_update_naming_only_that_path() {
    let state = applied_state().await;
    let engine = engine(remote_with(registry("ghcr.io", "ci")));
    let changed = DOCUMENT.replace("ghcr.io", "registry.example.com");
    let compiled = compile(&engine, &changed, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, Some(&state)).await.unwrap();

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(names(&plan), ["url"]);
}

#[tokio::test]
async fn rotating_the_secret_changes_only_the_password() {
    let state = applied_state().await;
    let engine = engine(remote_with(registry("ghcr.io", "ci")));
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", "a-rotated-token")]);

    let plan = engine.plan(&compiled, Some(&state)).await.unwrap();

    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(names(&plan), ["password"]);
    let shown = format!("{plan:?}{}", String::from_utf8_lossy(&plan.to_json_bytes()));
    assert!(!shown.contains("a-rotated-token") && !shown.contains(TOKEN));
}

#[tokio::test]
async fn a_change_made_outside_the_tool_is_reported_as_drift() {
    let state = applied_state().await;
    let engine = engine(remote_with(registry("ghcr.io", "someone-else")));
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, Some(&state)).await.unwrap();

    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update, "{plan:?}");
    assert_eq!(names(&plan), ["username"]);
}

#[tokio::test]
async fn an_existing_registry_the_tool_does_not_manage_is_a_collision_not_an_overwrite() {
    let engine = engine(remote_with(registry("ghcr.io", "ci")));
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(!plan.applyable(), "{plan:?}");
    assert!(
        plan.diagnostics()
            .iter()
            .any(|d| { d.code() == PlanDiagnosticCode::UnmanagedAddressCollision })
    );
    assert!(plan.changes().is_empty());
}

#[tokio::test]
async fn a_registry_deleted_remotely_is_planned_for_recreation() {
    let state = applied_state().await;
    let engine = engine(Canned::new().serving("registry.all", json!([])));
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, Some(&state)).await.unwrap();

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
}

// ---------------------------------------------------------------------------
// Authority: what a failed or inconsistent read blocks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failed_list_blocks_the_registries_it_covers() {
    let transport = Canned::new().on("registry.all", |_| {
        Err(Error::UnexpectedResponse {
            operation: "registry.all",
        })
    });
    let engine = engine(transport.clone());
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|d| d.code() == PlanDiagnosticCode::RemoteUnavailable)
    );
    assert_eq!(transport.calls(), ["registry.all"]);
}

#[tokio::test]
async fn a_list_that_is_not_a_list_is_an_invalid_response() {
    let engine = engine(Canned::new().serving("registry.all", json!({"items": []})));
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(!plan.applyable(), "{plan:?}");
}

#[tokio::test]
async fn a_direct_read_that_disagrees_with_the_collection_is_not_trusted() {
    let state = applied_state().await;
    let listed = registry("ghcr.io", "ci");
    let direct = registry("elsewhere.example.com", "ci");
    let transport = Canned::new()
        .serving("registry.all", json!([listed]))
        .serving("registry.one", direct);
    let engine = engine(transport);
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, Some(&state)).await.unwrap();

    assert!(
        !plan.applyable(),
        "disagreement is unavailable, not a guess: {plan:?}"
    );
    assert!(plan.changes().is_empty());
}

#[tokio::test]
async fn a_remote_value_outside_the_spec_blocks_that_registry_only() {
    let state = applied_state().await;
    let mut odd = registry("ghcr.io", "ci");
    odd["registryType"] = json!("self-hosted");
    let engine = engine(remote_with(odd));
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, Some(&state)).await.unwrap();

    assert!(!plan.applyable(), "{plan:?}");
}

#[tokio::test]
async fn nothing_is_read_for_a_kind_nobody_mentions() {
    let transport = Canned::new();
    let engine = engine(transport.clone());
    let compiled = compile(&engine, "version: 2\nsettings: {}\n", &[]);

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(plan.applyable() && plan.changes().is_empty());
    assert!(transport.calls().is_empty(), "{:?}", transport.calls());
}

const NESTED: &str = "\
version: 2
project:
  slug: shop
  environments:
    staging: {}
";

#[tokio::test]
async fn a_child_of_a_missing_parent_is_missing_without_reading_for_it() {
    let transport = Canned::new().serving("project.all", json!([]));
    let engine = engine(transport.clone());
    let compiled = compile(&engine, NESTED, &[]);

    let plan = engine.plan(&compiled, None).await.expect("plans");

    assert!(plan.applyable(), "{plan:?}");
    let created: Vec<String> = plan
        .changes()
        .iter()
        .map(|c| c.address().to_string())
        .collect();
    assert_eq!(
        created,
        ["project.shop", "project.shop/environment.staging"]
    );
    assert_eq!(
        transport.calls(),
        ["project.all"],
        "no read for a child of nothing"
    );
}

#[tokio::test]
async fn a_child_collection_is_read_once_per_parent_scoped_by_it() {
    let project = json!({"projectId": "p-1", "name": "shop", "description": null});
    let transport = Canned::new()
        .serving("project.all", json!([project.clone()]))
        .serving("project.one", project)
        .on("environment.byProjectId", |request| {
            assert_eq!(
                request.query_parameters().get("projectId"),
                Some(&json!("p-1"))
            );
            Ok(json!([]))
        });
    let engine = engine(transport.clone());
    let compiled = compile(&engine, NESTED, &[]);
    // Record the project in state (as if it were applied) so the environment is planned
    // beneath it.
    let mut state = empty_project_state(&engine);
    let first = compile(&engine, "version: 2\nproject:\n  slug: shop\n", &[]);
    let seed = Canned::new().serving("project.all", json!([]));
    let seeded = common::engine(seed).plan(&first, None).await.unwrap();
    for change in seeded.changes() {
        let resource = change
            .checkpoint()
            .present()
            .unwrap()
            .materialize(
                change.address(),
                dokploy_state::RemoteId::new("p-1").unwrap(),
            )
            .unwrap();
        state
            .upsert_resource(change.address().clone(), resource)
            .unwrap();
    }

    let plan = engine.plan(&compiled, Some(&state)).await.expect("plans");

    assert!(plan.applyable(), "{plan:?}");
    let created: Vec<String> = plan
        .changes()
        .iter()
        .map(|c| c.address().to_string())
        .collect();
    assert_eq!(created, ["project.shop/environment.staging"]);
    assert_eq!(
        transport.calls(),
        ["project.all", "project.one", "environment.byProjectId"],
        "the child collection is read once, after its parent"
    );
}

fn empty_project_state(engine: &dokploy_engine::Engine<Canned>) -> dokploy_state::StateFile {
    dokploy_state::StateFile::new_for_document(
        semver::Version::new(0, 1, 0),
        engine.instance().clone(),
        dokploy_state::DocumentId::Project(dokploy_state::ResourceName::new("shop").unwrap()),
    )
}

#[tokio::test]
async fn a_selector_without_a_fresh_resolution_blocks_planning() {
    let transport = Canned::new().serving("registry.all", json!([]));
    let engine = engine(transport);
    let document = format!("{DOCUMENT}      server:\n        name: edge-1\n");
    let compiled = compile(&engine, &document, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(!plan.applyable(), "{plan:?}");
    assert!(
        plan.diagnostics()
            .iter()
            .any(|d| { d.code() == PlanDiagnosticCode::UnresolvedExternalSelector })
    );
}

#[tokio::test]
async fn planning_is_deterministic() {
    let state = applied_state().await;
    let transport = remote_with(registry("ghcr.io", "someone-else"));
    let engine = engine(transport);
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let first = engine
        .plan(&compiled, Some(&state))
        .await
        .unwrap()
        .to_json_bytes();
    let second = engine
        .plan(&compiled, Some(&state))
        .await
        .unwrap()
        .to_json_bytes();

    assert_eq!(first, second);
}

#[tokio::test]
async fn state_for_another_document_or_instance_is_refused() {
    let transport = Canned::new().serving("registry.all", json!([]));
    let engine = engine(transport);
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", TOKEN)]);

    let project_state = dokploy_state::StateFile::new_for_document(
        semver::Version::new(0, 1, 0),
        engine.instance().clone(),
        dokploy_state::DocumentId::Project("shop".parse().unwrap()),
    );
    assert!(matches!(
        engine.plan(&compiled, Some(&project_state)).await,
        Err(EngineError::WrongDocument { .. })
    ));

    let other_instance = dokploy_state::StateFile::new_for_document(
        semver::Version::new(0, 1, 0),
        dokploy_state::InstanceIdentity::parse("https://elsewhere.example.com").unwrap(),
        dokploy_state::DocumentId::Settings,
    );
    assert!(matches!(
        engine.plan(&compiled, Some(&other_instance)).await,
        Err(EngineError::State(_))
    ));
}

#[tokio::test]
async fn dependencies_order_the_plan_and_resolve_by_suffix() {
    let transport = Canned::new().serving("registry.all", json!([]));
    let engine = engine(transport);
    let document = "\
version: 2
settings:
  registries:
    mirror:
      depends_on: [registry.primary]
      type: cloud
      url: mirror.example.com
      username: ci
      password: { env: GHCR_TOKEN }
    primary:
      type: cloud
      url: ghcr.io
      username: ci
      password: { env: GHCR_TOKEN }
";
    let compiled = compile(&engine, document, &[("GHCR_TOKEN", TOKEN)]);

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(plan.applyable(), "{plan:?}");
    let order: Vec<String> = plan
        .changes()
        .iter()
        .map(|c| c.address().to_string())
        .collect();
    assert_eq!(order, ["registry.primary", "registry.mirror"]);
}
