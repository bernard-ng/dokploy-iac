//! Safe reconciliation of interrupted journaled operations.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dokploy_core::{
    ConfigDigest, RemoteObservation, ResourceObservationMatch, compare_resource_observation,
};
use dokploy_sdk::Dokploy;
use dokploy_state::{
    JournalAction, RecoveryStep, RecoveryStepOutcome, RemoteId, ResourceAddress, StateFile,
    StateStore,
};
use thiserror::Error;

use crate::desired::{CompileDesiredError, compile_desired_for_instance};
use crate::remote::DiscoverRemoteError;

/// Value-free action that recovery can prove safe from durable and fresh evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    /// Close an operation that stopped outside a mutation step.
    ResolveOperation,
    /// Persist a success that was recorded before the prior process stopped.
    CheckpointRecordedSuccess,
    /// Adopt a uniquely observed resource created by the interrupted operation.
    AdoptCreatedResource,
    /// Persist a confirmed update, deployment, deletion, or state-only forget.
    CheckpointConfirmedSuccess,
    /// Record that the interrupted remote mutation made no change.
    ConfirmNoChange,
}

/// Redaction-safe preview presented before recovery changes durable state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryPreview {
    address: Option<ResourceAddress>,
    action: RecoveryAction,
}

impl RecoveryPreview {
    /// Returns the affected logical address, when recovery concerns one step.
    #[must_use]
    pub const fn address(&self) -> Option<&ResourceAddress> {
        self.address.as_ref()
    }

    /// Returns the value-free recovery action.
    #[must_use]
    pub const fn action(&self) -> RecoveryAction {
        self.action
    }
}

/// Outcome of a completed recovery operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryResult {
    recovered_steps: usize,
}

impl RecoveryResult {
    /// Returns the number of interrupted steps resolved by this invocation.
    #[must_use]
    pub const fn recovered_steps(self) -> usize {
        self.recovered_steps
    }
}

enum RecoveryDecision {
    ResolveOnly,
    CheckpointRecorded {
        sequence: u64,
    },
    CheckpointUncertain {
        sequence: u64,
        remote_id: Option<RemoteId>,
        action: RecoveryAction,
    },
    ConfirmNoChange {
        sequence: u64,
    },
}

impl RecoveryDecision {
    fn preview(&self, step: Option<&RecoveryStep>) -> RecoveryPreview {
        let action = match self {
            Self::ResolveOnly => RecoveryAction::ResolveOperation,
            Self::CheckpointRecorded { .. } => RecoveryAction::CheckpointRecordedSuccess,
            Self::CheckpointUncertain { action, .. } => *action,
            Self::ConfirmNoChange { .. } => RecoveryAction::ConfirmNoChange,
        };

        RecoveryPreview {
            address: step.map(|step| step.address().clone()),
            action,
        }
    }
}

