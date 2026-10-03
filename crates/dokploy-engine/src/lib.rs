//! The spec-driven engine: compile a document, discover the remote, and plan.
//!
//! The engine contains no knowledge of any kind. Everything a kind means comes from its
//! spec (ADR 0002), the document comes from `dokploy-model`, the plan from the pure
//! planner in `dokploy-core`, and every byte sent to Dokploy goes through a
//! [`Transport`](dokploy_sdk::Transport), so the same code runs over HTTP or in memory.
//!
//! It compiles documents, plans kinds that have a collection read, and applies the plan:
//! create, update by write group, remove, replace, and rename, each journaled, never
//! retried, and read back. Recovery, nested kinds, and deploy follow.

mod apply;
mod canonical;
mod compile;
mod contract;
mod discover;
mod envtext;
mod fingerprint;
mod project;
mod recover;
mod secrets;
mod selectors;

use dokploy_core::{Plan, RemoteStateError, StoredState, StoredStateError};
use dokploy_model::Document;
use dokploy_sdk::Transport;
use dokploy_spec::{Scope, SpecRegistry};
use dokploy_state::{
    InstanceIdentity, InstanceIdentityError, KindRegistrationError, StateError, StateFile,
    StateScope,
};
use thiserror::Error;

pub use apply::{ApplyError, ApplySummary};
pub use compile::{AddressError, CompileError, Compiled, DirectiveError};
pub use contract::required_at_creation;
pub use fingerprint::{FingerprintKey, FingerprintKeyError, Fingerprinter};
pub use recover::{RecoverError, RecoveryAction, RecoveryPreview, RecoverySummary};
pub use secrets::{MAX_SOURCE_BYTES, SecretError, SecretReader, WorkspaceSecrets};

