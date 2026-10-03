use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use uuid::Uuid;

use crate::storage::{harden_directory_permissions, harden_path_permissions, sync_directory};
use crate::strict_json::reject_duplicate_keys;
use crate::{
    ExpectedState, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    SensitiveInputs, StateFile, StateStore, StateStoreError, WriteSession,
};

const JOURNAL_DIRECTORY: &str = "journal";
const JOURNAL_ARCHIVE_DIRECTORY: &str = "archive";
const JOURNAL_FORMAT_VERSION: u32 = 2;

/// A mutation action recorded before and after a remote operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JournalAction {
    Create,
    Update,
    Delete,
    /// Remove ownership from state without making a remote request.
    Forget,
    /// Move one logical address without creating or deleting a remote resource.
    Move,
    Deploy,
}

/// A remote-identity-independent resource checkpoint persisted before mutation.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExpectedResource {
    kind: ResourceKind,
    protected: bool,
    last_applied: ManagedInputs,
    sensitive_inputs: SensitiveInputs,
    containment: Option<ResourceAddress>,
    dependencies: Vec<ResourceAddress>,
}

impl ExpectedResource {
    fn from_resource(resource: &ResourceState) -> Self {
        Self {
            kind: resource.kind(),
            protected: resource.is_protected(),
            last_applied: resource.last_applied().clone(),
            sensitive_inputs: resource.sensitive_inputs().clone(),
            containment: resource.containment().cloned(),
            dependencies: resource.dependencies().to_vec(),
        }
    }

    fn materialize(&self, remote_id: RemoteId) -> Result<ResourceState, ExpectedCheckpointError> {
        ResourceState::try_new(
            self.kind,
            remote_id,
            self.protected,
            self.last_applied.clone(),
            self.sensitive_inputs.clone(),
            self.containment.clone(),
            self.dependencies.clone(),
        )
        .map_err(|_| ExpectedCheckpointError)
    }
}

impl fmt::Debug for ExpectedResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExpectedResource")
            .field("kind", &self.kind)
            .field("protected", &self.protected)
            .field("last_applied", &"[REDACTED]")
            .field("sensitive_inputs", &"[REDACTED]")
            .field("containment", &self.containment)
            .field("dependency_count", &self.dependencies.len())
            .finish()
    }
}

/// Exact state precondition and target captured before one journaled action.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedCheckpoint {
    before: Option<ResourceState>,
    after: Option<ExpectedResource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    move_from: Option<ResourceAddress>,
}

impl ExpectedCheckpoint {
    /// Describes a create target without persisting its not-yet-known remote ID.
    pub fn create(target: ResourceState) -> Result<Self, ExpectedCheckpointError> {
        Ok(Self {
            before: None,
            after: Some(ExpectedResource::from_resource(&target)),
            move_from: None,
        })
    }

    /// Describes an identity-preserving update or deployment checkpoint.
    pub fn update(
        before: ResourceState,
        after: ResourceState,
    ) -> Result<Self, ExpectedCheckpointError> {
        if before.kind() != after.kind() || before.remote_id() != after.remote_id() {
            return Err(ExpectedCheckpointError);
        }

        Ok(Self {
            before: Some(before),
            after: Some(ExpectedResource::from_resource(&after)),
            move_from: None,
        })
    }

    /// Describes a remote deletion or state-only forget checkpoint.
    #[must_use]
    pub fn remove(before: ResourceState) -> Self {
        Self {
            before: Some(before),
            after: None,
            move_from: None,
        }
    }

    /// Describes an identity-preserving atomic logical-address move.
    pub fn move_resource(
        from: ResourceAddress,
        before: ResourceState,
        after: ResourceState,
    ) -> Result<Self, ExpectedCheckpointError> {
        if from.kind() != before.kind()
            || before.kind() != after.kind()
            || before.remote_id() != after.remote_id()
        {
            return Err(ExpectedCheckpointError);
        }

        Ok(Self {
            before: Some(before),
            after: Some(ExpectedResource::from_resource(&after)),
            move_from: Some(from),
        })
    }

    fn is_well_formed_for(&self, address: &ResourceAddress, action: JournalAction) -> bool {
        match action {
            JournalAction::Create => {
                self.before.is_none()
                    && self.move_from.is_none()
                    && self.after.as_ref().is_some_and(|after| {
                        after.kind == address.kind()
                            && after
                                .materialize(
                                    RemoteId::new("recovery-placeholder")
                                        .expect("static remote ID is valid"),
                                )
                                .is_ok()
                    })
            }
            JournalAction::Update | JournalAction::Deploy => {
                self.move_from.is_none()
                    && self.before.as_ref().is_some_and(|before| {
                        before.kind() == address.kind()
                            && self.after.as_ref().is_some_and(|after| {
                                after.kind == before.kind()
                                    && after.materialize(before.remote_id().clone()).is_ok()
                            })
                    })
            }
            JournalAction::Delete | JournalAction::Forget => {
                self.before
                    .as_ref()
                    .is_some_and(|before| before.kind() == address.kind())
                    && self.after.is_none()
                    && self.move_from.is_none()
            }
            JournalAction::Move => self.move_from.as_ref().is_some_and(|from| {
                from != address
                    && from.kind() == address.kind()
                    && self.before.as_ref().is_some_and(|before| {
                        before.kind() == address.kind()
                            && self.after.as_ref().is_some_and(|after| {
                                after.kind == before.kind()
                                    && after
                                        .materialize(before.remote_id().clone())
                                        .is_ok_and(|after| after.remote_id() == before.remote_id())
                            })
                    })
            }),
        }
    }

    fn validate_for(
        &self,
        address: &ResourceAddress,
        action: JournalAction,
        current: &StateFile,
    ) -> Result<(), ExpectedCheckpointError> {
        if !self.is_well_formed_for(address, action) {
            return Err(ExpectedCheckpointError);
        }
        let stored = current.resource(address);
        let valid = match action {
            JournalAction::Create => {
                self.before.is_none()
                    && stored.is_none()
                    && self
                        .after
                        .as_ref()
                        .is_some_and(|after| after.kind == address.kind())
            }
            JournalAction::Update | JournalAction::Deploy => {
                stored == self.before.as_ref()
                    && self.after.as_ref().is_some_and(|after| {
                        after.kind == address.kind()
                            && self.before.as_ref().is_some_and(|before| {
                                after.kind == before.kind()
                                    && after.materialize(before.remote_id().clone()).is_ok()
                            })
                    })
            }
            JournalAction::Delete | JournalAction::Forget => {
                stored == self.before.as_ref() && self.after.is_none()
            }
            JournalAction::Move => self.move_from.as_ref().is_some_and(|from| {
                current.resource(from) == self.before.as_ref() && stored.is_none()
            }),
        };
        if valid {
            Ok(())
        } else {
            Err(ExpectedCheckpointError)
        }
    }

    fn matches_proposed(
        &self,
        address: &ResourceAddress,
        action: JournalAction,
        proposed: &StateFile,
        remote_id: Option<&RemoteId>,
    ) -> bool {
        match action {
            JournalAction::Create => remote_id.is_some_and(|remote_id| {
                self.after.as_ref().is_some_and(|after| {
                    after.materialize(remote_id.clone()).ok().as_ref() == proposed.resource(address)
                })
            }),
            JournalAction::Update | JournalAction::Deploy => {
                self.before.as_ref().is_some_and(|before| {
                    self.after.as_ref().is_some_and(|after| {
                        after.materialize(before.remote_id().clone()).ok().as_ref()
                            == proposed.resource(address)
                    })
                })
            }
            JournalAction::Delete | JournalAction::Forget => proposed.resource(address).is_none(),
            JournalAction::Move => self.move_from.as_ref().is_some_and(|from| {
                proposed.resource(from).is_none()
                    && self.before.as_ref().is_some_and(|before| {
                        self.after.as_ref().is_some_and(|after| {
                            after.materialize(before.remote_id().clone()).ok().as_ref()
                                == proposed.resource(address)
                        })
                    })
            }),
        }
    }
}

