//! A selector that names a resource the document itself declares is a reference inside the
//! document: it resolves once that resource is made, the one that names it depends on it (so it is
//! made after it and removed before it), and a name nothing declares still blocks the plan.

mod common;

use common::project::sim;
use common::{compile_in, engine};
use dokploy_engine::ApplyError;
use dokploy_state::{DocumentId, StateStore};

struct Settings {
    sim: dokploy_sim::Sim,
    directory: tempfile::TempDir,
}

impl Settings {
    fn new() -> Self {
        Self {
            sim: sim(),
            directory: tempfile::tempdir().unwrap(),
        }
    }

    fn store(&self, engine: &dokploy_engine::Engine<&dokploy_sim::Sim>) -> StateStore {
        StateStore::for_document(
            self.directory.path().canonicalize().unwrap(),
            engine.instance().clone(),
            DocumentId::Settings,
        )
        .unwrap()
    }

    async fn apply(&self, text: &str) -> Result<usize, ApplyError> {
        let engine = engine(&self.sim);
        let compiled = compile_in(&engine, self.directory.path(), text, &[]);
        engine
            .apply(&compiled, &self.store(&engine), |_| true)
            .await
            .map(|summary| summary.applied())
    }

    fn operations(&self) -> Vec<String> {
        self.sim
            .mutations()
            .iter()
            .map(|logged| logged.operation().to_owned())
            .collect()
    }
}

const KEY: &str = "ssh_keys:\n    deploy:\n      public_key: ssh-ed25519 AAAA deploy\n      private_key: { env: DEPLOY_KEY }\n";

fn document(servers: &str) -> String {
    format!("version: 2\nsettings:\n  {KEY}  servers:\n{servers}")
}

const SERVER: &str = "    edge:\n      ip_address: 203.0.113.7\n      port: 22\n      username: root\n      ssh_key: { name: deploy }\n      type: deploy\n      docker_cleanup: true\n      command: \"\"\n";

#[tokio::test]
async fn a_server_naming_a_key_of_the_same_document_is_made_after_it() {
    let world = Settings::new();
    let engine = engine(&world.sim);
    let text = document(SERVER);
    let document = engine.parse(&text).expect("valid");
    let secrets = dokploy_engine::WorkspaceSecrets::new(world.directory.path(), |name: &str| {
        (name == "DEPLOY_KEY").then(|| "key-canary-1".to_owned())
    });
    let compiled = engine
        .compile(&document, &engine.fingerprinter(common::key()), &secrets)
        .expect("compiles");

    let summary = engine
        .apply(&compiled, &world.store(&engine), |_| true)
        .await
        .expect("applies");

    assert_eq!(summary.applied(), 2);
    let operations = world.operations();
    let key_at = operations
        .iter()
        .position(|op| op == "sshKey.create")
        .unwrap();
    let server_at = operations
        .iter()
        .position(|op| op == "server.create")
        .unwrap();
    assert!(key_at < server_at, "{operations:?}");
    let keys = world.sim.objects("ssh_key");
    let server = world.sim.objects("server").remove(0);
    assert_eq!(
        server["sshKeyId"], keys[0]["sshKeyId"],
        "the id of the key made first"
    );

    // Converged, and removed in the other order.
    let plan = engine
        .plan(&compiled, world.store(&engine).inspect().unwrap().as_ref())
        .await
        .unwrap();
    assert!(plan.applyable() && plan.changes().is_empty(), "{plan:?}");
    world.sim.clear_requests();
    engine
        .apply(&compiled.cleared(), &world.store(&engine), |_| true)
        .await
        .expect("removes");
    let operations = world.operations();
    let server_gone = operations
        .iter()
        .position(|op| op == "server.remove")
        .unwrap();
    let key_gone = operations
        .iter()
        .position(|op| op == "sshKey.remove")
        .unwrap();
    assert!(server_gone < key_gone, "{operations:?}");
}

#[tokio::test]
async fn a_name_nothing_declares_still_blocks_the_plan() {
    let world = Settings::new();
    let text = format!(
        "version: 2\nsettings:\n  servers:\n{}",
        SERVER.replace("deploy }", "nowhere }")
    );

    let error = world.apply(&text).await.unwrap_err();

    assert!(matches!(error, ApplyError::Blocked { .. }), "{error}");
    assert!(world.operations().is_empty(), "nothing was sent");
}
