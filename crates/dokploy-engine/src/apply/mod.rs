//! Apply: execute a plan, one journaled step at a time (ADR 0008).
//!
//! For each change the executor writes the journal's intent, sends one request per write
//! group, learns the identity of a created resource, checkpoints state, and re-reads the
//! resource to check that what it wrote reads back. A mutation is sent once and never
//! retried: when the transport cannot say whether it happened, the step stays open and
//! recovery decides from fresh evidence (invariant 16).

mod request;
mod run;

use dokploy_core::{ChangeKind, Plan};
use dokploy_sdk::Transport;
use dokploy_state::{
    ExpectedState, FailureCode, JournalError, OperationJournal, PlanDigest, ResourceAddress,
    StateError, StateFile, StateStore, StateStoreError,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::compile::Compiled;
use crate::{Engine, EngineError};

pub use run::ApplySummary;

/// Why an apply stopped. Messages never contain a value.
#[derive(Debug, Error)]
pub enum ApplyError {
    /// The plan could not be produced.
    #[error(transparent)]
    Plan(#[from] EngineError),
    /// The plan is not complete or has a blocking diagnostic; nothing was changed.
    #[error("the plan cannot be applied: {}; nothing was changed", diagnostics.join(", "))]
    Blocked {
        /// Each blocking diagnostic as `CODE` or `CODE at <address>`; never a value.
        diagnostics: Vec<String>,
    },
    /// The caller declined the plan; nothing was changed.
    #[error("the plan was declined; nothing was changed")]
    Declined,
    /// The engine cannot execute this part of the plan yet; nothing was changed.
    #[error("{}{}: {reason}", address, property.as_ref().map(|p| format!(" `{p}`")).unwrap_or_default())]
    Unsupported {
        address: ResourceAddress,
        property: Option<String>,
        reason: &'static str,
    },
    /// The create operation requires a field the document does not provide.
    #[error("{address}: `{field}` is required to create it")]
    MissingRequired {
        address: ResourceAddress,
        field: String,
    },
    /// A secret the document names was not compiled.
    #[error("{address}: no value was read for secret `{property}`")]
    MissingSecret {
        address: ResourceAddress,
        property: String,
    },
    /// A secret is not text.
    #[error("{address}: secret `{property}` is not text")]
    SecretNotText {
        address: ResourceAddress,
        property: String,
    },
    /// The update replaces the whole group, so a secret in it must be declared in the
    /// document.
    #[error("{address}: updating this replaces `{property}`, so its source must be declared")]
    NeedsSecret {
        address: ResourceAddress,
        property: String,
    },
    /// The plan's checkpoint does not fit the resource.
    #[error("{address}: the planned checkpoint does not fit the resource")]
    InvalidCheckpoint { address: ResourceAddress },
    /// A read the step needs failed; nothing was changed by this step.
    #[error("{address}: a read before the change failed ({code:?})")]
    Preparation {
        address: ResourceAddress,
        code: FailureCode,
    },
    /// Dokploy rejected the request. The step is recorded as failed.
    #[error("{address}: Dokploy rejected the change ({code:?})")]
    Rejected {
        address: ResourceAddress,
        code: FailureCode,
    },
    /// Whether the change happened is not known. The step stays open: run recovery.
    #[error("{address}: the outcome of the change is unknown; recover before applying again")]
    OutcomeUnknown { address: ResourceAddress },
    /// Dokploy acknowledged a removal but the resource can still be read. The step is recorded
    /// as failed and the resource stays tracked.
    #[error("{address}: Dokploy acknowledged the removal, but the resource is still there")]
    NotRemoved { address: ResourceAddress },
    /// The change was made and recorded, but it does not read back as written.
    #[error(
        "{address}: `{property}` does not read back as written; the next plan shows the difference"
    )]
    Verification {
        address: ResourceAddress,
        property: String,
    },
    /// The durable state or journal failed.
    #[error(transparent)]
    Journal(#[from] JournalError),
    /// The state store failed.
    #[error(transparent)]
    Store(#[from] StateStoreError),
    /// A state transition was refused.
    #[error(transparent)]
    State(#[from] StateError),
    /// The state does not agree with the specs.
    #[error(transparent)]
    Stored(#[from] dokploy_core::StoredStateError),
}

impl<T: Transport> Engine<T> {
    /// Plans the compiled document against fresh remote state and applies the plan.
    ///
    /// The writer lock is taken first, so the plan is made from the state that will be
    /// changed. `approve` sees the plan and may decline it; nothing is changed before it
    /// returns `true`. A plan that is incomplete or has a blocking diagnostic is never
    /// applied.
    pub async fn apply(
        &self,
        compiled: &Compiled,
        store: &StateStore,
        approve: impl FnOnce(&Plan) -> bool,
    ) -> Result<ApplySummary, ApplyError> {
        let mut session = store.begin_write()?;
        let durable = store.inspect()?;
        let plan = self.plan(compiled, durable.as_ref()).await?;

        if !approve(&plan) {
            return Err(ApplyError::Declined);
        }
        if !plan.complete() || !plan.applyable() {
            return Err(ApplyError::Blocked {
                diagnostics: plan
                    .diagnostics()
                    .iter()
                    .map(|diagnostic| {
                        let mut text = diagnostic.code().as_str().to_owned();
                        if let Some(address) = diagnostic.address() {
                            text.push_str(&format!(" at {address}"));
                        }
                        if let Some(failure) = diagnostic.remote_failure() {
                            text.push_str(&format!(" ({failure:?})"));
                        }
                        text
                    })
                    .collect(),
            });
        }
        self.preflight(compiled, &plan)?;
        if plan.changes().is_empty() {
            return Ok(ApplySummary { applied: 0 });
        }

        let state = match &durable {
            Some(state) => state.clone(),
            None => {
                let fresh = StateFile::new(
                    semver::Version::parse(env!("CARGO_PKG_VERSION"))
                        .expect("the crate version is semver"),
                    self.instance.clone(),
                    compiled.document.clone(),
                );
                session.checkpoint(ExpectedState::absent(), &fresh)?;
                fresh
            }
        };
        let journal = OperationJournal::begin(&mut session, plan_digest(&plan))?;

        run::run(self, compiled, &plan, state, journal).await
    }

    /// Checks, before anything is sent, that the engine can execute every change.
    fn preflight(&self, compiled: &Compiled, plan: &Plan) -> Result<(), ApplyError> {
        for change in plan.changes() {
            let address = change.address();
            let spec = self.specs.get(address.kind().as_str()).ok_or_else(|| {
                EngineError::UnknownKind {
                    kind: address.kind().as_str().to_owned(),
                }
            })?;
            match change.kind() {
                ChangeKind::Create | ChangeKind::Replace => {
                    let checkpoint = change.checkpoint().present().ok_or_else(|| {
                        ApplyError::InvalidCheckpoint {
                            address: address.clone(),
                        }
                    })?;
                    request::create_request(
                        spec,
                        address,
                        checkpoint,
                        Some("preflight"),
                        compiled,
                    )?;
                    if change.kind() == ChangeKind::Replace {
                        request::remove_request(spec, address, "preflight")?;
                        if change.replacement_order()
                            != Some(dokploy_core::ReplacementOrder::DeleteBeforeCreate)
                        {
                            return Err(ApplyError::Unsupported {
                                address: address.clone(),
                                property: None,
                                reason: "is replaced create-before-delete, which the executor does not do yet",
                            });
                        }
                    }
                }
                ChangeKind::Update => {
                    let written = run::written(change);
                    request::groups(spec, address, &written)?;
                }
                ChangeKind::Delete => {
                    request::remove_request(spec, address, "preflight")?;
                }
                ChangeKind::NoOp | ChangeKind::Forget => {}
                ChangeKind::Move => {
                    if change.move_action() != Some(dokploy_core::MoveAction::StateOnly) {
                        return Err(ApplyError::Unsupported {
                            address: address.clone(),
                            property: None,
                            reason: "is renamed and changed in one step, which the executor does not do yet",
                        });
                    }
                }
            }
        }

        Ok(())
    }
}

fn plan_digest(plan: &Plan) -> PlanDigest {
    use std::fmt::Write as _;
    let digest = Sha256::digest(plan.to_json_bytes());
    let mut text = String::with_capacity(64);
    for byte in digest {
        let _ = write!(text, "{byte:02x}");
    }

    PlanDigest::parse(text).expect("a SHA-256 digest is canonical lowercase hexadecimal")
}