/// Verifies fresh remote evidence and resolves one interrupted operation.
pub async fn recover_workspace_with_approval(
    client: &Dokploy,
    config_file: &Path,
    approval: impl FnOnce(&RecoveryPreview) -> io::Result<bool>,
) -> Result<RecoveryResult, RecoverWorkspaceError> {
    let workspace = canonical_workspace(config_file)?;
    let instance = dokploy_state::InstanceIdentity::parse(client.base_url().as_str())?;
    let scope = dokploy_config::peek_scope(config_file)?;
    let store = StateStore::with_scope(&workspace, instance.clone(), scope)?;
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

    let decision = match unresolved.as_ref() {
        None => RecoveryDecision::ResolveOnly,
        Some(step) => {
            decision_for_step(client, config_file, &workspace, &instance, &recovery, step).await?
        }
    };
    let preview = decision.preview(unresolved.as_ref());
    if !approval(&preview).map_err(|source| RecoverWorkspaceError::Approval { source })? {
        return Err(RecoverWorkspaceError::Declined);
    }

    match decision {
        RecoveryDecision::ResolveOnly => {}
        RecoveryDecision::CheckpointRecorded { sequence } => {
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
        RecoveryDecision::CheckpointUncertain {
            sequence,
            remote_id,
            ..
        } => {
            let proposed = recovery.expected_proposed_state(sequence, remote_id.clone())?;
            recovery.checkpoint_uncertain_success(sequence, remote_id, &proposed)?;
        }
        RecoveryDecision::ConfirmNoChange { sequence } => {
            recovery.confirm_no_change(sequence)?;
        }
    }
    let recovered_steps = usize::from(unresolved.is_some());
    recovery.resolve()?;

    Ok(RecoveryResult { recovered_steps })
}

async fn decision_for_step(
    client: &Dokploy,
    config_file: &Path,
    workspace: &Path,
    instance: &dokploy_state::InstanceIdentity,
    recovery: &dokploy_state::RecoverySession<'_>,
    step: &RecoveryStep,
) -> Result<RecoveryDecision, RecoverWorkspaceError> {
    if let RecoveryStepOutcome::Succeeded {
        checkpointed: false,
        ..
    } = step.outcome()
    {
        return Ok(RecoveryDecision::CheckpointRecorded {
            sequence: step.sequence(),
        });
    }
    if step.outcome() != &RecoveryStepOutcome::InProgress {
        return Err(RecoverWorkspaceError::UnsafeEvidence);
    }
    if matches!(step.action(), JournalAction::Forget | JournalAction::Move) {
        return Ok(RecoveryDecision::CheckpointUncertain {
            sequence: step.sequence(),
            remote_id: None,
            action: RecoveryAction::CheckpointConfirmedSuccess,
        });
    }

    let loaded = dokploy_config::load_with_digest(config_file)?;
    let source_digest = ConfigDigest::parse(hex_digest(loaded.source_sha256))
        .expect("a SHA-256 digest is canonical lowercase hexadecimal");
    let compiled =
        compile_desired_for_instance(&loaded.config, source_digest, instance.clone(), workspace)?;
    let remote = crate::scope::discover(client, &compiled, recovery.current_state()).await?;
    // A selector that no longer names exactly one record cannot prove which external
    // record an uncertain step selected, so the operator must decide.
    if compiled
        .bindings()
        .external_selectors()
        .filter(|(address, _, _, _)| *address == step.address())
        .any(|(address, path, _, _)| {
            remote
                .external_resolution(address, path)
                .is_some_and(|resolution| !resolution.is_resolved())
        })
    {
        return Err(RecoverWorkspaceError::ManualIntervention);
    }
    let observation = remote
        .observation(step.address())
        .ok_or(RecoverWorkspaceError::ManualIntervention)?;

    match step.action() {
        JournalAction::Create => match observation {
            RemoteObservation::Missing => Ok(RecoveryDecision::ConfirmNoChange {
                sequence: step.sequence(),
            }),
            RemoteObservation::Present(resource) => {
                let remote_id = resource.remote_id().clone();
                let proposed =
                    recovery.expected_proposed_state(step.sequence(), Some(remote_id.clone()))?;
                match compare_resource_observation(&proposed, step.address(), observation)? {
                    ResourceObservationMatch::Exact
                    | ResourceObservationMatch::ExactExceptSensitive => {
                        Ok(RecoveryDecision::CheckpointUncertain {
                            sequence: step.sequence(),
                            remote_id: Some(remote_id),
                            action: RecoveryAction::AdoptCreatedResource,
                        })
                    }
                    ResourceObservationMatch::Different | ResourceObservationMatch::Unavailable => {
                        Err(RecoverWorkspaceError::ManualIntervention)
                    }
                }
            }
            RemoteObservation::Unavailable(_) => Err(RecoverWorkspaceError::ManualIntervention),
        },
        JournalAction::Delete => match observation {
            RemoteObservation::Missing => Ok(RecoveryDecision::CheckpointUncertain {
                sequence: step.sequence(),
                remote_id: None,
                action: RecoveryAction::CheckpointConfirmedSuccess,
            }),
            RemoteObservation::Present(resource)
                if recovery
                    .current_state()
                    .resource(step.address())
                    .is_some_and(|stored| stored.remote_id() == resource.remote_id()) =>
            {
                Ok(RecoveryDecision::ConfirmNoChange {
                    sequence: step.sequence(),
                })
            }
            RemoteObservation::Present(_) | RemoteObservation::Unavailable(_) => {
                Err(RecoverWorkspaceError::ManualIntervention)
            }
        },
        JournalAction::Update | JournalAction::Deploy => {
            let proposed = recovery.expected_proposed_state(step.sequence(), None)?;
            match compare_resource_observation(&proposed, step.address(), observation)? {
                ResourceObservationMatch::Exact => Ok(RecoveryDecision::CheckpointUncertain {
                    sequence: step.sequence(),
                    remote_id: None,
                    action: RecoveryAction::CheckpointConfirmedSuccess,
                }),
                ResourceObservationMatch::Different => match compare_resource_observation(
                    recovery.current_state(),
                    step.address(),
                    observation,
                )? {
                    ResourceObservationMatch::Exact => Ok(RecoveryDecision::ConfirmNoChange {
                        sequence: step.sequence(),
                    }),
                    ResourceObservationMatch::ExactExceptSensitive
                    | ResourceObservationMatch::Different
                    | ResourceObservationMatch::Unavailable => {
                        Err(RecoverWorkspaceError::ManualIntervention)
                    }
                },
                ResourceObservationMatch::ExactExceptSensitive
                    if exact_except_sensitive_confirms_update(
                        step.action(),
                        recovery.current_state(),
                        &proposed,
                        step.address(),
                    ) =>
                {
                    Ok(RecoveryDecision::CheckpointUncertain {
                        sequence: step.sequence(),
                        remote_id: None,
                        action: RecoveryAction::CheckpointConfirmedSuccess,
                    })
                }
                ResourceObservationMatch::ExactExceptSensitive
                | ResourceObservationMatch::Unavailable => {
                    Err(RecoverWorkspaceError::ManualIntervention)
                }
            }
        }
        JournalAction::Forget | JournalAction::Move => {
            unreachable!("state-only action returned before discovery")
        }
    }
}

fn exact_except_sensitive_confirms_update(
    action: JournalAction,
    current: &StateFile,
    proposed: &StateFile,
    address: &ResourceAddress,
) -> bool {
    if action != JournalAction::Update {
        return false;
    }

    let Some(before) = current.resource(address) else {
        return false;
    };
    let Some(after) = proposed.resource(address) else {
        return false;
    };

    before.sensitive_inputs() == after.sensitive_inputs()
}

fn canonical_workspace(config_file: &Path) -> Result<PathBuf, RecoverWorkspaceError> {
    let parent = config_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::canonicalize(parent).map_err(|source| RecoverWorkspaceError::Workspace { source })
}

fn hex_digest(bytes: [u8; 32]) -> String {
    use std::fmt::Write;

    bytes
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a string cannot fail");
            hex
        })
}

