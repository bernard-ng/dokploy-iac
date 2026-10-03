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
use dokploy_state::{RecoveryStatus, StateStore};
use serde_json::Value as Json;

use crate::case::{Case, Val, Values};

const KEY: &str = "0199a0c8-2351-7c31-8899-2c8f81983ea5:0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";

pub(crate) struct World<'c> {
    pub(crate) case: &'c Case,
    pub(crate) sim: Sim,
    specs: Arc<SpecRegistry>,
    dir: tempfile::TempDir,
    notes: std::cell::RefCell<Vec<String>>,
    /// The remote identity of the kind's parent, when it has one.
    parent_id: std::cell::RefCell<Option<String>>,
}

pub(crate) type Check<T = ()> = Result<T, String>;

impl<'c> World<'c> {
    pub(crate) fn new(case: &'c Case, specs: &Arc<SpecRegistry>, fixtures: &Path) -> Self {
        let world = Self {
            case,
            sim: Sim::new(specs.clone()).with_fixtures(fixtures),
            specs: specs.clone(),
            dir: tempfile::tempdir().expect("a temporary workspace"),
            notes: std::cell::RefCell::default(),
            parent_id: std::cell::RefCell::default(),
        };
        world.apply_ancestors();
        world.seed_selector_targets();

        world
    }

    /// Puts the resources above the kind in place, remotely and in state, as if an earlier
    /// apply had made them: the suite exercises the kind, not the kinds above it.
    fn apply_ancestors(&self) {
        use dokploy_state::{
            ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceName, ResourceState,
            SensitiveInputs, StateFile,
        };

        if self.case.ancestors.is_empty() {
            return;
        }
        let engine = self.engine();
        let mut resources = std::collections::BTreeMap::new();
        let mut parent: Option<(ResourceAddress, String)> = None;
        for ancestor in &self.case.ancestors {
            let mut remote = serde_json::Map::new();
            let mut managed = serde_json::Map::new();
            for (name, field) in &ancestor.spec.fields {
                if field.default.as_deref() == Some("key") {
                    remote.insert(
                        field.request_name(name).to_owned(),
                        Json::String(ancestor.key.clone()),
                    );
                    managed.insert(name.clone(), Json::String(ancestor.key.clone()));
                }
            }
            if let (Some(attach), Some((_, parent_id))) = (&ancestor.attach, &parent) {
                remote.insert(attach.clone(), Json::String(parent_id.clone()));
            }
            let id = self.sim.seed(&ancestor.spec.kind, &Json::Object(remote));

            let kind: ResourceKind = ancestor.spec.kind.parse().expect("a registered kind");
            let name = ResourceName::new(ancestor.key.clone()).expect("a valid key");
            let address = match &parent {
                Some((parent, _)) => parent.child(kind, name).expect("a valid address"),
                None => ResourceAddress::new(kind, name),
            };
            let resource = ResourceState::try_new(
                kind,
                RemoteId::new(id.clone()).expect("a valid identity"),
                false,
                ManagedInputs::try_from_json(Json::Object(managed)).expect("valid inputs"),
                SensitiveInputs::default(),
                parent.as_ref().map(|(parent, _)| parent.clone()),
                Vec::new(),
            )
            .expect("a valid resource");
            resources.insert(address.clone(), resource);
            *self.parent_id.borrow_mut() = Some(id.clone());
            parent = Some((address, id));
        }
        let state = StateFile::with_resources(
            semver::Version::new(0, 1, 0),
            engine.instance().clone(),
            self.case.document_id(),
            resources,
        )
        .expect("a valid hierarchy");
        let store = self.store(&engine);
        let mut session = store.begin_write().expect("the writer lock");
        session
            .checkpoint(dokploy_state::ExpectedState::absent(), &state)
            .expect("the ancestors are recorded");
    }

    /// Puts the resources the kind's selector fields point at in place, under the ids the
    /// suite gave them.
    fn seed_selector_targets(&self) {
        for (kind, id, name) in self.case.selector_targets() {
            self.seed_target(&kind, &id, &name);
        }
    }

    /// Adds a resource a selector can point at, with the given id and name.
    pub(crate) fn seed_target(&self, kind: &str, id: &str, name: &str) {
        let Some(spec) = self.specs.get(kind) else {
            return;
        };
        let Some(key) = spec.identity.key.as_deref() else {
            return;
        };
        let Some(field) = spec.fields.get(key) else {
            return;
        };
        let mut object = serde_json::Map::new();
        object.insert(spec.api.id.clone(), Json::String(id.to_owned()));
        object.insert(
            field.request_name(key).to_owned(),
            Json::String(name.to_owned()),
        );
        self.sim.seed(kind, &Json::Object(object));
    }

    /// Adds a resource of the kind that nobody manages, attached to the seeded parent.
    pub(crate) fn seed_child(&self, fields: &Json) -> String {
        let mut object = fields.as_object().cloned().unwrap_or_default();
        if let Some(parent) = self.parent_id.borrow().clone() {
            for (field, value) in self.case.attachments(&parent) {
                object.insert(field, Json::String(value));
            }
        }

        self.sim.seed(&self.case.spec.kind, &Json::Object(object))
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
            self.case.document_id(),
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
        for (key, content) in &env {
            if let Some(path) = key.strip_prefix(crate::case::FILE_PREFIX) {
                let path = self.dir.path().join(path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("the workspace is writable");
                }
                std::fs::write(&path, content).expect("the workspace is writable");
            }
        }
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

    /// How many resources of the kind state records (its ancestors are not counted).
    pub(crate) fn state_resources(&self) -> usize {
        let engine = self.engine();
        self.store(&engine)
            .inspect()
            .ok()
            .flatten()
            .map_or(0, |state| {
                state
                    .resources()
                    .keys()
                    .filter(|address| address.kind().as_str() == self.case.spec.kind)
                    .count()
            })
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
                if path
                    .file_name()
                    .is_some_and(|name| name == crate::case::CONTENT_DIRECTORY)
                {
                    // The suite put its own canaries here, as the document's sources.
                    continue;
                }
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
            Val::Selector { id, .. } => Json::String(id.clone()),
            Val::SelectorSet { members, .. } => Json::Array(
                members
                    .iter()
                    .map(|(_, id)| Json::String(id.clone()))
                    .collect(),
            ),
            Val::Environment { variables, .. } => {
                Json::String(crate::case::environment_text(variables))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world_for_registry() -> (Case, Arc<SpecRegistry>) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let specs = Arc::new(dokploy_spec::load_dir(&root.join("specs")).expect("specs load"));
        let case = Case::new(
            specs.get("registry").expect("a registry spec"),
            &specs,
            None,
            &BTreeMap::new(),
        )
        .expect("a case");

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
