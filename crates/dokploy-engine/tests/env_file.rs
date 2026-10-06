//! An `env_file` expands a dotenv file into the `environment` of an application or a Compose
//! stack: what the document writes wins, the file's variables are secret and never reach
//! state, a change of the file is a change of the variables it touches, and a problem in the
//! file is named by its line and never by a value.

mod common;

use std::path::Path;

use common::compile_in;
use common::project::ProjectWorld;
use dokploy_engine::{CompileError, DotenvError, EngineError};
use serde_json::{Value, json};

const FROM_FILE: &str = "from-file-canary-1";
const EXPLICIT: &str = "explicit-canary-2";
const SHADOWED: &str = "shadowed-canary-3";

fn document(extra: &str) -> String {
    format!(
        "version: 2\nproject:\n  slug: shop\n  environments:\n    production:\n      applications:\n        api:\n          source: {{ type: github, owner: acme, repository: shop, branch: main }}\n{extra}"
    )
}

impl ProjectWorld {
    fn write(&self, path: &str, text: &str) {
        let path = self.directory.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn compiled(&self, text: &str) -> Result<dokploy_engine::Compiled, CompileError> {
        let engine = self.engine();
        let document = engine.parse(text).expect("the document is valid");
        let secrets = dokploy_engine::WorkspaceSecrets::new(self.directory.path(), |_: &str| None);
        engine.compile(&document, &engine.fingerprinter(common::key()), &secrets)
    }

    async fn apply_here(&self, text: &str) {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = compile_in(&engine, self.directory.path(), text, &[]);
        engine
            .apply(&compiled, &store, |_| true)
            .await
            .expect("applies");
    }

    async fn pending_here(&self, text: &str) -> usize {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = compile_in(&engine, self.directory.path(), text, &[]);
        let state = store.inspect().unwrap();
        let plan = engine.plan(&compiled, state.as_ref()).await.unwrap();
        assert!(plan.applyable(), "{:?}", plan.diagnostics());
        plan.changes().len()
    }

    fn environment(&self) -> String {
        let mut all = self.sim.objects("application");
        assert_eq!(all.len(), 1, "{all:?}");
        match all.remove(0)["env"].clone() {
            Value::String(text) => text,
            other => panic!("the environment is not text: {other}"),
        }
    }
}

fn lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    lines.sort_unstable();
    lines
}

#[tokio::test]
async fn the_variables_of_the_file_are_written_and_what_the_document_writes_wins() {
    let world = ProjectWorld::new();
    world.write(
        "secrets/api.env",
        &format!("# the api\nDATABASE_URL={FROM_FILE}\nLOG_LEVEL={SHADOWED}\nexport PORT=8080\n"),
    );
    let text = document(&format!(
        "          environment:\n            LOG_LEVEL: {EXPLICIT}\n          env_file: {{ file: secrets/api.env }}\n"
    ));

    world.apply_here(&text).await;

    assert_eq!(
        lines(&world.environment()),
        [
            format!("DATABASE_URL={FROM_FILE}").as_str(),
            format!("LOG_LEVEL={EXPLICIT}").as_str(),
            "PORT=8080"
        ],
        "the explicit LOG_LEVEL wins over the file's"
    );
    assert_eq!(world.pending_here(&text).await, 0, "converged");
}

#[tokio::test]
async fn a_change_of_the_file_is_a_change_of_the_variables_it_touches() {
    let world = ProjectWorld::new();
    let text = document("          env_file: { file: secrets/api.env }\n");
    world.write("secrets/api.env", "A=1\nB=2\n");
    world.apply_here(&text).await;
    world.sim.clear_requests();

    // A new value, a new variable, and one the file no longer gives.
    world.write("secrets/api.env", "A=1\nB=changed\nC=3\n");
    assert_eq!(world.pending_here(&text).await, 1);
    world.apply_here(&text).await;
    assert_eq!(lines(&world.environment()), ["A=1", "B=changed", "C=3"]);

    // A variable the file stops giving is removed, and the next plan has nothing to do.
    world.write("secrets/api.env", "A=1\nC=3\n");
    assert_eq!(world.pending_here(&text).await, 1);
    world.apply_here(&text).await;
    assert_eq!(lines(&world.environment()), ["A=1", "C=3"]);
    assert_eq!(world.pending_here(&text).await, 0);
}

#[tokio::test]
async fn an_empty_file_with_nothing_written_leaves_no_variables() {
    let world = ProjectWorld::new();
    world.write("secrets/api.env", "A=1\n");
    let text = document("          env_file: { file: secrets/api.env }\n");
    world.apply_here(&text).await;
    world.write("secrets/api.env", "# nothing yet\n");

    world.apply_here(&text).await;

    assert!(
        lines(&world.environment()).is_empty(),
        "{:?}",
        world.environment()
    );
}