impl fmt::Debug for ExpectedCheckpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExpectedCheckpoint")
            .field("before", &self.before.as_ref().map(|_| "[REDACTED]"))
            .field("after", &self.after)
            .field("move_from", &self.move_from)
            .finish()
    }
}

/// A checkpoint expectation that is inconsistent with its action.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("expected checkpoint does not match the journaled action")]
pub struct ExpectedCheckpointError;

/// A bounded, non-secret classification for a failed operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureCode {
    RemoteRejected,
    Unauthorized,
    Validation,
    Timeout,
    TransportOutcomeUnknown,
    DependencyFailed,
    Internal,
}

/// A canonical lowercase SHA-256 digest.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlanDigest(String);

impl PlanDigest {
    /// Validates a 64-character lowercase hexadecimal SHA-256 digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, PlanDigestError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(PlanDigestError);
        }

        Ok(Self(value))
    }

    /// Returns the canonical hexadecimal digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PlanDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("PlanDigest").field(&self.0).finish()
    }
}

impl Serialize for PlanDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PlanDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// A value that is not canonical SHA-256 hexadecimal text.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("plan digest must be 64 lowercase hexadecimal characters")]
pub struct PlanDigestError;

/// An opaque capability for resolving the currently open journal step.
#[derive(Clone, Eq, PartialEq)]
pub struct StepToken {
    operation_id: Uuid,
    sequence: u64,
    address: ResourceAddress,
    action: JournalAction,
    expected_checkpoint: Option<Box<ExpectedCheckpoint>>,
}

