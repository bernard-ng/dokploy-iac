//! Recovery: an apply that stopped between sending a mutation and recording its result is
//! settled from fresh evidence, never by repeating the mutation (invariant 16).

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{compile, engine, specs};
use dokploy_engine::{ApplyError, Engine, RecoverError, RecoveryAction};
use dokploy_sim::{Fault, FaultKind, Sim};
use dokploy_state::{DocumentId, RecoveryStatus, StateStore};
use serde_json::json;

const PASSWORD: &str = "ghcr-password-canary-1";
const ROTATED: &str = "ghcr-password-canary-2";

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

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6")
}

struct Setup {
    sim: Sim,
    directory: tempfile::TempDir,
}

impl Setup {
    fn new() -> Self {
        Self {
            sim: Sim::new(Arc::new(specs())).with_fixtures(&fixtures()),
            directory: tempfile::tempdir().unwrap(),
        }
    }

    fn engine(&self) -> Engine<&Sim> {
        engine(&self.sim)
    }

    fn store(&self, engine: &Engine<&Sim>) -> StateStore {
        StateStore::for_document(
            self.directory.path().canonicalize().unwrap(),
            engine.instance().clone(),
            DocumentId::Settings,
        )
        .unwrap()
    }
}

async fn apply(
    engine: &Engine<&Sim>,
    store: &StateStore,
    text: &str,
    token: &str,
) -> Result<dokploy_engine::ApplySummary, ApplyError> {
    let compiled = compile(engine, text, &[("GHCR_TOKEN", token)]);
    engine.apply(&compiled, store, |_| true).await
}

async fn recover(
    engine: &Engine<&Sim>,
    store: &StateStore,
    text: &str,
    token: &str,
) -> Result<RecoveryAction, RecoverError> {
    let compiled = compile(engine, text, &[("GHCR_TOKEN", token)]);
    let mut action = None;
    engine
        .recover(&compiled, store, |preview| {
            action = Some(preview.action());
            true
        })
        .await?;

    Ok(action.expect("approval was asked"))
}

fn clean(store: &StateStore) -> bool {
    matches!(store.recovery_status().unwrap(), RecoveryStatus::Clean)
}