#[tokio::test]
async fn a_file_that_cannot_be_read_fails_the_compile_without_a_value() {
    let world = ProjectWorld::new();
    let text = document("          env_file: { file: secrets/api.env }\n");

    let missing = world.compiled(&text).unwrap_err();
    assert!(matches!(missing, CompileError::Secret { .. }), "{missing}");

    world.write("secrets/api.env", &format!("OK=1\n{FROM_FILE}\n"));
    let error = world.compiled(&text).unwrap_err();
    assert!(
        matches!(
            error,
            CompileError::EnvFile {
                source: DotenvError::NotAVariable { line: 2 },
                ..
            }
        ),
        "{error}"
    );
    assert!(!error.to_string().contains(FROM_FILE), "{error}");

    world.write(
        "secrets/api.env",
        &format!("A={FROM_FILE}\nA={FROM_FILE}\n"),
    );
    let error = world.compiled(&text).unwrap_err();
    assert!(error.to_string().contains("line 2"), "{error}");
    assert!(!error.to_string().contains(FROM_FILE), "{error}");
}

#[tokio::test]
async fn the_values_of_the_file_never_reach_state_or_the_journal() {
    let world = ProjectWorld::new();
    world.write("secrets/api.env", &format!("DATABASE_URL={FROM_FILE}\n"));
    world
        .apply_here(&document("          env_file: { file: secrets/api.env }\n"))
        .await;

    let mut everything = String::new();
    fn walk(path: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().is_some_and(|name| name != "api.env") {
                out.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
            }
        }
    }
    walk(world.directory.path(), &mut everything);
    assert!(!everything.is_empty(), "state was written");
    assert!(
        !everything.contains(FROM_FILE),
        "a value from the dotenv file reached the workspace"
    );
    assert!(
        everything.contains("DATABASE_URL"),
        "the variable's name is state, its value is not"
    );
}

#[tokio::test]
async fn a_variable_dropped_from_the_map_is_removed_and_one_never_owned_stays() {
    let world = ProjectWorld::new();
    let two = document("          environment:\n            A: \"1\"\n            B: \"2\"\n");
    world.apply_here(&two).await;
    // Someone adds a variable at Dokploy that the document never named.
    let id = world.sim.objects("application")[0]["applicationId"]
        .as_str()
        .unwrap()
        .to_owned();
    world.sim.patch(
        "application",
        &id,
        &serde_json::json!({ "env": "A=1\nB=2\nSTRANGER=x" }),
    );

    let one = document("          environment:\n            A: \"1\"\n");
    assert_eq!(world.pending_here(&one).await, 1);
    world.apply_here(&one).await;

    assert_eq!(
        lines(&world.environment()),
        ["A=1", "STRANGER=x"],
        "B was the document's and is removed; STRANGER never was"
    );
    assert_eq!(world.pending_here(&one).await, 0, "converged");
}

#[tokio::test]
async fn a_vault_variable_is_sent_as_the_reference_dokploy_resolves() {
    let world = ProjectWorld::new();
    world
        .sim
        .seed("vault_provider", &json!({ "name": "infisical" }));
    let text = document(
        "          environment:\n            MAIL_KEY: { vault: { provider: infisical, secret: MAIL_KEY } }\n            LOG_LEVEL: info\n",
    );

    world.apply_here(&text).await;

    assert_eq!(
        lines(&world.environment()),
        ["LOG_LEVEL=info", "MAIL_KEY=${{vault.infisical.MAIL_KEY}}"]
    );
    assert_eq!(world.pending_here(&text).await, 0, "converged");
}

#[tokio::test]
async fn a_vault_variable_whose_provider_does_not_exist_blocks_planning() {
    let world = ProjectWorld::new();
    let text = document(
        "          environment:\n            MAIL_KEY: { vault: { provider: missing, secret: MAIL_KEY } }\n",
    );
    let engine = world.engine();
    let compiled = compile_in(&engine, world.directory.path(), &text, &[]);
    let state = world.store(&engine).inspect().unwrap();

    assert!(matches!(
        engine.plan(&compiled, state.as_ref()).await,
        Err(EngineError::VaultProvider { property, .. }) if property == "environment.MAIL_KEY"
    ));
}

#[tokio::test]
async fn a_variable_changed_at_dokploy_is_a_drift_that_apply_restores() {
    let world = ProjectWorld::new();
    world.write("secrets/api.env", &format!("DATABASE_URL={FROM_FILE}\n"));
    let text = document(
        "          environment:\n            LOG_LEVEL: info\n          env_file: { file: secrets/api.env }\n",
    );
    world.apply_here(&text).await;
    assert_eq!(world.pending_here(&text).await, 0);

    let id = world.sim.objects("application")[0]["applicationId"]
        .as_str()
        .unwrap()
        .to_owned();
    world.sim.patch(
        "application",
        &id,
        &serde_json::json!({ "env": format!("DATABASE_URL=edited-in-the-dashboard\nLOG_LEVEL=info\nSTRANGER=x") }),
    );

    assert_eq!(
        world.pending_here(&text).await,
        1,
        "the changed value is seen"
    );
    world.apply_here(&text).await;
    assert_eq!(
        lines(&world.environment()),
        [
            format!("DATABASE_URL={FROM_FILE}").as_str(),
            "LOG_LEVEL=info",
            "STRANGER=x"
        ],
        "restored, and a variable the document never owned is left alone"
    );
    assert_eq!(world.pending_here(&text).await, 0);
}