impl fmt::Debug for StepToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StepToken")
            .field("operation_id", &self.operation_id)
            .field("sequence", &self.sequence)
            .field("address", &self.address)
            .field("action", &self.action)
            .field(
                "expected_checkpoint",
                &self.expected_checkpoint.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// A journal coupled to the exclusive state writer that it checkpoints.
pub struct OperationJournal<'journal, 'store> {
    session: &'journal mut WriteSession<'store>,
    file: File,
    operation_id: Uuid,
    current_state: StateFile,
    next_sequence: u64,
    open_steps: BTreeMap<u64, StepToken>,
    failed: bool,
    poisoned: bool,
    record_count: usize,
    max_bytes: u64,
    max_records: usize,
}

impl<'journal, 'store> OperationJournal<'journal, 'store> {
    /// Creates a UUID-named journal and durably writes its Begin record.
    pub fn begin(
        session: &'journal mut WriteSession<'store>,
        plan_digest: PlanDigest,
    ) -> Result<Self, JournalError> {
        session.reserve_journal().map_err(JournalError::state)?;
        session
            .revalidate_state_directory()
            .map_err(JournalError::state)?;
        let current_state = session.current_state().map_err(JournalError::state)?;
        let journal_directory = prepare_journal_directory(session.state_directory())?;
        let operation_id = Uuid::new_v4();
        let journal_path = journal_directory.join(format!("{operation_id}.jsonl"));
        let mut options = OpenOptions::new();
        options.read(true).append(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(&journal_path)
            .map_err(|source| JournalError::io("create operation journal", source))?;
        harden_path_permissions(&journal_path)
            .map_err(|source| JournalError::io("harden operation journal", source))?;
        let (max_bytes, max_records) = session.journal_limits();
        let mut journal = Self {
            session,
            file,
            operation_id,
            current_state,
            next_sequence: 1,
            open_steps: BTreeMap::new(),
            failed: false,
            poisoned: false,
            record_count: 0,
            max_bytes,
            max_records,
        };
        journal.append(&JournalRecord::Begin {
            format_version: JOURNAL_FORMAT_VERSION,
            operation_id,
            lineage: journal.current_state.lineage(),
            starting_serial: journal.current_state.serial(),
            plan_digest,
        })?;
        sync_directory(&journal_directory).map_err(journal_directory_sync_unknown)?;

        Ok(journal)
    }

    /// Returns the operation identifier used as the journal filename.
    #[must_use]
    pub const fn operation_id(&self) -> Uuid {
        self.operation_id
    }

    /// Durably records the start of one remote mutation.
    pub fn start_step(
        &mut self,
        address: ResourceAddress,
        action: JournalAction,
    ) -> Result<StepToken, JournalError> {
        self.start_step_internal(address, action, None)
    }

    /// Durably records a remote mutation with its exact safe recovery intent.
    pub fn start_recoverable_step(
        &mut self,
        address: ResourceAddress,
        action: JournalAction,
        expected_checkpoint: ExpectedCheckpoint,
    ) -> Result<StepToken, JournalError> {
        expected_checkpoint
            .validate_for(&address, action, &self.current_state)
            .map_err(|_| JournalError::InvalidExpectedCheckpoint)?;
        self.start_step_internal(address, action, Some(Box::new(expected_checkpoint)))
    }

    fn start_step_internal(
        &mut self,
        address: ResourceAddress,
        action: JournalAction,
        expected_checkpoint: Option<Box<ExpectedCheckpoint>>,
    ) -> Result<StepToken, JournalError> {
        self.ensure_startable()?;
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(JournalError::SequenceOverflow)?;

        let token = StepToken {
            operation_id: self.operation_id,
            sequence: self.next_sequence,
            address,
            action,
            expected_checkpoint,
        };
        self.append_or_poison(&JournalRecord::StepStarted {
            sequence: token.sequence,
            address: token.address.clone(),
            action: token.action,
            expected_checkpoint: token.expected_checkpoint.clone(),
        })?;
        self.open_steps.insert(token.sequence, token.clone());
        self.next_sequence = next_sequence;

        Ok(token)
    }

    /// Records remote success before checkpointing exactly one state serial.
    pub fn succeed(
        &mut self,
        token: StepToken,
        remote_id: Option<RemoteId>,
        proposed: &StateFile,
    ) -> Result<(), JournalError> {
        self.ensure_resolvable()?;
        self.validate_token(&token)?;
        validate_success_remote_id(token.action, remote_id.as_ref())?;
        self.append_or_poison(&JournalRecord::StepSucceeded {
            sequence: token.sequence,
            address: token.address.clone(),
            action: token.action,
            remote_id: remote_id.clone(),
        })?;
        self.poisoned = true;
        validate_state_transition(&self.current_state, proposed, &token, remote_id.as_ref())?;

        let expected = ExpectedState::from_state(&self.current_state);
        self.session
            .checkpoint_from_journal(expected, proposed)
            .map_err(JournalError::state)?;

        self.current_state = proposed.clone();
        self.open_steps.remove(&token.sequence);
        self.poisoned = false;

        Ok(())
    }

    /// Durably records a constrained failure without arbitrary text.
    pub fn fail(
        &mut self,
        token: StepToken,
        failure_code: FailureCode,
    ) -> Result<(), JournalError> {
        self.ensure_resolvable()?;
        self.validate_token(&token)?;
        self.append_or_poison(&JournalRecord::StepFailed {
            sequence: token.sequence,
            address: token.address,
            action: token.action,
            failure_code,
        })?;
        self.open_steps.remove(&token.sequence);
        self.failed = true;

        Ok(())
    }

    /// Durably closes a successful operation at its current state revision.
    pub fn commit(mut self) -> Result<(), JournalError> {
        self.ensure_startable()?;
        if !self.open_steps.is_empty() {
            return Err(JournalError::StepStillOpen);
        }
        if let Err(error) = self
            .session
            .assert_current_revision(self.current_state.revision())
        {
            self.poisoned = true;
            return Err(JournalError::state(error));
        }

        self.append_or_poison(&JournalRecord::Commit {
            final_serial: self.current_state.serial(),
        })?;
        self.session.complete_journal();

        Ok(())
    }

    fn validate_token(&self, token: &StepToken) -> Result<(), JournalError> {
        if self.open_steps.get(&token.sequence) != Some(token)
            || token.operation_id != self.operation_id
        {
            return Err(JournalError::InvalidStepToken);
        }

        Ok(())
    }

    fn ensure_startable(&self) -> Result<(), JournalError> {
        if self.failed || self.poisoned {
            return Err(JournalError::Terminal);
        }

        Ok(())
    }

    fn ensure_resolvable(&self) -> Result<(), JournalError> {
        if self.poisoned {
            return Err(JournalError::Terminal);
        }

        Ok(())
    }

    fn append(&mut self, record: &JournalRecord) -> Result<(), JournalError> {
        if self.record_count >= self.max_records {
            return Err(JournalError::RecordLimitExceeded);
        }

        let mut bytes = serde_json::to_vec(record).map_err(|_| JournalError::Serialization)?;
        bytes.push(b'\n');
        let current_length = self
            .file
            .metadata()
            .map_err(|source| JournalError::io("inspect operation journal", source))?
            .len();
        if current_length.saturating_add(bytes.len() as u64) > self.max_bytes {
            return Err(JournalError::ByteLimitExceeded);
        }

        self.file
            .write_all(&bytes)
            .map_err(|source| JournalError::io("append operation journal", source))?;
        self.file
            .flush()
            .map_err(|source| JournalError::io("flush operation journal", source))?;
        self.file
            .sync_all()
            .map_err(|source| JournalError::io("sync operation journal", source))?;
        self.record_count += 1;

        Ok(())
    }

    fn append_or_poison(&mut self, record: &JournalRecord) -> Result<(), JournalError> {
        if let Err(error) = self.append(record) {
            self.poisoned = true;
            return Err(error);
        }

        Ok(())
    }
}

fn validate_success_remote_id(
    action: JournalAction,
    remote_id: Option<&RemoteId>,
) -> Result<(), JournalError> {
    match (action, remote_id) {
        (JournalAction::Create, None) => Err(JournalError::CreateRequiresRemoteId),
        (JournalAction::Delete | JournalAction::Forget | JournalAction::Move, Some(_)) => {
            Err(JournalError::DeleteForbidsRemoteId)
        }
        _ => Ok(()),
    }
}

fn validate_state_transition(
    current: &StateFile,
    proposed: &StateFile,
    token: &StepToken,
    recorded_remote_id: Option<&RemoteId>,
) -> Result<(), JournalError> {
    let current_resource = current.resource(&token.address);
    let proposed_resource = proposed.resource(&token.address);
    let valid = match token.action {
        JournalAction::Create => {
            current_resource.is_none()
                && proposed_resource
                    .is_some_and(|resource| recorded_remote_id == Some(resource.remote_id()))
                && proposed.resources().len() == current.resources().len().saturating_add(1)
                && unchanged_resources(current, proposed, &token.address)
        }
        JournalAction::Delete | JournalAction::Forget => {
            current_resource.is_some()
                && proposed_resource.is_none()
                && current.resources().len() == proposed.resources().len().saturating_add(1)
                && unchanged_resources(current, proposed, &token.address)
        }
        JournalAction::Move => token
            .expected_checkpoint
            .as_ref()
            .and_then(|expected| expected.move_from.as_ref())
            .is_some_and(|from| {
                let mut expected = current.clone();
                expected.move_resource(from, token.address.clone()).is_ok() && &expected == proposed
            }),
        JournalAction::Update | JournalAction::Deploy => {
            current_resource.is_some_and(|current_resource| {
                proposed_resource.is_some_and(|proposed_resource| {
                    current_resource.remote_id() == proposed_resource.remote_id()
                        && recorded_remote_id
                            .is_none_or(|remote_id| remote_id == proposed_resource.remote_id())
                })
            }) && current.resources().len() == proposed.resources().len()
                && unchanged_resources(current, proposed, &token.address)
        }
    };
    if !valid {
        return Err(JournalError::InvalidStateTransition);
    }
    if token.expected_checkpoint.as_ref().is_some_and(|expected| {
        !expected.matches_proposed(&token.address, token.action, proposed, recorded_remote_id)
    }) {
        return Err(JournalError::InvalidExpectedCheckpoint);
    }

    Ok(())
}

fn unchanged_resources(
    current: &StateFile,
    proposed: &StateFile,
    changed_address: &ResourceAddress,
) -> bool {
    current.resources().iter().all(|(address, resource)| {
        address == changed_address || proposed.resource(address) == Some(resource)
    }) && proposed.resources().iter().all(|(address, resource)| {
        address == changed_address || current.resource(address) == Some(resource)
    })
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum JournalRecord {
    Begin {
        format_version: u32,
        operation_id: Uuid,
        lineage: Uuid,
        starting_serial: u64,
        plan_digest: PlanDigest,
    },
    StepStarted {
        sequence: u64,
        address: ResourceAddress,
        action: JournalAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_checkpoint: Option<Box<ExpectedCheckpoint>>,
    },
    StepSucceeded {
        sequence: u64,
        address: ResourceAddress,
        action: JournalAction,
        remote_id: Option<RemoteId>,
    },
    StepFailed {
        sequence: u64,
        address: ResourceAddress,
        action: JournalAction,
        failure_code: FailureCode,
    },
    StepRecoveredNoChange {
        sequence: u64,
        address: ResourceAddress,
        action: JournalAction,
    },
    Commit {
        final_serial: u64,
    },
    RecoveryResolved {
        final_serial: u64,
    },
}

/// A safe operation-journal failure.
#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal directory is not a trusted canonical directory")]
    UnsafeJournalDirectory,
    #[error("the supplied step token is not an open journal step")]
    InvalidStepToken,
    #[error("the journal still has an open step")]
    StepStillOpen,
    #[error("the operation journal is terminal and requires recovery")]
    Terminal,
    #[error("create success requires a remote identifier")]
    CreateRequiresRemoteId,
    #[error("delete success cannot contain a remote identifier")]
    DeleteForbidsRemoteId,
    #[error("proposed state does not match the journaled action transition")]
    InvalidStateTransition,
    #[error("proposed state does not match the durable expected checkpoint")]
    InvalidExpectedCheckpoint,
    #[error("journal sequence cannot advance beyond its maximum value")]
    SequenceOverflow,
    #[error("operation journal exceeds its byte limit")]
    ByteLimitExceeded,
    #[error("operation journal exceeds its record limit")]
    RecordLimitExceeded,
    #[error("operation journal serialization failed")]
    Serialization,
    #[error("operation journal creation was persisted but {stage:?} failed; durability is unknown")]
    DurabilityOutcomeUnknown {
        stage: JournalDurabilityStage,
        #[source]
        source: io::Error,
    },
    #[error("state checkpoint failed after a durable journal record")]
    State {
        #[source]
        source: Box<StateStoreError>,
    },
    #[error("could not {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

/// The post-persist journal durability operation whose outcome is uncertain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalDurabilityStage {
    DirectoryEntrySync,
}

impl JournalError {
    fn state(source: StateStoreError) -> Self {
        Self::State {
            source: Box::new(source),
        }
    }

    fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }
}

fn journal_directory_sync_unknown(source: io::Error) -> JournalError {
    JournalError::DurabilityOutcomeUnknown {
        stage: JournalDurabilityStage::DirectoryEntrySync,
        source,
    }
}

/// Read-only classification of all operation journals.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryStatus {
    Clean,
    RecoveryRequired(Box<RecoverySummary>),
}

