//! Vault assignments are a keyed array in Dokploy and a map of named selectors in the document.

mod common;

use common::{compile, engine};
use dokploy_core::PlanDiagnosticCode;
use dokploy_state::{DocumentId, RemoteId, StateStore};
use serde_json::json;

fn document(environment: &str) -> String {
    format!(
        "version: 2\nsettings:\n  vault_providers:\n    secrets:\n      config:\n        type: doppler\n        service_token: {{ env: VAULT_TOKEN }}\n        project: shop\n        config: production\n      assignments:\n        shop:\n          - {{ name: {environment} }}\n"
    )
}

#[tokio::test]
async fn project_and_environment_names_are_written_as_scoped_ids() {
    let sim = common::project::sim();
    let project_id = sim.seed("project", &json!({ "name": "shop" }));
    let environment_id = sim.seed(
        "environment",
        &json!({ "name": "production", "projectId": project_id }),
    );
    let engine = engine(&sim);
    let directory = tempfile::tempdir().unwrap();
    let store = StateStore::for_document(
        directory.path().canonicalize().unwrap(),
        engine.instance().clone(),
        DocumentId::Settings,
    )
    .unwrap();
    let text = document("production");
    let compiled = compile(&engine, &text, &[("VAULT_TOKEN", "canary")]);

    engine
        .apply(&compiled, &store, |_| true)
        .await
        .expect("applies");

    let provider = sim.objects("vault_provider").remove(0);
    assert_eq!(provider["assignments"][0]["projectId"], project_id);
    assert_eq!(
        provider["assignments"][0]["environmentIds"],
        json!([environment_id])
    );
    let state = store.inspect().unwrap();
    let plan = engine.plan(&compiled, state.as_ref()).await.unwrap();
    assert!(plan.changes().is_empty(), "converged: {plan:?}");

    let phase = document("production").replace(
        "type: doppler\n        service_token: { env: VAULT_TOKEN }\n        project: shop\n        config: production",
        "type: phase\n        token: { env: VAULT_TOKEN }\n        app_id: app\n        environment_slug: production\n        path: /\n        api_url: https://api.phase.dev",
    );
    let compiled = compile(&engine, &phase, &[("VAULT_TOKEN", "canary")]);
    let state = store.inspect().unwrap();
    let plan = engine.plan(&compiled, state.as_ref()).await.unwrap();
    let checkpoint = plan.changes()[0].checkpoint().present().unwrap();
    checkpoint
        .materialize(plan.changes()[0].address(), RemoteId::new("probe").unwrap())
        .unwrap_or_else(|error| panic!("phase checkpoint: {error:?}"));
}

#[tokio::test]
async fn an_environment_outside_the_named_project_blocks_the_plan() {
    let sim = common::project::sim();
    sim.seed("project", &json!({ "name": "shop" }));
    let other = sim.seed("project", &json!({ "name": "other" }));
    sim.seed(
        "environment",
        &json!({ "name": "production", "projectId": other }),
    );
    let engine = engine(&sim);
    let compiled = compile(
        &engine,
        &document("production"),
        &[("VAULT_TOKEN", "canary")],
    );

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(!plan.applyable());
    assert!(
        plan.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == PlanDiagnosticCode::UnresolvedExternalSelector),
        "{:?}",
        plan.diagnostics()
    );
}
