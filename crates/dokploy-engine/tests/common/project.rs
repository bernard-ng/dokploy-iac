//! A project the tests apply into: the simulator, a workspace, and the project and its
//! environment already in place (remotely and in state), so a test exercises the kind under the
//! environment and not the kinds above it.

use std::path::Path;
use std::sync::Arc;

use dokploy_core::Plan;
use dokploy_engine::{ApplyError, ApplySummary, Engine};
use dokploy_sim::Sim;
use dokploy_state::{
    DocumentId, ExpectedState, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceName, ResourceState, SensitiveInputs, StateFile, StateStore,
};
use serde_json::{Map, Value};

use super::{compile, engine, specs};

pub fn sim() -> Sim {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6");
    Sim::new(Arc::new(specs())).with_fixtures(&fixtures)
}

pub struct ProjectWorld {
    pub sim: Sim,
    pub directory: tempfile::TempDir,
}

impl ProjectWorld {
    pub fn new() -> Self {
        let world = Self::bare();
        world.seed_ancestors();
        world
    }

    /// A simulator with nothing in it and no state: the project is created by the apply.
    pub fn bare() -> Self {
        Self {
            sim: sim(),
            directory: tempfile::tempdir().unwrap(),
        }
    }

    /// The project and its environment exist, remotely and in state, as if an earlier apply had
    /// made them: these tests exercise the application's source, not the kinds above it.
    fn seed_ancestors(&self) {
        let engine = self.engine();
        let specs = specs();
        let mut resources = std::collections::BTreeMap::new();
        let mut parent: Option<(ResourceAddress, String)> = None;
        for (kind, key) in [("project", "shop"), ("environment", "production")] {
            let spec = specs.get(kind).expect("a spec");
            let mut remote = Map::new();
            let mut managed = Map::new();
            for (name, field) in &spec.fields {
                if field.default.as_deref() == Some("key") {
                    remote.insert(
                        field.request_name(name).to_owned(),
                        Value::String(key.to_owned()),
                    );
                    managed.insert(name.clone(), Value::String(key.to_owned()));
                }
            }
            if let Some((_, parent_id)) = &parent {
                let attach = spec
                    .api
                    .create
                    .as_ref()
                    .and_then(|op| op.parent_id_field(spec.parents.first().map(String::as_str)))
                    .expect("an environment attaches to its project");
                remote.insert(attach.to_owned(), Value::String(parent_id.clone()));
            }
            let id = self.sim.seed(kind, &Value::Object(remote));
            let resource_kind: ResourceKind = kind.parse().unwrap();
            let name = ResourceName::new(key).unwrap();
            let address = match &parent {
                Some((parent, _)) => parent.child(resource_kind, name).unwrap(),
                None => ResourceAddress::new(resource_kind, name),
            };
            let resource = ResourceState::try_new(
                resource_kind,
                RemoteId::new(id.clone()).unwrap(),
                false,
                ManagedInputs::try_from_json(Value::Object(managed)).unwrap(),
                SensitiveInputs::default(),
                parent.as_ref().map(|(parent, _)| parent.clone()),
                Vec::new(),
            )
            .unwrap();
            resources.insert(address.clone(), resource);
            parent = Some((address, id));
        }
        let state = StateFile::with_resources(
            semver::Version::new(0, 1, 0),
            engine.instance().clone(),
            DocumentId::Project(ResourceName::new("shop").unwrap()),
            resources,
        )
        .unwrap();
        self.store(&engine)
            .begin_write()
            .unwrap()
            .checkpoint(ExpectedState::absent(), &state)
            .unwrap();
    }

    pub fn engine(&self) -> Engine<&Sim> {
        engine(&self.sim)
    }

    pub fn store(&self, engine: &Engine<&Sim>) -> StateStore {
        StateStore::for_document(
            self.directory.path().canonicalize().unwrap(),
            engine.instance().clone(),
            DocumentId::Project(ResourceName::new("shop").unwrap()),
        )
        .unwrap()
    }

    pub async fn apply_text(
        &self,
        text: &str,
        environment: &[(&str, &str)],
    ) -> Result<ApplySummary, ApplyError> {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = compile(&engine, text, environment);
        engine.apply(&compiled, &store, |_| true).await
    }

    pub async fn plan_text(&self, text: &str, environment: &[(&str, &str)]) -> Plan {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = compile(&engine, text, environment);
        let state = store.inspect().unwrap();
        engine.plan(&compiled, state.as_ref()).await.unwrap()
    }
}