/// Why one otherwise valid journal requires recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryReason {
    Begun,
    StepInProgress,
    SuccessWithoutCheckpoint,
    CheckpointedWithoutCommit,
    Failed(FailureCode),
    TruncatedTail,
}

/// A safe description of the step whose outcome needs attention.
#[derive(Clone, Eq, PartialEq)]
pub struct RecoveryStep {
    sequence: u64,
    address: ResourceAddress,
    action: JournalAction,
    expected_checkpoint: Option<Box<ExpectedCheckpoint>>,
    outcome: RecoveryStepOutcome,
}

impl RecoveryStep {
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn address(&self) -> &ResourceAddress {
        &self.address
    }

    #[must_use]
    pub const fn action(&self) -> JournalAction {
        self.action
    }

    /// Returns the exact precondition and target when the step used format v2 evidence.
    #[must_use]
    pub fn expected_checkpoint(&self) -> Option<&ExpectedCheckpoint> {
        self.expected_checkpoint.as_deref()
    }

    /// Returns the durable result known for this step.
    #[must_use]
    pub const fn outcome(&self) -> &RecoveryStepOutcome {
        &self.outcome
    }
}

impl fmt::Debug for RecoveryStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveryStep")
            .field("sequence", &self.sequence)
            .field("address", &self.address)
            .field("action", &self.action)
            .field(
                "expected_checkpoint",
                &self.expected_checkpoint.as_ref().map(|_| "[REDACTED]"),
            )
            .field("outcome", &self.outcome)
            .finish()
    }
}

/// Durable evidence for one started journal step.
#[derive(Clone, Eq, PartialEq)]
pub enum RecoveryStepOutcome {
    InProgress,
    Succeeded {
        remote_id: Option<RemoteId>,
        checkpointed: bool,
    },
    Failed(FailureCode),
    NoChangeConfirmed,
}

impl RecoveryStepOutcome {
    /// Returns whether no terminal result record exists for the step.
    #[must_use]
    pub const fn is_in_progress(&self) -> bool {
        matches!(self, Self::InProgress)
    }

    /// Returns the successful remote identity, when recorded.
    #[must_use]
    pub const fn remote_id(&self) -> Option<&RemoteId> {
        match self {
            Self::Succeeded { remote_id, .. } => remote_id.as_ref(),
            Self::InProgress | Self::Failed(_) | Self::NoChangeConfirmed => None,
        }
    }

    /// Returns whether a successful result is reflected in durable state.
    #[must_use]
    pub const fn is_checkpointed(&self) -> bool {
        matches!(
            self,
            Self::Succeeded {
                checkpointed: true,
                ..
            }
        )
    }
}

impl fmt::Debug for RecoveryStepOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InProgress => formatter.write_str("InProgress"),
            Self::Succeeded { checkpointed, .. } => formatter
                .debug_struct("Succeeded")
                .field("remote_id", &"[REDACTED]")
                .field("checkpointed", checkpointed)
                .finish(),
            Self::Failed(code) => formatter.debug_tuple("Failed").field(code).finish(),
            Self::NoChangeConfirmed => formatter.write_str("NoChangeConfirmed"),
        }
    }
}

/// The last confirmed and potentially uncertain part of one operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoverySummary {
    format_version: u32,
    operation_id: Uuid,
    plan_digest: PlanDigest,
    lineage: Uuid,
    starting_serial: u64,
    durable_serial: u64,
    last_confirmed_sequence: Option<u64>,
    uncertain_step: Option<RecoveryStep>,
    steps: Vec<RecoveryStep>,
    reason: RecoveryReason,
    truncated_tail: bool,
}

impl RecoverySummary {
    /// Returns the journal format used by the interrupted operation.
    #[must_use]
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    #[must_use]
    pub const fn operation_id(&self) -> Uuid {
        self.operation_id
    }

    /// Returns the immutable digest of the plan that began the operation.
    #[must_use]
    pub const fn plan_digest(&self) -> &PlanDigest {
        &self.plan_digest
    }

    #[must_use]
    pub const fn lineage(&self) -> Uuid {
        self.lineage
    }

    #[must_use]
    pub const fn starting_serial(&self) -> u64 {
        self.starting_serial
    }

    #[must_use]
    pub const fn durable_serial(&self) -> u64 {
        self.durable_serial
    }

    #[must_use]
    pub const fn last_confirmed_sequence(&self) -> Option<u64> {
        self.last_confirmed_sequence
    }

    #[must_use]
    pub const fn uncertain_step(&self) -> Option<&RecoveryStep> {
        self.uncertain_step.as_ref()
    }

    /// Returns every started step in durable sequence order.
    #[must_use]
    pub fn steps(&self) -> &[RecoveryStep] {
        &self.steps
    }

    #[must_use]
    pub const fn reason(&self) -> &RecoveryReason {
        &self.reason
    }

    #[must_use]
    pub const fn has_truncated_tail(&self) -> bool {
        self.truncated_tail
    }
}

impl StateStore {
    /// Scans all journals without changing recovery evidence.
    pub fn recovery_status(&self) -> Result<RecoveryStatus, StateStoreError> {
        scan_recovery(self).map_err(StateStoreError::from_recovery_scan)
    }

    /// Acquires the ordinary writer lock and opens the single recoverable journal.
    pub fn begin_recovery(&self) -> Result<RecoverySession<'_>, RecoveryError> {
        let mut session = self.begin_recovery_write().map_err(RecoveryError::state)?;
        let mut summary = required_summary(self)?;
        if summary.has_truncated_tail() {
            archive_and_trim_tail(session.state_directory(), &summary, self.journal_limits())?;
            summary = required_summary(self)?;
        }
        validate_recovery_safety(&summary)?;
        session.reserve_journal().map_err(RecoveryError::state)?;

        let path = journal_path(session.state_directory(), summary.operation_id());
        let record_count = count_records(&path, self.journal_limits().0)?;
        let mut options = OpenOptions::new();
        options.read(true).append(true);
        let file = options
            .open(&path)
            .map_err(|source| RecoveryError::io("open recovery journal", source))?;
        harden_path_permissions(&path)
            .map_err(|source| RecoveryError::io("harden recovery journal", source))?;
        let current_state = session.current_state().map_err(RecoveryError::state)?;
        let (max_bytes, max_records) = self.journal_limits();

        Ok(RecoverySession {
            session,
            file,
            summary,
            current_state,
            record_count,
            max_bytes,
            max_records,
            poisoned: false,
        })
    }
}

/// An exclusive session for completing one interrupted operation.
pub struct RecoverySession<'store> {
    session: WriteSession<'store>,
    file: File,
    summary: RecoverySummary,
    current_state: StateFile,
    record_count: usize,
    max_bytes: u64,
    max_records: usize,
    poisoned: bool,
}

