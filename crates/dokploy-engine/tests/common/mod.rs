#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dokploy_core::Plan;
use dokploy_engine::{Compiled, Engine, FingerprintKey, WorkspaceSecrets};
use dokploy_sdk::{Error, OperationRequest, Transport};
use dokploy_spec::{SpecRegistry, load_dir};
use dokploy_state::{RemoteId, StateFile};
use semver::Version;
use serde_json::Value;
use url::Url;

pub const KEY: &str = "0199a0c8-2351-7c31-8899-2c8f81983ea5:0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";

pub fn specs() -> SpecRegistry {
    load_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs"))
        .expect("repository specs are valid")
}

type Handler = Box<dyn Fn(&OperationRequest) -> Result<Value, Error> + Send + Sync>;

/// An in-memory Dokploy: one handler per operation, and a log of every call. No socket.
#[derive(Clone)]
pub struct Canned {
    url: Url,
    handlers: Arc<Mutex<BTreeMap<String, Handler>>>,
    log: Arc<Mutex<Vec<OperationRequest>>>,
}

impl Canned {
    pub fn new() -> Self {
        Self {
            url: Url::parse("https://dokploy.sim.test/").unwrap(),
            handlers: Arc::default(),
            log: Arc::default(),
        }
    }

    pub fn on(
        self,
        operation: &str,
        handler: impl Fn(&OperationRequest) -> Result<Value, Error> + Send + Sync + 'static,
    ) -> Self {
        self.handlers
            .lock()
            .unwrap()
            .insert(operation.to_owned(), Box::new(handler));
        self
    }

    /// Always answers `operation` with `body`.
    pub fn serving(self, operation: &str, body: Value) -> Self {
        self.on(operation, move |_| Ok(body.clone()))
    }

    pub fn calls(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.operation().to_owned())
            .collect()
    }

    pub fn requests(&self) -> Vec<OperationRequest> {
        self.log.lock().unwrap().clone()
    }
}

impl Transport for Canned {
    fn base_url(&self) -> &Url {
        &self.url
    }

    async fn call(&self, request: OperationRequest) -> Result<Value, Error> {
        self.log.lock().unwrap().push(request.clone());
        let handlers = self.handlers.lock().unwrap();
        match handlers.get(request.operation()) {
            Some(handler) => handler(&request),
            None => Err(Error::UnexpectedResponse {
                operation: "unrouted",
            }),
        }
    }
}

pub fn engine<T: Transport>(transport: T) -> Engine<T> {
    Engine::new(specs(), transport).expect("the engine builds")
}

pub fn key() -> FingerprintKey {
    FingerprintKey::parse_explicit(KEY).expect("the key parses")
}

/// Compiles `text` with the given process environment.
pub fn compile<T: Transport>(
    engine: &Engine<T>,
    text: &str,
    environment: &[(&str, &str)],
) -> Compiled {
    let environment: BTreeMap<String, String> = environment
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    let document = engine
        .parse(text)
        .unwrap_or_else(|error| panic!("the document is valid:\n{error}"));
    let secrets = WorkspaceSecrets::new(std::env::temp_dir(), move |name: &str| {
        environment.get(name).cloned()
    });
    engine
        .compile(&document, &engine.fingerprinter(key()), &secrets)
        .unwrap_or_else(|error| panic!("the document compiles: {error}"))
}

/// Applies a plan's checkpoints to a state file, as an executor would after each success.
pub fn checkpointed(plan: &Plan, ids: &[(&str, &str)], mut state: StateFile) -> StateFile {
    for change in plan.changes() {
        let Some(checkpoint) = change.checkpoint().present() else {
            continue;
        };
        let id = ids
            .iter()
            .find(|(address, _)| *address == change.address().to_string())
            .map_or("remote-1", |(_, id)| id);
        let resource = checkpoint
            .materialize(change.address(), RemoteId::new(id).unwrap())
            .expect("the checkpoint materializes");
        state
            .upsert_resource(change.address().clone(), resource)
            .expect("state accepts the resource");
    }

    state
}

pub fn empty_settings_state(engine: &Engine<impl Transport>) -> StateFile {
    StateFile::new_for_document(
        Version::new(0, 1, 0),
        engine.instance().clone(),
        dokploy_state::DocumentId::Settings,
    )
}
