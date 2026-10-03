//! Recovery: settling an interrupted apply from fresh evidence (ADR 0008, invariant 16).
//!
//! An apply that stopped between sending a mutation and recording its result leaves one step
//! open. Recovery never guesses and never repeats the mutation: it reads the resource again,
//! compares it with the state the step was meant to produce, and either records the success,
//! records that nothing changed, or says that a person must decide.

use dokploy_core::{
    RemoteObservation, ResourceObservationMatch, compare_resource_observation_with_specs,
};
use dokploy_sdk::Transport;
use dokploy_state::{
    JournalAction, RecoveryError, RecoveryStep, RecoveryStepOutcome, RemoteId, ResourceAddress,
    StateFile, StateStore, StateStoreError,
};
use thiserror::Error;

use crate::compile::Compiled;
use crate::{Engine, EngineError};

/// What recovery can prove safe. Value-free.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    /// Close an operation that stopped outside a mutation step.
    ResolveOperation,
    /// Persist a success that was recorded before the process stopped.
    CheckpointRecordedSuccess,
    /// Adopt a uniquely observed resource created by the interrupted operation.
    AdoptCreatedResource,
    /// Persist an update, deletion, rename, or state-only forget that is now confirmed.
    CheckpointConfirmedSuccess,
    /// Record that the interrupted mutation changed nothing.
    ConfirmNoChange,
}

/// The decision shown to the caller before durable state changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryPreview {
    address: Option<ResourceAddress>,
    action: RecoveryAction,
}

impl RecoveryPreview {
    /// The resource the interrupted step concerned, when there is one.
    #[must_use]
    pub const fn address(&self) -> Option<&ResourceAddress> {
        self.address.as_ref()
    }

    /// What recovery will do.
    #[must_use]
    pub const fn action(&self) -> RecoveryAction {
        self.action
    }
}

/// What recovery did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoverySummary {
    recovered_steps: usize,
}

impl RecoverySummary {
    /// How many interrupted steps were settled.
    #[must_use]
    pub const fn recovered_steps(self) -> usize {
        self.recovered_steps
    }
}