impl RecoverySession<'_> {
    /// Returns all immutable and durable evidence for the interrupted operation.
    #[must_use]
    pub const fn evidence(&self) -> &RecoverySummary {
        &self.summary
    }

    /// Returns the snapshot against which the next recovered transition must be built.
    #[must_use]
    pub const fn current_state(&self) -> &StateFile {
        &self.current_state
    }

    /// Reconstructs the exact next checkpoint from durable evidence.
    pub fn expected_proposed_state(
        &self,
        sequence: u64,
        remote_id: Option<RemoteId>,
    ) -> Result<StateFile, RecoveryError> {
        let step = self.step(sequence)?;
        let expected = step
            .expected_checkpoint
            .as_ref()
            .ok_or(RecoveryError::IncompleteEvidence)?;
        expected
            .validate_for(&step.address, step.action, &self.current_state)
            .map_err(|_| RecoveryError::InvalidExpectedCheckpoint)?;
        validate_success_remote_id(step.action, remote_id.as_ref())
            .map_err(RecoveryError::journal)?;
        let mut proposed = self.current_state.clone();
        match (&expected.after, step.action) {
            (Some(after), JournalAction::Create) => {
                let remote_id = remote_id
                    .ok_or_else(|| RecoveryError::journal(JournalError::CreateRequiresRemoteId))?;
                proposed
                    .upsert_resource(
                        step.address.clone(),
                        after
                            .materialize(remote_id)
                            .map_err(|_| RecoveryError::InvalidExpectedCheckpoint)?,
                    )
                    .map_err(|_| RecoveryError::InvalidExpectedCheckpoint)?;
            }
            (Some(after), JournalAction::Update | JournalAction::Deploy) => {
                let before = expected
                    .before
                    .as_ref()
                    .ok_or(RecoveryError::InvalidExpectedCheckpoint)?;
                proposed
                    .upsert_resource(
                        step.address.clone(),
                        after
                            .materialize(before.remote_id().clone())
                            .map_err(|_| RecoveryError::InvalidExpectedCheckpoint)?,
                    )
                    .map_err(|_| RecoveryError::InvalidExpectedCheckpoint)?;
            }
            (Some(_), JournalAction::Move) => {
                let from = expected
                    .move_from
                    .as_ref()
                    .ok_or(RecoveryError::InvalidExpectedCheckpoint)?;
                proposed
                    .move_resource(from, step.address.clone())
                    .map_err(|_| RecoveryError::InvalidExpectedCheckpoint)?;
                if !expected.matches_proposed(&step.address, step.action, &proposed, None) {
                    return Err(RecoveryError::InvalidExpectedCheckpoint);
                }
            }
            (None, JournalAction::Delete | JournalAction::Forget) => {
                proposed
                    .remove_resource(&step.address)
                    .map_err(|_| RecoveryError::InvalidExpectedCheckpoint)?;
            }
            _ => return Err(RecoveryError::InvalidExpectedCheckpoint),
        }

        Ok(proposed)
    }

    /// Records a now-confirmed success and checkpoints its exact durable target.
    pub fn checkpoint_uncertain_success(
        &mut self,
        sequence: u64,
        remote_id: Option<RemoteId>,
        proposed: &StateFile,
    ) -> Result<(), RecoveryError> {
        self.ensure_usable()?;
        let step = self.step(sequence)?.clone();
        if step.outcome != RecoveryStepOutcome::InProgress {
            return Err(RecoveryError::StepAlreadyResolved);
        }
        let expected_checkpoint = step
            .expected_checkpoint
            .clone()
            .ok_or(RecoveryError::IncompleteEvidence)?;
        validate_success_remote_id(step.action, remote_id.as_ref())
            .map_err(RecoveryError::journal)?;
        self.append_or_poison(&JournalRecord::StepSucceeded {
            sequence,
            address: step.address.clone(),
            action: step.action,
            remote_id: remote_id.clone(),
        })?;
        self.step_mut(sequence)?.outcome = RecoveryStepOutcome::Succeeded {
            remote_id: remote_id.clone(),
            checkpointed: false,
        };
        self.checkpoint_step(&step, *expected_checkpoint, remote_id.as_ref(), proposed)
    }

    /// Checkpoints a success record that was durable before the prior process stopped.
    pub fn checkpoint_recorded_success(
        &mut self,
        sequence: u64,
        proposed: &StateFile,
    ) -> Result<(), RecoveryError> {
        self.ensure_usable()?;
        let step = self.step(sequence)?.clone();
        let remote_id = match &step.outcome {
            RecoveryStepOutcome::Succeeded {
                remote_id,
                checkpointed: false,
            } => remote_id.clone(),
            RecoveryStepOutcome::InProgress
            | RecoveryStepOutcome::Succeeded {
                checkpointed: true, ..
            }
            | RecoveryStepOutcome::Failed(_)
            | RecoveryStepOutcome::NoChangeConfirmed => {
                return Err(RecoveryError::StepAlreadyResolved);
            }
        };
        let expected_checkpoint = step
            .expected_checkpoint
            .clone()
            .ok_or(RecoveryError::IncompleteEvidence)?;
        self.checkpoint_step(&step, *expected_checkpoint, remote_id.as_ref(), proposed)
    }

    /// Records that an interrupted mutation made no remote or local change.
    pub fn confirm_no_change(&mut self, sequence: u64) -> Result<(), RecoveryError> {
        self.ensure_usable()?;
        let step = self.step(sequence)?.clone();
        if step.outcome != RecoveryStepOutcome::InProgress {
            return Err(RecoveryError::StepAlreadyResolved);
        }
        if step.expected_checkpoint.is_none() {
            return Err(RecoveryError::IncompleteEvidence);
        }
        self.append_or_poison(&JournalRecord::StepRecoveredNoChange {
            sequence,
            address: step.address,
            action: step.action,
        })?;
        self.step_mut(sequence)?.outcome = RecoveryStepOutcome::NoChangeConfirmed;

        Ok(())
    }

    /// Durably marks recovery complete and releases the session journal guard.
    pub fn resolve(mut self) -> Result<(), RecoveryError> {
        self.ensure_usable()?;
        if self.summary.steps.iter().any(|step| {
            matches!(
                step.outcome,
                RecoveryStepOutcome::InProgress
                    | RecoveryStepOutcome::Succeeded {
                        checkpointed: false,
                        ..
                    }
            )
        }) {
            return Err(RecoveryError::UnresolvedSteps);
        }
        self.session
            .assert_current_revision(self.current_state.revision())
            .map_err(RecoveryError::state)?;
        self.append_or_poison(&JournalRecord::RecoveryResolved {
            final_serial: self.current_state.serial(),
        })?;
        self.session.complete_journal();

        Ok(())
    }

    fn checkpoint_step(
        &mut self,
        step: &RecoveryStep,
        expected_checkpoint: ExpectedCheckpoint,
        remote_id: Option<&RemoteId>,
        proposed: &StateFile,
    ) -> Result<(), RecoveryError> {
        let token = StepToken {
            operation_id: self.summary.operation_id,
            sequence: step.sequence,
            address: step.address.clone(),
            action: step.action,
            expected_checkpoint: Some(Box::new(expected_checkpoint)),
        };
        validate_state_transition(&self.current_state, proposed, &token, remote_id)
            .map_err(RecoveryError::journal)?;
        let expected = ExpectedState::from_state(&self.current_state);
        self.session
            .checkpoint_from_journal(expected, proposed)
            .map_err(RecoveryError::state)?;
        self.current_state = proposed.clone();
        self.summary.durable_serial = proposed.serial();
        self.summary.last_confirmed_sequence = Some(step.sequence);
        if let RecoveryStepOutcome::Succeeded { checkpointed, .. } =
            &mut self.step_mut(step.sequence)?.outcome
        {
            *checkpointed = true;
        }

        Ok(())
    }

    fn step(&self, sequence: u64) -> Result<&RecoveryStep, RecoveryError> {
        self.summary
            .steps
            .iter()
            .find(|step| step.sequence == sequence)
            .ok_or(RecoveryError::InvalidStep)
    }

    fn step_mut(&mut self, sequence: u64) -> Result<&mut RecoveryStep, RecoveryError> {
        self.summary
            .steps
            .iter_mut()
            .find(|step| step.sequence == sequence)
            .ok_or(RecoveryError::InvalidStep)
    }

    fn ensure_usable(&self) -> Result<(), RecoveryError> {
        if self.poisoned {
            Err(RecoveryError::Terminal)
        } else {
            Ok(())
        }
    }

    fn append_or_poison(&mut self, record: &JournalRecord) -> Result<(), RecoveryError> {
        if let Err(error) = append_record(
            &mut self.file,
            &mut self.record_count,
            self.max_bytes,
            self.max_records,
            record,
        ) {
            self.poisoned = true;
            return Err(error);
        }

        Ok(())
    }
}

