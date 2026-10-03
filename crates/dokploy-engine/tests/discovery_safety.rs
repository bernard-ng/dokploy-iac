//! What discovery refuses to conclude from: contradictory, oversized, or inconsistent reads.

mod common;

use common::{Canned, compile, engine};
use dokploy_core::{ChangeKind, PlanDiagnosticCode, RemoteFailureKind};
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
";

fn registry(id: &str) -> Value {
    json!({
        "registryId": id, "registryName": "ghcr", "registryType": "cloud",
        "registryUrl": "ghcr.io", "username": "ci", "imagePrefix": null,
        "createdAt": "2026-10-03T00:00:00.000Z", "organizationId": "org-1"
    })
}

async fn plan(transport: Canned) -> dokploy_core::Plan {
    let engine = engine(transport);
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", "canary-token")]);
    engine.plan(&compiled, None).await.expect("plans")
}

fn unavailable_as_invalid(plan: &dokploy_core::Plan) -> bool {
    !plan.applyable()
        && plan.diagnostics().iter().any(|d| {
            d.code() == PlanDiagnosticCode::RemoteUnavailable
                && d.remote_failure() == Some(RemoteFailureKind::InvalidResponse)
        })
}

#[tokio::test]
async fn a_collection_that_lists_one_identity_twice_contradicts_itself() {
    let item = registry("reg-1");
    let plan = plan(Canned::new().serving("registry.all", json!([item.clone(), item]))).await;

    assert!(unavailable_as_invalid(&plan), "{plan:?}");
}

#[tokio::test]
async fn a_collection_past_the_bound_is_refused_not_read() {
    let items: Vec<Value> = (0..10_001)
        .map(|n| json!({"registryId": format!("reg-{n}")}))
        .collect();
    let plan = plan(Canned::new().serving("registry.all", Value::Array(items))).await;

    assert!(unavailable_as_invalid(&plan), "{plan:?}");
}

#[tokio::test]
async fn fields_the_spec_does_not_know_are_ignored() {
    let mut item = registry("reg-1");
    item["somethingNew"] = json!({"nested": [1, 2, 3]});
    let one = item.clone();
    let plan = plan(
        Canned::new()
            .serving("registry.all", json!([item]))
            .on("registry.one", move |_| Ok(one.clone())),
    )
    .await;

    // The registry exists but is not managed here, so it is a collision, not a read failure.
    assert!(
        plan.diagnostics()
            .iter()
            .all(|d| d.code() == PlanDiagnosticCode::UnmanagedAddressCollision),
        "{plan:?}"
    );
}

#[tokio::test]
async fn a_direct_404_while_the_collection_lists_the_identity_is_not_absence() {
    let item = registry("reg-1");
    let plan = plan(Canned::new().serving("registry.all", json!([item])).on(
        "registry.one",
        |_| {
            Err(Error::Api(dokploy_sdk::DokployError::new(
                404,
                "NOT_FOUND".to_owned(),
                "Registry not found".to_owned(),
                Vec::new(),
            )))
        },
    ))
    .await;

    assert!(!plan.applyable(), "{plan:?}");
    assert!(
        plan.changes()
            .iter()
            .all(|c| c.kind() != ChangeKind::Create),
        "a resource the collection lists was planned for creation: {plan:?}"
    );
}

#[tokio::test]
async fn absence_from_an_authoritative_collection_is_proof() {
    let plan = plan(Canned::new().serving("registry.all", json!([]))).await;

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
}
