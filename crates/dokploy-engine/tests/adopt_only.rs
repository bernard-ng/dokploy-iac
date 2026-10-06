//! Adopt-only resources are enrolled, updated, and forgotten without a create or remove call.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{compile, engine, specs};
use dokploy_core::{ChangeKind, PlanDiagnosticCode};
use dokploy_sim::Sim;
use dokploy_state::{DocumentId, StateStore};
use serde_json::json;

const DOCUMENT: &str = r#"
version: 2
settings:
  github_providers:
    github-contract:
      app_name: wanted-app
      shared_with_organization: true
"#;

fn sim() -> Sim {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6");
    Sim::new(Arc::new(specs())).with_fixtures(&fixtures)
}

fn store(directory: &tempfile::TempDir, engine: &dokploy_engine::Engine<&Sim>) -> StateStore {
    StateStore::for_document(
        directory.path().canonicalize().unwrap(),
        engine.instance().clone(),
        DocumentId::Settings,
    )
    .unwrap()
}

#[tokio::test]
async fn a_github_provider_is_adopted_updated_and_forgotten_without_lifecycle_mutations() {
    let sim = sim();
    let id = sim.seed(
        "github_provider",
        &json!({
            "gitProviderId": "github-provider-1",
            "githubId": "github-row-1",
            "githubAppName": "installed-app",
            "gitProvider": {
                "name": "github-contract",
                "sharedWithOrganization": false
            }
        }),
    );
    let engine = engine(&sim);
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory, &engine);
    let compiled = compile(&engine, DOCUMENT, &[]);

    let plan = engine.plan(&compiled, None).await.unwrap();
    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Adopt);
    assert_eq!(plan.changes()[0].adopted_remote_id().unwrap().as_str(), id);

    let summary = engine.apply(&compiled, &store, |_| true).await.unwrap();
    assert_eq!(summary.applied(), 1);
    assert_eq!(
        sim.mutations()
            .iter()
            .map(|request| request.operation())
            .collect::<Vec<_>>(),
        ["github.update", "gitProvider.toggleShare"]
    );
    let held = sim.object("github_provider", &id).unwrap();
    assert_eq!(held["githubAppName"], "wanted-app");
    assert_eq!(held["gitProvider"]["sharedWithOrganization"], true);

    let state = store
        .inspect()
        .unwrap()
        .expect("adoption checkpoints state");
    assert_eq!(state.resources().len(), 1);
    assert_eq!(
        state
            .resources()
            .values()
            .next()
            .unwrap()
            .remote_id()
            .as_str(),
        id
    );
    assert!(
        engine
            .plan(&compiled, Some(&state))
            .await
            .unwrap()
            .changes()
            .is_empty()
    );

    sim.clear_requests();
    let empty = compile(&engine, "version: 2\nsettings: {}\n", &[]);
    assert_eq!(
        engine
            .apply(&empty, &store, |_| true)
            .await
            .unwrap()
            .applied(),
        1
    );
    assert!(
        sim.mutations().is_empty(),
        "forget does not call a remove operation"
    );
    assert!(sim.object("github_provider", &id).is_some());
    assert!(store.inspect().unwrap().unwrap().resources().is_empty());
}

#[tokio::test]
async fn a_missing_github_provider_cannot_be_created() {
    let sim = sim();
    let engine = engine(&sim);
    let compiled = compile(&engine, DOCUMENT, &[]);

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(!plan.applyable());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::AdoptTargetMissing
    );
    assert!(sim.mutations().is_empty());
}

#[tokio::test]
async fn an_organization_singleton_is_adopted_updated_and_forgotten() {
    let sim = sim();
    let organization = sim.objects("organization").into_iter().next().unwrap();
    let id = organization["id"].as_str().unwrap().to_owned();
    sim.patch(
        "organization",
        &id,
        &json!({ "name": "Before", "logo": null, "defaultRole": null }),
    );
    let engine = engine(&sim);
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory, &engine);
    let compiled = compile(
        &engine,
        r#"
version: 2
settings:
  organization:
    name: After
    logo: https://example.invalid/logo.svg
    default_role: member
"#,
        &[],
    );

    let plan = engine.plan(&compiled, None).await.unwrap();
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Adopt);
    assert_eq!(
        plan.changes()[0].address().to_string(),
        "organization.active"
    );
    assert_eq!(plan.changes()[0].adopted_remote_id().unwrap().as_str(), id);

    let result = engine.apply(&compiled, &store, |_| true).await;
    assert!(
        result.is_ok(),
        "{result:?}; bodies: {:?}",
        sim.mutations()
            .iter()
            .map(|request| request.body())
            .collect::<Vec<_>>()
    );
    assert_eq!(result.unwrap().applied(), 1);
    let held = sim.object("organization", &id).unwrap();
    assert_eq!(held["name"], "After");
    assert_eq!(held["logo"], "https://example.invalid/logo.svg");
    assert_eq!(held["defaultRole"], "member");
    assert!(
        engine
            .plan(&compiled, store.inspect().unwrap().as_ref())
            .await
            .unwrap()
            .changes()
            .is_empty()
    );

    sim.clear_requests();
    let empty = compile(&engine, "version: 2\nsettings: {}\n", &[]);
    engine.apply(&empty, &store, |_| true).await.unwrap();
    assert!(sim.mutations().is_empty());
    assert!(sim.object("organization", &id).is_some());
}
