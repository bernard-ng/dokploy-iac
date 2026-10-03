//! One scenario's world: a fresh simulator, a fresh workspace, and an engine over them.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use dokploy_core::Plan;
use dokploy_engine::{
    ApplyError, ApplySummary, Compiled, Engine, FingerprintKey, RecoverError, RecoveryAction,
    WorkspaceSecrets,
};
use dokploy_sim::Sim;
use dokploy_spec::SpecRegistry;
use dokploy_state::{DocumentId, RecoveryStatus, StateStore};
use serde_json::Value as Json;

use crate::case::{Case, Val, Values};

const KEY: &str = "0199a0c8-2351-7c31-8899-2c8f81983ea5:0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";

pub(crate) struct World<'c> {
    pub(crate) case: &'c Case,
    pub(crate) sim: Sim,
    specs: Arc<SpecRegistry>,
    dir: tempfile::TempDir,
    notes: std::cell::RefCell<Vec<String>>,
}

pub(crate) type Check<T = ()> = Result<T, String>;

impl<'c> World<'c> {
    pub(crate) fn new(case: &'c Case, specs: &Arc<SpecRegistry>, fixtures: &Path) -> Self {
        Self {
            case,
            sim: Sim::new(specs.clone()).with_fixtures(fixtures),
            specs: specs.clone(),
            dir: tempfile::tempdir().expect("a temporary workspace"),
            notes: std::cell::RefCell::default(),
        }
    }

    pub(crate) fn engine(&self) -> Engine<&Sim> {
        Engine::new((*self.specs).clone(), &self.sim).expect("the engine builds")
    }

    pub(crate) fn store(&self, engine: &Engine<&Sim>) -> StateStore {
        StateStore::for_document(
            self.dir
                .path()
                .canonicalize()
                .expect("the workspace exists"),
            engine.instance().clone(),
            DocumentId::Settings,
        )
        .expect("a state store")
    }

    /// Remembers text that must never contain a secret.
    pub(crate) fn note(&self, text: impl Into<String>) {
        self.notes.borrow_mut().push(text.into());
    }

    fn compile(
        &self,
        engine: &Engine<&Sim>,
        text: &str,
        env: BTreeMap<String, String>,
    ) -> Check<Compiled> {
        let document = engine
            .parse(text)
            .map_err(|error| format!("the generated document is invalid:\n{error}\n{text}"))?;
        let secrets = WorkspaceSecrets::new(self.dir.path().to_path_buf(), move |name: &str| {
            env.get(name).cloned()
        });
        let key = FingerprintKey::parse_explicit(KEY).expect("the key parses");
        engine
            .compile(&document, &engine.fingerprinter(key), &secrets)
            .map_err(|error| format!("the generated document does not compile: {error}"))
    }

    pub(crate) async fn apply_text(
        &self,
        text: &str,
        env: BTreeMap<String, String>,
    ) -> Check<Result<ApplySummary, ApplyError>> {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = self.compile(&engine, text, env)?;
        let result = engine.apply(&compiled, &store, |_| true).await;
        if let Err(error) = &result {
            self.note(format!("{error} {error:?}"));
        }

        Ok(result)
    }

    pub(crate) async fn apply(&self, values: &Values) -> Check<Result<ApplySummary, ApplyError>> {
        self.apply_text(
            &self.case.document(values, false),
            self.case.environment(values),
        )
        .await
    }

    /// Applies and requires success.
    pub(crate) async fn apply_ok(&self, values: &Values) -> Check<ApplySummary> {
        self.apply(values)
            .await?
            .map_err(|error| format!("apply failed: {error}"))
    }

    pub(crate) async fn plan_text(&self, text: &str, env: BTreeMap<String, String>) -> Check<Plan> {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = self.compile(&engine, text, env)?;
        let state = store
            .inspect()
            .map_err(|error| format!("reading state: {error}"))?;
        let plan = engine
            .plan(&compiled, state.as_ref())
            .await
            .map_err(|error| format!("planning failed: {error}"))?;
        self.note(format!("{plan:?}"));
        self.note(String::from_utf8_lossy(&plan.to_json_bytes()).into_owned());

        Ok(plan)
    }

