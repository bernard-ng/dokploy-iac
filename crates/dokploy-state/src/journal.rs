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
    ExpectedState, RemoteId, ResourceAddress, StateFile, StateStore, StateStoreError, WriteSession,
};

const JOURNAL_DIRECTORY: &str = "journal";
const JOURNAL_FORMAT_VERSION: u32 = 1;

/// A mutation action recorded before and after a remote operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JournalAction {
    Create,
    Update,
    Delete,
    Deploy,
}

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
}

impl fmt::Debug for StepToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StepToken")
            .field("operation_id", &self.operation_id)
            .field("sequence", &self.sequence)
            .field("address", &self.address)
            .field("action", &self.action)
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
        };
        self.append_or_poison(&JournalRecord::StepStarted {
            sequence: token.sequence,
            address: token.address.clone(),
            action: token.action,
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
        (JournalAction::Delete, Some(_)) => Err(JournalError::DeleteForbidsRemoteId),
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
        JournalAction::Delete => {
            current_resource.is_some()
                && proposed_resource.is_none()
                && current.resources().len() == proposed.resources().len().saturating_add(1)
                && unchanged_resources(current, proposed, &token.address)
        }
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
    Commit {
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
    RecoveryRequired(RecoverySummary),
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryStep {
    sequence: u64,
    address: ResourceAddress,
    action: JournalAction,
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
}

/// The last confirmed and potentially uncertain part of one operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoverySummary {
    operation_id: Uuid,
    lineage: Uuid,
    starting_serial: u64,
    durable_serial: u64,
    last_confirmed_sequence: Option<u64>,
    uncertain_step: Option<RecoveryStep>,
    reason: RecoveryReason,
    truncated_tail: bool,
}

impl RecoverySummary {
    #[must_use]
    pub const fn operation_id(&self) -> Uuid {
        self.operation_id
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
        paths.push(entry.path());
    }
    paths.sort();

    let mut incomplete = Vec::new();
    for path in paths {
        match scan_journal(&path, state.as_ref(), max_bytes, max_records)? {
            ScannedJournal::Committed => {}
            ScannedJournal::Incomplete(summary) => incomplete.push(summary),
        }
    }

    match incomplete.len() {
        0 => Ok(RecoveryStatus::Clean),
        1 => Ok(RecoveryStatus::RecoveryRequired(
            incomplete.pop().expect("one summary must exist"),
        )),
        _ => Err(RecoveryScanError::Ambiguous),
    }
}

fn map_store_scan_error(error: StateStoreError) -> RecoveryScanError {
    match error {
        StateStoreError::Io { source, .. } => RecoveryScanError::Io(source),
        _ => RecoveryScanError::Corrupt,
    }
}

enum ScannedJournal {
    Committed,
    Incomplete(RecoverySummary),
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
    let (format_version, operation_id, lineage, starting_serial) = match records.first() {
        Some(JournalRecord::Begin {
            format_version,
            operation_id,
            lineage,
            starting_serial,
            plan_digest: _,
        }) => (*format_version, *operation_id, *lineage, *starting_serial),
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
    let mut succeeded = Vec::new();
    let mut failed: Option<(ScannedStep, FailureCode)> = None;
    let mut committed = None;
    for record in records.into_iter().skip(1) {
        if committed.is_some() {
            return Err(RecoveryScanError::Corrupt);
        }

        match record {
            JournalRecord::Begin { .. } => return Err(RecoveryScanError::Corrupt),
            JournalRecord::StepStarted {
                sequence,
                address,
                action,
            } => {
                if failed.is_some() || sequence != next_sequence {
                    return Err(RecoveryScanError::Corrupt);
                }
                open.insert(
                    sequence,
                    ScannedStep {
                        sequence,
                        address,
                        action,
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
                succeeded.push(step);
            }
            JournalRecord::StepFailed {
                sequence,
                address,
                action,
                failure_code,
            } => {
                let step = take_matching_step(&mut open, sequence, &address, action)?;
                failed.get_or_insert((step, failure_code));
            }
            JournalRecord::Commit { final_serial } => {
                if !open.is_empty() || failed.is_some() || final_serial != journal_serial {
                    return Err(RecoveryScanError::Corrupt);
                }
                committed = Some(final_serial);
            }
        }
    }

    if let Some(final_serial) = committed {
        if trailing_tail || state.serial() < final_serial {
            return Err(RecoveryScanError::Corrupt);
        }

        return Ok(ScannedJournal::Committed);
    }
    if trailing_tail && failed.is_some() {
        return Err(RecoveryScanError::Corrupt);
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
        .map(|step| step.sequence);
    let uncertain_step = if success_without_checkpoint {
        succeeded.last().cloned().map(recovery_step)
    } else {
        open.values().next().cloned().map(recovery_step)
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

    Ok(ScannedJournal::Incomplete(RecoverySummary {
        operation_id: filename_operation_id,
        lineage,
        starting_serial,
        durable_serial,
        last_confirmed_sequence,
        uncertain_step,
        reason,
        truncated_tail: trailing_tail,
    }))
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
        (JournalAction::Create, None) | (JournalAction::Delete, Some(_)) => {
            Err(RecoveryScanError::Corrupt)
        }
        _ => Ok(()),
    }
}

fn recovery_step(step: ScannedStep) -> RecoveryStep {
    RecoveryStep {
        sequence: step.sequence,
        address: step.address,
        action: step.action,
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
