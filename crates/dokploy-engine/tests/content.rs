//! A content property (a file's text) is compared with what Dokploy holds through the keyed
//! fingerprint of both, so an edit made behind the engine's back is a drift, the text is never
//! shown, and a property that says `trim` ignores the whitespace Dokploy adds or drops around it.

mod common;

use common::compile_in;
use common::project::ProjectWorld;
use serde_json::{Value, json};

const CANARY: &str = "motd-content-canary-1";

fn document() -> String {
    "version: 2\nproject:\n  slug: shop\n  environments:\n    production:\n      applications:\n        api:\n          source: { type: github, owner: acme, repository: shop, branch: main }\n          patches:\n            motd:\n              file_path: README.md\n              type: create\n              content: { file: patches/motd.txt }\n"
        .to_owned()
}

impl ProjectWorld {
    fn write_patch(&self, text: &str) {
        let path = self.directory.path().join("patches/motd.txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    async fn apply_document(&self) {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = compile_in(&engine, self.directory.path(), &document(), &[]);
        engine
            .apply(&compiled, &store, |_| true)
            .await
            .expect("applies");
    }

    async fn changes(&self) -> Vec<String> {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = compile_in(&engine, self.directory.path(), &document(), &[]);
        let state = store.inspect().unwrap();
        let plan = engine.plan(&compiled, state.as_ref()).await.unwrap();
        assert!(plan.applyable(), "{:?}", plan.diagnostics());
        let text = format!("{plan:?}");
        assert!(!text.contains(CANARY), "the plan shows the content");
        plan.changes()
            .iter()
            .flat_map(|change| {
                change
                    .fields()
                    .iter()
                    .map(|field| format!("{:?}", field.key()))
            })
            .collect()
    }

    fn patch(&self) -> Value {
        let mut all = self.sim.objects("patch");
        assert_eq!(all.len(), 1, "{all:?}");
        all.remove(0)
    }

    fn set_remote_content(&self, text: &str) {
        let id = self.patch()["patchId"].as_str().unwrap().to_owned();
        self.sim.patch("patch", &id, &json!({ "content": text }));
    }
}

#[tokio::test]
async fn an_edit_made_behind_the_engines_back_is_a_drift_that_apply_restores() {
    let world = ProjectWorld::new();
    world.write_patch(&format!("{CANARY}\n"));
    world.apply_document().await;
    assert_eq!(world.patch()["content"], format!("{CANARY}\n"));
    assert!(world.changes().await.is_empty(), "converged");

    world.set_remote_content("someone edited this in the dashboard");
    let changes = world.changes().await;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert!(changes[0].contains("content"), "{changes:?}");

    world.apply_document().await;
    assert_eq!(world.patch()["content"], format!("{CANARY}\n"));
    assert!(world.changes().await.is_empty(), "restored");
}

#[tokio::test]
async fn whitespace_around_a_trimmed_property_is_not_a_difference() {
    let world = ProjectWorld::new();
    world.write_patch("line one\nline two");
    world.apply_document().await;

    // Dokploy stores the content of an update with a trailing newline (captured on 0.30.6 and
    // 0.30.7), and the patch spec says `trim`.
    world.set_remote_content("line one\nline two\n");

    assert!(world.changes().await.is_empty());
}

#[tokio::test]
async fn a_changed_local_file_is_a_change_and_the_new_text_is_sent() {
    let world = ProjectWorld::new();
    world.write_patch("first");
    world.apply_document().await;

    world.write_patch("second");

    assert_eq!(world.changes().await.len(), 1);
    world.apply_document().await;
    assert_eq!(world.patch()["content"], "second");
    assert!(world.changes().await.is_empty());
}