/// Anything that stops the engine from producing a plan.
#[derive(Debug, Error)]
pub enum EngineError {
    /// A spec kind could not be registered with the state layer.
    #[error(transparent)]
    Kinds(#[from] KindRegistrationError),
    /// The transport's base URL is not a usable instance identity.
    #[error(transparent)]
    Instance(#[from] InstanceIdentityError),
    /// A kind in the document or state has no spec.
    #[error("kind `{kind}` has no spec")]
    UnknownKind { kind: String },
    /// The engine cannot read this kind from Dokploy yet.
    #[error("cannot discover kind `{kind}` yet: {reason}")]
    UnsupportedDiscovery { kind: String, reason: &'static str },
    /// The state belongs to a different document or instance.
    #[error(transparent)]
    State(#[from] StateError),
    /// The state does not agree with the specs.
    #[error(transparent)]
    Stored(#[from] StoredStateError),
    /// The state tracks a different document than the one being planned.
    #[error("the state tracks a {state} document, but a {document} document is being planned")]
    WrongDocument {
        state: StateScope,
        document: StateScope,
    },
    /// The remote snapshot could not be assembled.
    #[error(transparent)]
    Remote(RemoteStateError),
    /// A `moves` or `removed` entry does not name exactly one address.
    #[error(transparent)]
    Directive(#[from] DirectiveError),
}

/// Compiles documents and plans them against a Dokploy reached through a transport.
pub struct Engine<T> {
    specs: SpecRegistry,
    transport: T,
    instance: InstanceIdentity,
}

impl<T: Transport> Engine<T> {
    /// Binds the specs to a transport. Every spec kind is registered with the state layer.
    pub fn new(specs: SpecRegistry, transport: T) -> Result<Self, EngineError> {
        dokploy_core::register_spec_kinds(&specs)?;
        let instance = InstanceIdentity::parse(transport.base_url().as_str())?;

        Ok(Self {
            specs,
            transport,
            instance,
        })
    }

    /// The kind specs this engine runs.
    #[must_use]
    pub fn specs(&self) -> &SpecRegistry {
        &self.specs
    }

    /// The instance this engine talks to.
    #[must_use]
    pub fn instance(&self) -> &InstanceIdentity {
        &self.instance
    }

    /// A fingerprinter for this engine's instance.
    #[must_use]
    pub fn fingerprinter(&self, key: FingerprintKey) -> Fingerprinter {
        Fingerprinter::new(self.instance.clone(), key)
    }

    /// Parses a document against this engine's specs.
    pub fn parse(&self, text: &str) -> Result<Document, dokploy_model::Diagnostics> {
        Document::parse(text, &self.specs)
    }

    /// Compiles a document into the planner's desired state.
    ///
    /// Secret sources are read through `secrets` and fingerprinted. The planner only ever sees
    /// the receipts; the values stay in zeroizing memory inside the compiled document, only so
    /// an apply can send them, and are never printed or written.
    pub fn compile(
        &self,
        document: &Document,
        fingerprinter: &Fingerprinter,
        secrets: &dyn SecretReader,
    ) -> Result<Compiled, CompileError> {
        compile::Compiler {
            specs: &self.specs,
            fingerprinter,
            secrets,
        }
        .compile(document)
    }

    /// A document with nothing declared, to destroy what its state tracks. No secret is read.
    pub fn compile_empty(&self, document: &Document) -> Result<Compiled, CompileError> {
        Compiled::empty_for(document)
    }

    /// Plans a compiled document against fresh remote state.
    ///
    /// `state` is the document's durable state, or `None` before the first apply. Nothing is
    /// written to Dokploy; the plan is deterministic and shows paths, never values.
    pub async fn plan(
        &self,
        compiled: &Compiled,
        state: Option<&StateFile>,
    ) -> Result<Plan, EngineError> {
        let stored = match state {
            Some(state) => {
                state.ensure_instance(&self.instance)?;
                let document = match compiled.scope() {
                    Scope::Project => StateScope::Project,
                    Scope::Settings => StateScope::Settings,
                };
                if state.scope() != document {
                    return Err(EngineError::WrongDocument {
                        state: state.scope(),
                        document,
                    });
                }
                StoredState::try_from_state(state, &self.specs)?
            }
            None => StoredState::absent(self.instance.clone()),
        };
        let remote = discover::discover(
            &self.transport,
            &self.specs,
            &self.instance,
            compiled,
            state,
        )
        .await?;

        let desired = compiled.desired_for(state)?;
        let desired = desired.with_cleared_properties(self.dropped_set_members(compiled, state));

        Ok(dokploy_core::plan(&desired, &stored, &remote))
    }

    /// The members of a set of selectors that state last applied and the document, which now
    /// declares the set, no longer names: they are cleared, so they are detached.
    fn dropped_set_members(
        &self,
        compiled: &Compiled,
        state: Option<&StateFile>,
    ) -> std::collections::BTreeMap<dokploy_state::ResourceAddress, Vec<dokploy_core::PropertyPath>>
    {
        let mut cleared = std::collections::BTreeMap::new();
        let Some(state) = state else {
            return cleared;
        };
        for (address, resource) in &compiled.resources {
            if resource.declared_sets.is_empty() {
                continue;
            }
            let (Some(spec), Some(stored), Some(desired)) = (
                self.specs.get(&resource.spec_kind),
                state.resource(address),
                compiled.desired.resources().get(address),
            ) else {
                continue;
            };
            let Some(applied) = stored.last_applied().as_json().as_object() else {
                continue;
            };
            let mut dropped = Vec::new();
            for field in &resource.declared_sets {
                let Some(members) = applied.get(field).and_then(serde_json::Value::as_object)
                else {
                    continue;
                };
                for (member, value) in members {
                    // A member already cleared has nothing left to clear.
                    if value.is_null() {
                        continue;
                    }
                    let Ok(path) =
                        dokploy_core::PropertyPath::from_spec(spec, &format!("{field}.{member}"))
                    else {
                        continue;
                    };
                    if !desired.properties().contains_key(&path) {
                        dropped.push(path);
                    }
                }
            }
            if !dropped.is_empty() {
                cleared.insert(address.clone(), dropped);
            }
        }

        cleared
    }
}