/// A safe failure while opening or completing interrupted state recovery.
#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("there is no incomplete operation journal to recover")]
    NoRecoveryRequired,
    #[error("incomplete evidence cannot safely determine the interrupted outcome")]
    IncompleteEvidence,
    #[error("durable expected checkpoint evidence is inconsistent")]
    InvalidExpectedCheckpoint,
    #[error("the requested recovery step does not exist")]
    InvalidStep,
    #[error("the requested recovery step is already resolved")]
    StepAlreadyResolved,
    #[error("all interrupted steps must be resolved before recovery can finish")]
    UnresolvedSteps,
    #[error("the recovery journal is terminal after a durability failure")]
    Terminal,
    #[error("a different truncated journal is already archived at the recovery path")]
    ArchiveCollision,
    #[error("operation journal recovery failed")]
    Journal {
        #[source]
        source: Box<JournalError>,
    },
    #[error("state recovery failed")]
    State {
        #[source]
        source: Box<StateStoreError>,
    },
    #[error("could not {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

impl RecoveryError {
    fn journal(source: JournalError) -> Self {
        Self::Journal {
            source: Box::new(source),
        }
    }

    fn state(source: StateStoreError) -> Self {
        Self::State {
            source: Box::new(source),
        }
    }

    fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }
}

fn required_summary(store: &StateStore) -> Result<RecoverySummary, RecoveryError> {
    match scan_recovery(store).map_err(map_recovery_scan_error)? {
        RecoveryStatus::Clean => Err(RecoveryError::NoRecoveryRequired),
        RecoveryStatus::RecoveryRequired(summary) => Ok(*summary),
    }
}

fn map_recovery_scan_error(error: RecoveryScanError) -> RecoveryError {
    match error {
        RecoveryScanError::Io(source) => RecoveryError::io("scan recovery journals", source),
        RecoveryScanError::Corrupt | RecoveryScanError::Ambiguous => {
            RecoveryError::state(StateStoreError::from_recovery_scan(error))
        }
    }
}

fn validate_recovery_safety(summary: &RecoverySummary) -> Result<(), RecoveryError> {
    if summary.steps.iter().any(|step| {
        step.expected_checkpoint.is_none()
            && matches!(
                step.outcome,
                RecoveryStepOutcome::InProgress
                    | RecoveryStepOutcome::Succeeded {
                        checkpointed: false,
                        ..
                    }
            )
    }) {
        return Err(RecoveryError::IncompleteEvidence);
    }
    if summary.steps.iter().any(|step| {
        matches!(
            step.outcome,
            RecoveryStepOutcome::Failed(
                FailureCode::Timeout | FailureCode::TransportOutcomeUnknown | FailureCode::Internal
            )
        )
    }) {
        return Err(RecoveryError::IncompleteEvidence);
    }

    Ok(())
}

fn journal_path(state_directory: &Path, operation_id: Uuid) -> PathBuf {
    state_directory
        .join(JOURNAL_DIRECTORY)
        .join(format!("{operation_id}.jsonl"))
}

fn count_records(path: &Path, max_bytes: u64) -> Result<usize, RecoveryError> {
    let bytes =
        fs::read(path).map_err(|source| RecoveryError::io("read recovery journal", source))?;
    if bytes.len() as u64 > max_bytes || !bytes.ends_with(b"\n") {
        return Err(RecoveryError::state(StateStoreError::JournalCorrupt));
    }

    Ok(bytes.iter().filter(|byte| **byte == b'\n').count())
}

fn archive_and_trim_tail(
    state_directory: &Path,
    summary: &RecoverySummary,
    limits: (u64, usize),
) -> Result<(), RecoveryError> {
    let journal_directory = state_directory.join(JOURNAL_DIRECTORY);
    let path = journal_path(state_directory, summary.operation_id);
    let bytes =
        fs::read(&path).map_err(|source| RecoveryError::io("read truncated journal", source))?;
    if bytes.len() as u64 > limits.0 || bytes.ends_with(b"\n") {
        return Err(RecoveryError::state(StateStoreError::JournalCorrupt));
    }
    let trim_length = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .and_then(|index| index.checked_add(1))
        .ok_or_else(|| RecoveryError::state(StateStoreError::JournalCorrupt))?;

    let archive_directory = journal_directory.join(JOURNAL_ARCHIVE_DIRECTORY);
    match fs::symlink_metadata(&archive_directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(RecoveryError::state(StateStoreError::JournalCorrupt));
        }
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&archive_directory)
                .map_err(|source| RecoveryError::io("create journal archive", source))?;
        }
        Err(source) => return Err(RecoveryError::io("inspect journal archive", source)),
    }
    harden_directory_permissions(&archive_directory)
        .map_err(|source| RecoveryError::io("harden journal archive", source))?;
    let archive_path = archive_directory.join(format!(
        "{}.{}.truncated.jsonl",
        summary.operation_id,
        bytes.len()
    ));
    match fs::read(&archive_path) {
        Ok(existing) if existing != bytes => return Err(RecoveryError::ArchiveCollision),
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut archive = options
                .open(&archive_path)
                .map_err(|source| RecoveryError::io("create truncated journal archive", source))?;
            archive
                .write_all(&bytes)
                .map_err(|source| RecoveryError::io("write truncated journal archive", source))?;
            archive
                .sync_all()
                .map_err(|source| RecoveryError::io("sync truncated journal archive", source))?;
            harden_path_permissions(&archive_path)
                .map_err(|source| RecoveryError::io("harden truncated journal archive", source))?;
            sync_directory(&archive_directory)
                .map_err(|source| RecoveryError::io("sync journal archive directory", source))?;
        }
        Err(source) => return Err(RecoveryError::io("read truncated journal archive", source)),
    }

    let file = OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|source| RecoveryError::io("open truncated journal", source))?;
    file.set_len(trim_length as u64)
        .map_err(|source| RecoveryError::io("trim truncated journal", source))?;
    file.sync_all()
        .map_err(|source| RecoveryError::io("sync trimmed journal", source))?;
    sync_directory(&journal_directory)
        .map_err(|source| RecoveryError::io("sync journal directory", source))?;

    Ok(())
}