    pub(crate) async fn plan(&self, values: &Values) -> Check<Plan> {
        self.plan_text(
            &self.case.document(values, false),
            self.case.environment(values),
        )
        .await
    }

    pub(crate) async fn recover(
        &self,
        values: &Values,
    ) -> Check<Result<RecoveryAction, RecoverError>> {
        self.recover_text(
            &self.case.document(values, false),
            self.case.environment(values),
        )
        .await
    }

    /// Applies, with the caller declining the plan.
    pub(crate) async fn apply_declined(
        &self,
        values: &Values,
    ) -> Check<Result<ApplySummary, ApplyError>> {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = self.compile(
            &engine,
            &self.case.document(values, false),
            self.case.environment(values),
        )?;

        Ok(engine.apply(&compiled, &store, |_| false).await)
    }

    pub(crate) async fn recover_text(
        &self,
        text: &str,
        env: BTreeMap<String, String>,
    ) -> Check<Result<RecoveryAction, RecoverError>> {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = self.compile(&engine, text, env)?;
        let mut action = None;
        let result = engine
            .recover(&compiled, &store, |preview| {
                action = Some(preview.action());
                true
            })
            .await;
        if let Err(error) = &result {
            self.note(format!("{error} {error:?}"));
        }

        Ok(result.map(|_| action.expect("approval is asked before anything is recorded")))
    }

    pub(crate) fn state_resources(&self) -> usize {
        let engine = self.engine();
        self.store(&engine)
            .inspect()
            .ok()
            .flatten()
            .map_or(0, |state| state.resources().len())
    }

    pub(crate) fn recovery_clean(&self) -> bool {
        let engine = self.engine();
        matches!(
            self.store(&engine).recovery_status(),
            Ok(RecoveryStatus::Clean)
        )
    }

    pub(crate) fn objects(&self) -> Vec<Json> {
        self.sim.objects(&self.case.spec.kind)
    }

    /// The single remote object, or why there is not one.
    pub(crate) fn only_object(&self) -> Check<Json> {
        let mut objects = self.objects();
        if objects.len() != 1 {
            return Err(format!(
                "expected one remote object, found {}",
                objects.len()
            ));
        }

        Ok(objects.remove(0))
    }

    pub(crate) fn operations(&self) -> Vec<String> {
        self.sim
            .mutations()
            .iter()
            .map(|m| m.operation().to_owned())
            .collect()
    }

    /// No secret may appear on disk or in anything the scenario printed.
    pub(crate) fn scan(&self) -> Check {
        fn walk(path: &Path, out: &mut String) {
            let Ok(entries) = std::fs::read_dir(path) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push_str(&text);
                }
            }
        }
        let mut haystack = String::new();
        walk(self.dir.path(), &mut haystack);
        haystack.push_str(&self.notes.borrow().join("\n"));
        for canary in self.case.canaries() {
            if haystack.contains(&canary) {
                return Err(format!("the secret `{canary}` leaked"));
            }
        }

        Ok(())
    }
}

impl Val {
    pub(crate) fn json(&self) -> Json {
        match self {
            Val::Json(json) => json.clone(),
            Val::Secret(secret) => Json::String(secret.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world_for_registry() -> (Case, Arc<SpecRegistry>) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let specs = Arc::new(dokploy_spec::load_dir(&root.join("specs")).expect("specs load"));
        let case = Case::new(specs.get("registry").expect("a registry spec")).expect("a case");

        (case, specs)
    }

    #[test]
    fn the_scan_finds_a_secret_on_disk_and_in_printed_text() {
        let (case, specs) = world_for_registry();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6");
        let canary = case.canaries().remove(0);

        let world = World::new(&case, &specs, &fixtures);
        assert!(world.scan().is_ok(), "a clean world has nothing to find");

        std::fs::create_dir_all(world.dir.path().join(".dokploy")).unwrap();
        std::fs::write(
            world.dir.path().join(".dokploy/state.json"),
            format!("{{\"x\":\"{canary}\"}}"),
        )
        .unwrap();
        assert!(world.scan().is_err(), "a secret in a file is found");

        let other = World::new(&case, &specs, &fixtures);
        other.note(format!("error: {canary}"));
        assert!(other.scan().is_err(), "a secret in printed text is found");
    }
}