#[tokio::test]
async fn a_create_whose_response_was_lost_is_adopted_by_recovery() {
    let setup = Setup::new();
    let engine = setup.engine();
    let store = setup.store(&engine);
    setup
        .sim
        .inject(Fault::new("registry.create", FaultKind::DropAfter));
    let error = apply(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .unwrap_err();
    assert!(
        matches!(error, ApplyError::OutcomeUnknown { .. }),
        "{error}"
    );
    assert!(!clean(&store));

    let action = recover(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .expect("recovers");

    assert_eq!(action, RecoveryAction::AdoptCreatedResource);
    assert!(clean(&store));
    let state = store.inspect().unwrap().unwrap();
    let remote = &setup.sim.objects("registry")[0];
    assert_eq!(
        state
            .resources()
            .values()
            .next()
            .unwrap()
            .remote_id()
            .as_str(),
        remote["registryId"].as_str().unwrap(),
        "the identity found by observation"
    );
    assert_eq!(
        setup.sim.objects("registry").len(),
        1,
        "never created twice"
    );
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", PASSWORD)]);
    let plan = engine.plan(&compiled, Some(&state)).await.unwrap();
    assert!(plan.changes().is_empty(), "converged: {plan:?}");
}

#[tokio::test]
async fn a_create_that_never_reached_dokploy_is_confirmed_as_no_change_and_can_be_applied() {
    let setup = Setup::new();
    let engine = setup.engine();
    let store = setup.store(&engine);
    setup
        .sim
        .inject(Fault::new("registry.create", FaultKind::DropBefore));
    apply(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .unwrap_err();

    let action = recover(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .expect("recovers");

    assert_eq!(action, RecoveryAction::ConfirmNoChange);
    assert!(clean(&store));
    assert!(setup.sim.objects("registry").is_empty());
    apply(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .expect("the retry is a new apply");
    assert_eq!(setup.sim.objects("registry").len(), 1);
}

#[tokio::test]
async fn an_update_whose_response_was_lost_is_checkpointed_when_the_remote_shows_it() {
    let setup = Setup::new();
    let engine = setup.engine();
    let store = setup.store(&engine);
    apply(&engine, &store, DOCUMENT, PASSWORD).await.unwrap();
    setup
        .sim
        .inject(Fault::new("registry.update", FaultKind::DropAfter));
    let changed = DOCUMENT.replace("username: ci", "username: deploy");
    apply(&engine, &store, &changed, PASSWORD)
        .await
        .unwrap_err();

    let action = recover(&engine, &store, &changed, PASSWORD)
        .await
        .expect("recovers");

    assert_eq!(action, RecoveryAction::CheckpointConfirmedSuccess);
    assert!(clean(&store));
    let compiled = compile(&engine, &changed, &[("GHCR_TOKEN", PASSWORD)]);
    let plan = engine
        .plan(&compiled, store.inspect().unwrap().as_ref())
        .await
        .unwrap();
    assert!(plan.changes().is_empty(), "{plan:?}");
}

#[tokio::test]
async fn an_update_that_never_arrived_is_confirmed_as_no_change() {
    let setup = Setup::new();
    let engine = setup.engine();
    let store = setup.store(&engine);
    apply(&engine, &store, DOCUMENT, PASSWORD).await.unwrap();
    setup
        .sim
        .inject(Fault::new("registry.update", FaultKind::DropBefore));
    let changed = DOCUMENT.replace("username: ci", "username: deploy");
    apply(&engine, &store, &changed, PASSWORD)
        .await
        .unwrap_err();

    let action = recover(&engine, &store, &changed, PASSWORD)
        .await
        .expect("recovers");

    assert_eq!(action, RecoveryAction::ConfirmNoChange);
    assert_eq!(setup.sim.objects("registry")[0]["username"], "ci");
    apply(&engine, &store, &changed, PASSWORD)
        .await
        .expect("applies afterwards");
    assert_eq!(setup.sim.objects("registry")[0]["username"], "deploy");
}

#[tokio::test]
async fn a_secret_rotation_with_a_lost_response_cannot_be_proven_and_needs_a_person() {
    let setup = Setup::new();
    let engine = setup.engine();
    let store = setup.store(&engine);
    apply(&engine, &store, DOCUMENT, PASSWORD).await.unwrap();
    setup
        .sim
        .inject(Fault::new("registry.update", FaultKind::DropAfter));
    apply(&engine, &store, DOCUMENT, ROTATED).await.unwrap_err();

    let error = recover(&engine, &store, DOCUMENT, ROTATED)
        .await
        .unwrap_err();

    assert!(matches!(error, RecoverError::ManualIntervention), "{error}");
    assert!(!clean(&store), "nothing was recorded on a guess");
}

#[tokio::test]
async fn a_delete_whose_response_was_lost_is_checkpointed_when_the_resource_is_gone() {
    let setup = Setup::new();
    let engine = setup.engine();
    let store = setup.store(&engine);
    apply(&engine, &store, DOCUMENT, PASSWORD).await.unwrap();
    let empty = "version: 2\nsettings: {}\n";
    setup
        .sim
        .inject(Fault::new("registry.remove", FaultKind::DropAfter));
    apply(&engine, &store, empty, PASSWORD).await.unwrap_err();

    let action = recover(&engine, &store, empty, PASSWORD)
        .await
        .expect("recovers");

    assert_eq!(action, RecoveryAction::CheckpointConfirmedSuccess);
    assert!(store.inspect().unwrap().unwrap().resources().is_empty());
}

#[tokio::test]
async fn a_delete_that_never_arrived_is_confirmed_as_no_change() {
    let setup = Setup::new();
    let engine = setup.engine();
    let store = setup.store(&engine);
    apply(&engine, &store, DOCUMENT, PASSWORD).await.unwrap();
    let empty = "version: 2\nsettings: {}\n";
    setup
        .sim
        .inject(Fault::new("registry.remove", FaultKind::DropBefore));
    apply(&engine, &store, empty, PASSWORD).await.unwrap_err();

    let action = recover(&engine, &store, empty, PASSWORD)
        .await
        .expect("recovers");

    assert_eq!(action, RecoveryAction::ConfirmNoChange);
    assert_eq!(store.inspect().unwrap().unwrap().resources().len(), 1);
    assert_eq!(setup.sim.objects("registry").len(), 1);
}

#[tokio::test]
async fn a_resource_changed_by_someone_else_meanwhile_is_left_to_a_person() {
    let setup = Setup::new();
    let engine = setup.engine();
    let store = setup.store(&engine);
    setup
        .sim
        .inject(Fault::new("registry.create", FaultKind::DropAfter));
    apply(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .unwrap_err();
    let id = setup.sim.objects("registry")[0]["registryId"]
        .as_str()
        .unwrap()
        .to_owned();
    setup.sim.patch(
        "registry",
        &id,
        &json!({"registryUrl": "elsewhere.example"}),
    );

    let error = recover(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .unwrap_err();

    assert!(matches!(error, RecoverError::ManualIntervention), "{error}");
    assert!(!clean(&store));
}

#[tokio::test]
async fn declining_recovery_records_nothing() {
    let setup = Setup::new();
    let engine = setup.engine();
    let store = setup.store(&engine);
    setup
        .sim
        .inject(Fault::new("registry.create", FaultKind::DropAfter));
    apply(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .unwrap_err();
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", PASSWORD)]);

    let error = engine
        .recover(&compiled, &store, |_| false)
        .await
        .unwrap_err();

    assert!(matches!(error, RecoverError::Declined), "{error}");
    assert!(!clean(&store));
    assert!(store.inspect().unwrap().unwrap().resources().is_empty());
}

/// A kind whose create answers `true` and whose spec says the identity cannot be learned
/// from the response: the create succeeded, but only observation can tell which object it made.
const UNNAMED: &str = "\
kind: gadget
scope: settings
section: gadgets
title: Gadget
class: managed
identity:
  key: name
  collision: [name]
  address: \"gadget.{key}\"
api:
  id: tagId
  create: { op: tag.create }
  update: { op: tag.update }
  remove: { op: tag.remove }
  read:
    list: { op: tag.all, authority: authoritative }
    one: { op: tag.one, id_param: tagId, agree: true }
  create_identity: none
fields:
  name:
    type: text
    min_len: 1
    default: key
write:
  - op: tag.update
    fields: [name]
    shape: partial
ledger:
  readonly: [createdAt, organizationId]
";

#[tokio::test]
async fn a_create_whose_identity_cannot_be_learned_leaves_the_step_open_and_recovery_finds_it() {
    let spec = dokploy_spec::parse_spec(UNNAMED).unwrap();
    let specs =
        dokploy_spec::SpecRegistry::from_specs(vec![spec], &std::collections::BTreeSet::new())
            .unwrap();
    let sim = Sim::new(Arc::new(specs.clone())).with_fixtures(&fixtures());
    let engine = Engine::new(specs, &sim).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = StateStore::for_document(
        directory.path().canonicalize().unwrap(),
        engine.instance().clone(),
        DocumentId::Settings,
    )
    .unwrap();
    let text = "version: 2\nsettings:\n  gadgets:\n    thing: {}\n";
    let compiled = compile(&engine, text, &[]);

    let error = engine.apply(&compiled, &store, |_| true).await.unwrap_err();
    assert!(
        matches!(error, ApplyError::OutcomeUnknown { .. }),
        "{error}"
    );
    assert_eq!(sim.objects("gadget").len(), 1, "the create did happen");

    let mut action = None;
    engine
        .recover(&compiled, &store, |preview| {
            action = Some(preview.action());
            true
        })
        .await
        .expect("recovers");
    assert_eq!(action, Some(RecoveryAction::AdoptCreatedResource));
    assert_eq!(store.inspect().unwrap().unwrap().resources().len(), 1);
}