fn append_record(
    file: &mut File,
    record_count: &mut usize,
    max_bytes: u64,
    max_records: usize,
    record: &JournalRecord,
) -> Result<(), RecoveryError> {
    if *record_count >= max_records {
        return Err(RecoveryError::journal(JournalError::RecordLimitExceeded));
    }
    let mut bytes = serde_json::to_vec(record)
        .map_err(|_| RecoveryError::journal(JournalError::Serialization))?;
    bytes.push(b'\n');
    let current_length = file
        .metadata()
        .map_err(|source| RecoveryError::io("inspect recovery journal", source))?
        .len();
    if current_length.saturating_add(bytes.len() as u64) > max_bytes {
        return Err(RecoveryError::journal(JournalError::ByteLimitExceeded));
    }
    file.write_all(&bytes)
        .map_err(|source| RecoveryError::io("append recovery journal", source))?;
    file.flush()
        .map_err(|source| RecoveryError::io("flush recovery journal", source))?;
    file.sync_all()
        .map_err(|source| RecoveryError::io("sync recovery journal", source))?;
    *record_count += 1;

    Ok(())
}

pub(crate) enum RecoveryScanError {
    Corrupt,
    Ambiguous,
    Io(io::Error),
}

pub(crate) fn scan_recovery(store: &StateStore) -> Result<RecoveryStatus, RecoveryScanError> {
    let journal_directory = store.state_directory_path().join(JOURNAL_DIRECTORY);
    let metadata = match fs::symlink_metadata(&journal_directory) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(RecoveryStatus::Clean);
        }
        Err(source) => return Err(RecoveryScanError::Io(source)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RecoveryScanError::Corrupt);
    }
    let canonical = fs::canonicalize(&journal_directory).map_err(RecoveryScanError::Io)?;
    if canonical != journal_directory {
        return Err(RecoveryScanError::Corrupt);
    }

    let state = store
        .current_state_for_journal()
        .map_err(map_store_scan_error)?;
    let (max_bytes, max_records) = store.journal_limits();
    let mut paths = Vec::new();
    for entry in fs::read_dir(&journal_directory).map_err(RecoveryScanError::Io)? {
        let entry = entry.map_err(RecoveryScanError::Io)?;
        if entry.file_name() == JOURNAL_ARCHIVE_DIRECTORY {
            validate_archive_directory(&entry.path())?;
            continue;
        }
        paths.push(entry.path());
    }
    paths.sort();

    let mut incomplete = Vec::new();
    for path in paths {
        match scan_journal(&path, state.as_ref(), max_bytes, max_records)? {
            ScannedJournal::Committed => {}
            ScannedJournal::Incomplete(summary) => incomplete.push(*summary),
        }
    }

    match incomplete.len() {
        0 => Ok(RecoveryStatus::Clean),
        1 => Ok(RecoveryStatus::RecoveryRequired(Box::new(
            incomplete.pop().expect("one summary must exist"),
        ))),
        _ => Err(RecoveryScanError::Ambiguous),
    }
}

fn validate_archive_directory(path: &Path) -> Result<(), RecoveryScanError> {
    let metadata = fs::symlink_metadata(path).map_err(RecoveryScanError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RecoveryScanError::Corrupt);
    }
    let canonical = fs::canonicalize(path).map_err(RecoveryScanError::Io)?;
    if canonical != path {
        return Err(RecoveryScanError::Corrupt);
    }

    Ok(())
}

fn map_store_scan_error(error: StateStoreError) -> RecoveryScanError {
    match error {
        StateStoreError::Io { source, .. } => RecoveryScanError::Io(source),
        _ => RecoveryScanError::Corrupt,
    }
}

enum ScannedJournal {
    Committed,
    Incomplete(Box<RecoverySummary>),
}

fn scan_journal(
    path: &Path,
    state: Option<&StateFile>,
    max_bytes: u64,
    max_records: usize,
) -> Result<ScannedJournal, RecoveryScanError> {
    let operation_id = operation_id_from_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(RecoveryScanError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(RecoveryScanError::Corrupt);
    }
    let mut file = File::open(path).map_err(RecoveryScanError::Io)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(RecoveryScanError::Io)?;
    if bytes.len() as u64 > max_bytes || bytes.is_empty() {
        return Err(RecoveryScanError::Corrupt);
    }

    let trailing_tail = !bytes.ends_with(b"\n");
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if trailing_tail {
        let fragment = lines.pop().ok_or(RecoveryScanError::Corrupt)?;
        if fragment.is_empty() {
            return Err(RecoveryScanError::Corrupt);
        }
    } else {
        lines.pop();
    }
    if lines.is_empty() || lines.len() > max_records || lines.iter().any(|line| line.is_empty()) {
        return Err(RecoveryScanError::Corrupt);
    }

    let records = lines
        .into_iter()
        .map(parse_record)
        .collect::<Result<Vec<_>, _>>()?;
    validate_records(operation_id, records, state, trailing_tail)
}

fn operation_id_from_path(path: &Path) -> Result<Uuid, RecoveryScanError> {
    if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
        return Err(RecoveryScanError::Corrupt);
    }
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or(RecoveryScanError::Corrupt)?;
    let operation_id = Uuid::parse_str(stem).map_err(|_| RecoveryScanError::Corrupt)?;
    if operation_id.to_string() != stem {
        return Err(RecoveryScanError::Corrupt);
    }

    Ok(operation_id)
}

fn parse_record(line: &[u8]) -> Result<JournalRecord, RecoveryScanError> {
    reject_duplicate_keys(line).map_err(|()| RecoveryScanError::Corrupt)?;
    serde_json::from_slice(line).map_err(|_| RecoveryScanError::Corrupt)
}

#[derive(Clone)]
struct ScannedStep {
    sequence: u64,
    address: ResourceAddress,
    action: JournalAction,
}