/// Redaction-safe failure while reconciling an interrupted operation.
#[derive(Debug, Error)]
pub enum RecoverWorkspaceError {
    #[error("failed to resolve the configuration workspace")]
    Workspace {
        #[source]
        source: io::Error,
    },
    #[error("the Dokploy client URL is not a valid instance identity")]
    Instance(#[from] dokploy_state::InstanceIdentityError),
    #[error("failed to inspect durable recovery state")]
    State(#[from] dokploy_state::StateStoreError),
    #[error("failed to reconcile the interrupted journal")]
    Recovery(#[from] dokploy_state::RecoveryError),
    #[error("failed to load the declarative configuration")]
    Config(#[from] dokploy_config::ConfigFileError),
    #[error("desired configuration cannot be compiled safely")]
    Desired(#[from] CompileDesiredError),
    #[error("fresh Dokploy state cannot be discovered safely")]
    Remote(#[from] DiscoverRemoteError),
    #[error("durable state cannot be projected safely")]
    StoredState(#[from] dokploy_core::StoredStateError),
    #[error("recovery evidence is inconsistent with the interrupted step")]
    UnsafeEvidence,
    #[error("recovery requires a manual decision because the remote outcome is not provable")]
    ManualIntervention,
    #[error("failed to read recovery approval")]
    Approval {
        #[source]
        source: io::Error,
    },
    #[error("recovery was declined")]
    Declined,
}