/// Why recovery stopped. Messages never contain a value.
#[derive(Debug, Error)]
pub enum RecoverError {
    /// Fresh evidence could not be gathered.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// The state store failed.
    #[error(transparent)]
    Store(#[from] StateStoreError),
    /// The journal could not be settled.
    #[error(transparent)]
    Recovery(#[from] RecoveryError),
    /// The state does not agree with the specs.
    #[error(transparent)]
    Stored(#[from] dokploy_core::StoredStateError),
    /// The journal says something recovery cannot reconcile.
    #[error("the recovery evidence is inconsistent with the interrupted step")]
    UnsafeEvidence,
    /// Fresh evidence does not prove whether the change happened.
    #[error("the remote outcome is not provable; a person has to decide")]
    ManualIntervention,
    /// The caller declined.
    #[error("recovery was declined")]
    Declined,
}

enum Decision {
    ResolveOnly,
    CheckpointRecorded {
        sequence: u64,
    },
    Checkpoint {
        sequence: u64,
        remote_id: Option<RemoteId>,
        action: RecoveryAction,
    },
    NoChange {
        sequence: u64,
    },
}

impl<T: Transport> Engine<T> {
    /// Settles the one interrupted apply of the document's state.
    ///
    /// `compiled` must be the document that was being applied: it says how to find the
    /// resource again. `approve` sees what recovery proved before anything is recorded.
    pub async fn recover(
        &self,
        compiled: &Compiled,
        store: &StateStore,
        approve: impl FnOnce(&RecoveryPreview) -> bool,
    ) -> Result<RecoverySummary, RecoverError> {
        let mut recovery = store.begin_recovery()?;
        let unresolved = recovery
            .evidence()
            .steps()
            .iter()
            .find(|step| {
                matches!(
                    step.outcome(),
                    RecoveryStepOutcome::InProgress
                        | RecoveryStepOutcome::Succeeded {
                            checkpointed: false,
                            ..
                        }
                )
            })
            .cloned();

        let decision = match &unresolved {
            None => Decision::ResolveOnly,
            Some(step) => self.decide(compiled, &recovery, step).await?,
        };
        let preview = RecoveryPreview {
            address: unresolved.as_ref().map(|step| step.address().clone()),
            action: match &decision {
                Decision::ResolveOnly => RecoveryAction::ResolveOperation,
                Decision::CheckpointRecorded { .. } => RecoveryAction::CheckpointRecordedSuccess,
                Decision::Checkpoint { action, .. } => *action,
                Decision::NoChange { .. } => RecoveryAction::ConfirmNoChange,
            },
        };
        if !approve(&preview) {
            return Err(RecoverError::Declined);
        }

        match decision {
            Decision::ResolveOnly => {}
            Decision::CheckpointRecorded { sequence } => {
                let remote_id = recovery
                    .evidence()
                    .steps()
                    .iter()
                    .find(|step| step.sequence() == sequence)
                    .and_then(|step| step.outcome().remote_id())
                    .cloned();
                let proposed = recovery.expected_proposed_state(sequence, remote_id)?;
                recovery.checkpoint_recorded_success(sequence, &proposed)?;
            }
            Decision::Checkpoint {
                sequence,
                remote_id,
                ..
            } => {
                let proposed = recovery.expected_proposed_state(sequence, remote_id.clone())?;
                recovery.checkpoint_uncertain_success(sequence, remote_id, &proposed)?;
            }
            Decision::NoChange { sequence } => recovery.confirm_no_change(sequence)?,
        }
        recovery.resolve()?;

        Ok(RecoverySummary {
            recovered_steps: usize::from(unresolved.is_some()),
        })
    }

    async fn decide(
        &self,
        compiled: &Compiled,
        recovery: &dokploy_state::RecoverySession<'_>,
        step: &RecoveryStep,
    ) -> Result<Decision, RecoverError> {
        let sequence = step.sequence();
        if matches!(
            step.outcome(),
            RecoveryStepOutcome::Succeeded {
                checkpointed: false,
                ..
            }
        ) {
            return Ok(Decision::CheckpointRecorded { sequence });
        }
        if step.outcome() != &RecoveryStepOutcome::InProgress {
            return Err(RecoverError::UnsafeEvidence);
        }
        // A state-only step has nothing to observe: it can only have happened locally.
        if matches!(step.action(), JournalAction::Forget | JournalAction::Move) {
            return Ok(Decision::Checkpoint {
                sequence,
                remote_id: None,
                action: RecoveryAction::CheckpointConfirmedSuccess,
            });
        }

        let current: &StateFile = recovery.current_state();
        let remote = crate::discover::discover(
            &self.transport,
            &self.specs,
            &self.instance,
            compiled,
            Some(current),
        )
        .await?;
        let observation = remote
            .observation(step.address())
            .ok_or(RecoverError::ManualIntervention)?;
        let compare = |state: &StateFile| {
            compare_resource_observation_with_specs(state, step.address(), observation, &self.specs)
        };

        match step.action() {
            JournalAction::Create => match observation {
                RemoteObservation::Missing => Ok(Decision::NoChange { sequence }),
                RemoteObservation::Present(resource) => {
                    let remote_id = resource.remote_id().clone();
                    let proposed =
                        recovery.expected_proposed_state(sequence, Some(remote_id.clone()))?;
                    match compare(&proposed)? {
                        ResourceObservationMatch::Exact
                        | ResourceObservationMatch::ExactExceptSensitive => {
                            Ok(Decision::Checkpoint {
                                sequence,
                                remote_id: Some(remote_id),
                                action: RecoveryAction::AdoptCreatedResource,
                            })
                        }
                        ResourceObservationMatch::Different
                        | ResourceObservationMatch::Unavailable => {
                            Err(RecoverError::ManualIntervention)
                        }
                    }
                }
                RemoteObservation::Unavailable(_) => Err(RecoverError::ManualIntervention),
            },
            JournalAction::Delete => match observation {
                RemoteObservation::Missing => Ok(Decision::Checkpoint {
                    sequence,
                    remote_id: None,
                    action: RecoveryAction::CheckpointConfirmedSuccess,
                }),
                RemoteObservation::Present(resource)
                    if current
                        .resource(step.address())
                        .is_some_and(|stored| stored.remote_id() == resource.remote_id()) =>
                {
                    Ok(Decision::NoChange { sequence })
                }
                RemoteObservation::Present(_) | RemoteObservation::Unavailable(_) => {
                    Err(RecoverError::ManualIntervention)
                }
            },
            JournalAction::Update | JournalAction::Deploy => {
                let proposed = recovery.expected_proposed_state(sequence, None)?;
                match compare(&proposed)? {
                    ResourceObservationMatch::Exact => Ok(Decision::Checkpoint {
                        sequence,
                        remote_id: None,
                        action: RecoveryAction::CheckpointConfirmedSuccess,
                    }),
                    ResourceObservationMatch::Different => match compare(current)? {
                        ResourceObservationMatch::Exact => Ok(Decision::NoChange { sequence }),
                        // The step writes no secret, so a secret that cannot be read back
                        // cannot have been changed by it either.
                        ResourceObservationMatch::ExactExceptSensitive
                            if secrets_unchanged(current, &proposed, step.address()) =>
                        {
                            Ok(Decision::NoChange { sequence })
                        }
                        _ => Err(RecoverError::ManualIntervention),
                    },
                    // A secret cannot be read back. When the update did not change any secret,
                    // everything that can be compared agrees, and that is proof enough.
                    ResourceObservationMatch::ExactExceptSensitive
                        if step.action() == JournalAction::Update
                            && secrets_unchanged(current, &proposed, step.address()) =>
                    {
                        Ok(Decision::Checkpoint {
                            sequence,
                            remote_id: None,
                            action: RecoveryAction::CheckpointConfirmedSuccess,
                        })
                    }
                    ResourceObservationMatch::ExactExceptSensitive
                    | ResourceObservationMatch::Unavailable => {
                        Err(RecoverError::ManualIntervention)
                    }
                }
            }
            JournalAction::Forget | JournalAction::Move => {
                unreachable!("state-only action returned before discovery")
            }
        }
    }
}

fn secrets_unchanged(current: &StateFile, proposed: &StateFile, address: &ResourceAddress) -> bool {
    match (current.resource(address), proposed.resource(address)) {
        (Some(before), Some(after)) => before.sensitive_inputs() == after.sensitive_inputs(),
        _ => false,
    }
}