fn validate_records(
    filename_operation_id: Uuid,
    records: Vec<JournalRecord>,
    state: Option<&StateFile>,
    trailing_tail: bool,
) -> Result<ScannedJournal, RecoveryScanError> {
    let (format_version, operation_id, lineage, starting_serial, plan_digest) =
        match records.first() {
            Some(JournalRecord::Begin {
                format_version,
                operation_id,
                lineage,
                starting_serial,
                plan_digest,
            }) => (
                *format_version,
                *operation_id,
                *lineage,
                *starting_serial,
                plan_digest.clone(),
            ),
            _ => return Err(RecoveryScanError::Corrupt),
        };
    if format_version != JOURNAL_FORMAT_VERSION || operation_id != filename_operation_id {
        return Err(RecoveryScanError::Corrupt);
    }
    let state = state.ok_or(RecoveryScanError::Corrupt)?;
    if state.lineage() != lineage || state.serial() < starting_serial {
        return Err(RecoveryScanError::Corrupt);
    }

    let mut next_sequence = 1_u64;
    let mut journal_serial = starting_serial;
    let mut open = BTreeMap::<u64, ScannedStep>::new();
    let mut steps = BTreeMap::<u64, RecoveryStep>::new();
    let mut succeeded = Vec::<u64>::new();
    let mut failed: Option<(ScannedStep, FailureCode)> = None;
    let mut terminal = None;
    for record in records.into_iter().skip(1) {
        if terminal.is_some() {
            return Err(RecoveryScanError::Corrupt);
        }

        match record {
            JournalRecord::Begin { .. } => return Err(RecoveryScanError::Corrupt),
            JournalRecord::StepStarted {
                sequence,
                address,
                action,
                expected_checkpoint,
            } => {
                if failed.is_some() || sequence != next_sequence {
                    return Err(RecoveryScanError::Corrupt);
                }
                if expected_checkpoint
                    .as_ref()
                    .is_some_and(|expected| !expected.is_well_formed_for(&address, action))
                {
                    return Err(RecoveryScanError::Corrupt);
                }
                open.insert(
                    sequence,
                    ScannedStep {
                        sequence,
                        address: address.clone(),
                        action,
                    },
                );
                steps.insert(
                    sequence,
                    RecoveryStep {
                        sequence,
                        address,
                        action,
                        expected_checkpoint,
                        outcome: RecoveryStepOutcome::InProgress,
                    },
                );
                next_sequence = next_sequence
                    .checked_add(1)
                    .ok_or(RecoveryScanError::Corrupt)?;
            }
            JournalRecord::StepSucceeded {
                sequence,
                address,
                action,
                remote_id,
            } => {
                let step = take_matching_step(&mut open, sequence, &address, action)?;
                validate_scanned_remote_id(action, remote_id.as_ref())?;
                journal_serial = journal_serial
                    .checked_add(1)
                    .ok_or(RecoveryScanError::Corrupt)?;
                let evidence = steps.get_mut(&sequence).ok_or(RecoveryScanError::Corrupt)?;
                evidence.outcome = RecoveryStepOutcome::Succeeded {
                    remote_id,
                    checkpointed: false,
                };
                succeeded.push(step.sequence);
            }
            JournalRecord::StepFailed {
                sequence,
                address,
                action,
                failure_code,
            } => {
                let step = take_matching_step(&mut open, sequence, &address, action)?;
                let evidence = steps.get_mut(&sequence).ok_or(RecoveryScanError::Corrupt)?;
                evidence.outcome = RecoveryStepOutcome::Failed(failure_code);
                failed.get_or_insert((step, failure_code));
            }
            JournalRecord::StepRecoveredNoChange {
                sequence,
                address,
                action,
            } => {
                take_matching_step(&mut open, sequence, &address, action)?;
                let evidence = steps.get_mut(&sequence).ok_or(RecoveryScanError::Corrupt)?;
                evidence.outcome = RecoveryStepOutcome::NoChangeConfirmed;
            }
            JournalRecord::Commit { final_serial } => {
                if !open.is_empty() || failed.is_some() || final_serial != journal_serial {
                    return Err(RecoveryScanError::Corrupt);
                }
                terminal = Some(final_serial);
            }
            JournalRecord::RecoveryResolved { final_serial } => {
                if !open.is_empty() || final_serial != journal_serial {
                    return Err(RecoveryScanError::Corrupt);
                }
                terminal = Some(final_serial);
            }
        }
    }

    if let Some(final_serial) = terminal {
        if trailing_tail || state.serial() < final_serial {
            return Err(RecoveryScanError::Corrupt);
        }

        return Ok(ScannedJournal::Committed);
    }
    let durable_serial = state.serial();
    let success_without_checkpoint =
        !succeeded.is_empty() && durable_serial.checked_add(1) == Some(journal_serial);
    if durable_serial != journal_serial && !success_without_checkpoint {
        return Err(RecoveryScanError::Corrupt);
    }
    if !open.is_empty() && durable_serial != journal_serial {
        return Err(RecoveryScanError::Corrupt);
    }
    if failed.is_some() && durable_serial != journal_serial {
        return Err(RecoveryScanError::Corrupt);
    }

    let confirmed_count = usize::try_from(
        durable_serial
            .checked_sub(starting_serial)
            .ok_or(RecoveryScanError::Corrupt)?,
    )
    .map_err(|_| RecoveryScanError::Corrupt)?;
    if confirmed_count > succeeded.len() {
        return Err(RecoveryScanError::Corrupt);
    }
    let last_confirmed_sequence = confirmed_count
        .checked_sub(1)
        .and_then(|index| succeeded.get(index))
        .copied();
    for sequence in succeeded.iter().take(confirmed_count) {
        let evidence = steps.get_mut(sequence).ok_or(RecoveryScanError::Corrupt)?;
        match &mut evidence.outcome {
            RecoveryStepOutcome::Succeeded { checkpointed, .. } => *checkpointed = true,
            RecoveryStepOutcome::InProgress
            | RecoveryStepOutcome::Failed(_)
            | RecoveryStepOutcome::NoChangeConfirmed => {
                return Err(RecoveryScanError::Corrupt);
            }
        }
    }
    let uncertain_step = if success_without_checkpoint {
        succeeded
            .last()
            .and_then(|sequence| steps.get(sequence))
            .cloned()
    } else {
        open.keys()
            .next()
            .and_then(|sequence| steps.get(sequence))
            .cloned()
    };
    let reason = if trailing_tail {
        RecoveryReason::TruncatedTail
    } else if !open.is_empty() {
        RecoveryReason::StepInProgress
    } else if success_without_checkpoint {
        RecoveryReason::SuccessWithoutCheckpoint
    } else if let Some((_, failure_code)) = failed {
        RecoveryReason::Failed(failure_code)
    } else if !succeeded.is_empty() {
        RecoveryReason::CheckpointedWithoutCommit
    } else {
        RecoveryReason::Begun
    };

    Ok(ScannedJournal::Incomplete(Box::new(RecoverySummary {
        format_version,
        operation_id: filename_operation_id,
        plan_digest,
        lineage,
        starting_serial,
        durable_serial,
        last_confirmed_sequence,
        uncertain_step,
        steps: steps.into_values().collect(),
        reason,
        truncated_tail: trailing_tail,
    })))
}

fn take_matching_step(
    open: &mut BTreeMap<u64, ScannedStep>,
    sequence: u64,
    address: &ResourceAddress,
    action: JournalAction,
) -> Result<ScannedStep, RecoveryScanError> {
    let step = open.remove(&sequence).ok_or(RecoveryScanError::Corrupt)?;
    if step.sequence != sequence || step.address != *address || step.action != action {
        return Err(RecoveryScanError::Corrupt);
    }

    Ok(step)
}

fn validate_scanned_remote_id(
    action: JournalAction,
    remote_id: Option<&RemoteId>,
) -> Result<(), RecoveryScanError> {
    match (action, remote_id) {
        (JournalAction::Create, None)
        | (JournalAction::Delete | JournalAction::Forget | JournalAction::Move, Some(_)) => {
            Err(RecoveryScanError::Corrupt)
        }
        _ => Ok(()),
    }
}

fn prepare_journal_directory(state_directory: &Path) -> Result<PathBuf, JournalError> {
    let journal_directory = state_directory.join(JOURNAL_DIRECTORY);
    let created = match fs::symlink_metadata(&journal_directory) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(JournalError::UnsafeJournalDirectory);
            }
            false
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&journal_directory)
                .map_err(|source| JournalError::io("create journal directory", source))?;
            true
        }
        Err(source) => return Err(JournalError::io("inspect journal directory", source)),
    };
    let canonical = fs::canonicalize(&journal_directory)
        .map_err(|source| JournalError::io("canonicalize journal directory", source))?;
    if canonical != journal_directory {
        return Err(JournalError::UnsafeJournalDirectory);
    }
    harden_directory_permissions(&journal_directory)
        .map_err(|source| JournalError::io("harden journal directory", source))?;
    if created {
        sync_directory(state_directory)
            .map_err(|source| JournalError::io("sync state directory", source))?;
    }

    Ok(journal_directory)
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{JournalDurabilityStage, JournalError, journal_directory_sync_unknown};

    #[test]
    fn classifies_begin_directory_sync_failure_as_outcome_unknown() {
        let error = journal_directory_sync_unknown(io::Error::other("injected sync failure"));

        assert!(matches!(
            error,
            JournalError::DurabilityOutcomeUnknown {
                stage: JournalDurabilityStage::DirectoryEntrySync,
                ..
            }
        ));
    }
}
