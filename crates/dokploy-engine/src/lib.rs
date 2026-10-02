//! The spec-driven engine: compile a document, discover the remote, and plan.
//!
//! The engine contains no knowledge of any kind. Everything a kind means comes from its
//! spec (ADR 0002), the document comes from `dokploy-model`, the plan from the pure
//! planner in `dokploy-core`, and every byte sent to Dokploy goes through a
//! [`Transport`](dokploy_sdk::Transport), so the same code runs over HTTP or in memory.
//!
//! This is the skeleton (milestone M1): it compiles documents and plans settings kinds that
//! have a flat collection read, offline against any transport. Applying, recovery, nested
//! kinds, and deploy follow in later milestones.

mod canonical;
mod compile;
mod discover;
mod fingerprint;
mod project;
mod secrets;

use dokploy_core::{Plan, RemoteStateError, StoredState, StoredStateError};
use dokploy_model::Document;
use dokploy_sdk::Transport;
use dokploy_spec::{Scope, SpecRegistry};
use dokploy_state::{
    InstanceIdentity, InstanceIdentityError, KindRegistrationError, StateError, StateFile,
    StateScope,
};
use thiserror::Error;

pub use compile::{AddressError, CompileError, Compiled};
pub use fingerprint::{FingerprintKey, FingerprintKeyError, Fingerprinter};
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
    /// Secret sources are read through `secrets`, fingerprinted, and forgotten: the compiled
    /// result holds receipts, never values.
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
                StoredState::try_from_state_with_specs(state, &self.specs)?
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

        Ok(dokploy_core::plan(compiled.desired(), &stored, &remote))
    }
}
