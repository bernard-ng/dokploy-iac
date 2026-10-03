//! `moves` and `removed`: a rename keeps the remote identity, with or without a change in the same
//! step, a removal forgets or deletes, and a declaration that was already applied does nothing.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{compile, engine, specs};
use dokploy_engine::{ApplyError, Engine};
use dokploy_sim::Sim;
use dokploy_state::{DocumentId, StateStore};

fn sim() -> Sim {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6");
    Sim::new(Arc::new(specs())).with_fixtures(&fixtures)
}

fn registry(key: &str, username: &str, extra: &str) -> String {
    format!(
        "version: 2\nsettings:\n  registries:\n    {key}:\n      name: shared-registry\n      type: cloud\n      url: ghcr.io\n      username: {username}\n      password:\n        env: GHCR_TOKEN\n{extra}"
    )
}

struct World {
    sim: Sim,
    directory: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        Self {
            sim: sim(),
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

    async fn apply(&self, text: &str) -> Result<dokploy_engine::ApplySummary, ApplyError> {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = compile(&engine, text, &[("GHCR_TOKEN", "token-canary")]);
        engine.apply(&compiled, &store, |_| true).await
    }

    async fn plan_changes(&self, text: &str) -> usize {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = compile(&engine, text, &[("GHCR_TOKEN", "token-canary")]);
        let state = store.inspect().unwrap();
        let plan = engine.plan(&compiled, state.as_ref()).await.unwrap();
        assert!(plan.applyable(), "{:?}", plan.diagnostics());
        plan.changes().len()
    }

    fn addresses(&self) -> Vec<String> {
        let engine = self.engine();
        let store = self.store(&engine);
        store
            .inspect()
            .unwrap()
            .map(|state| state.resources().keys().map(ToString::to_string).collect())
            .unwrap_or_default()
    }
}

const MOVE: &str = "moves:\n  - { from: registry.old, to: registry.new }\n";

fn tag(key: &str, extra: &str) -> String {
    format!(
        "version: 2\nsettings:\n  tags:\n    {key}:\n      name: production\n      color: \"#ff0000\"\n{extra}"
    )
}

#[tokio::test]
async fn a_move_keeps_the_remote_identity_and_sends_nothing() {
    let world = World::new();
    world.apply(&tag("old", "")).await.unwrap();
    let id = world.sim.objects("tag")[0]["tagId"].clone();
    world.sim.clear_requests();

    let moved = tag("new", "moves:\n  - { from: tag.old, to: tag.new }\n");
    assert_eq!(world.plan_changes(&moved).await, 1);
    world.apply(&moved).await.unwrap();

    assert_eq!(world.addresses(), ["tag.new"]);
    assert!(world.sim.mutations().is_empty(), "a rename is state only");
    assert_eq!(world.sim.objects("tag").len(), 1);
    assert_eq!(world.sim.objects("tag")[0]["tagId"], id);
}

#[tokio::test]
async fn a_moved_secret_is_sent_again_because_its_receipt_is_bound_to_the_address() {
    let world = World::new();
    world.apply(&registry("old", "ci", "")).await.unwrap();
    let id = world.sim.objects("registry")[0]["registryId"].clone();
    world.sim.clear_requests();

    world.apply(&registry("new", "ci", MOVE)).await.unwrap();

    assert_eq!(world.addresses(), ["registry.new"]);
    assert_eq!(world.sim.objects("registry")[0]["registryId"], id);
    let operations: Vec<String> = world
        .sim
        .mutations()
        .iter()
        .map(|m| m.operation().to_owned())
        .collect();
    assert_eq!(
        operations,
        ["registry.update"],
        "the same secret, written again"
    );
}

#[tokio::test]
async fn a_move_that_also_changes_a_property_is_an_update_of_the_same_resource() {
    let world = World::new();
    world.apply(&registry("old", "ci", "")).await.unwrap();
    let id = world.sim.objects("registry")[0]["registryId"].clone();
    world.sim.clear_requests();

    world
        .apply(&registry("new", "deployer", MOVE))
        .await
        .unwrap();

    assert_eq!(world.addresses(), ["registry.new"]);
    let objects = world.sim.objects("registry");
    assert_eq!(objects.len(), 1, "no second registry was made");
    assert_eq!(objects[0]["registryId"], id);
    assert_eq!(objects[0]["username"], "deployer");
    let operations: Vec<String> = world
        .sim
        .mutations()
        .iter()
        .map(|m| m.operation().to_owned())
        .collect();
    assert_eq!(operations, ["registry.update"]);
}

#[tokio::test]
async fn a_move_that_was_already_applied_changes_nothing() {
    let world = World::new();
    world.apply(&registry("old", "ci", "")).await.unwrap();
    let moved = registry("new", "ci", MOVE);
    world.apply(&moved).await.unwrap();

    assert_eq!(
        world.plan_changes(&moved).await,
        0,
        "the declaration stays harmless"
    );
}

#[tokio::test]
async fn an_unknown_or_ambiguous_address_names_the_directive() {
    let world = World::new();
    world.apply(&registry("old", "ci", "")).await.unwrap();
    let engine = world.engine();
    let store = world.store(&engine);
    let state = store.inspect().unwrap();

    let ambiguous = registry(
        "new",
        "ci",
        "moves:\n  - { from: registry.old, to: registry.nothing }\n",
    );
    let compiled = compile(&engine, &ambiguous, &[("GHCR_TOKEN", "x")]);
    let error = engine
        .plan(&compiled, state.as_ref())
        .await
        .expect_err("no such target");
    assert!(error.to_string().contains("moves"), "{error}");
}

#[tokio::test]
async fn removed_forgets_a_resource_or_deletes_it() {
    let world = World::new();
    world.apply(&registry("old", "ci", "")).await.unwrap();
    let empty = "version: 2\nsettings: {}\n";

    // `destroy` defaults to false: stop managing it, and leave it at Dokploy.
    let forget = format!("{empty}removed:\n  - {{ from: registry.old }}\n");
    world.apply(&forget).await.unwrap();
    assert!(world.addresses().is_empty());
    assert_eq!(world.sim.objects("registry").len(), 1, "it is still there");
    assert_eq!(
        world.plan_changes(&forget).await,
        0,
        "the declaration is harmless once it is applied"
    );

    let world = World::new();
    world.apply(&registry("old", "ci", "")).await.unwrap();
    let destroy = format!("{empty}removed:\n  - {{ from: registry.old, destroy: true }}\n");
    world.apply(&destroy).await.unwrap();
    assert!(world.addresses().is_empty());
    assert!(world.sim.objects("registry").is_empty(), "it was deleted");
}
