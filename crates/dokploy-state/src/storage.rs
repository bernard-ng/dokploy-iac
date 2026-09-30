use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

use fs4::{FileExt, TryLockError};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::journal::{RecoveryScanError, scan_recovery};
use crate::{InstanceIdentity, RecoveryStatus, StateFile, StateRevision};

const DEFAULT_MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_MAX_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_MAX_JOURNAL_RECORDS: usize = 10_000;
const STATE_DIRECTORY: &str = ".dokploy";
const STATE_FILE: &str = "state.json";
const BACKUP_FILE: &str = "state.backup.json";
const LOCK_FILE: &str = "state.lock";

/// The snapshot identity a caller observed before preparing a checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedState(ExpectedStateKind);

#[derive(Clone, Debug, Eq, PartialEq)]
enum ExpectedStateKind {
    Absent,
    Present {
        instance: InstanceIdentity,
        revision: StateRevision,
    },
}

impl ExpectedState {
    /// Expects that no state lineage has been initialized.
    #[must_use]
    pub const fn absent() -> Self {
        Self(ExpectedStateKind::Absent)
    }

    /// Captures the instance, lineage, and serial of an observed snapshot.
    #[must_use]
    pub fn from_state(state: &StateFile) -> Self {
        Self(ExpectedStateKind::Present {
            instance: state.instance().clone(),
            revision: state.revision(),
        })
    }
}

/// The local durable-state module rooted at a workspace directory.
///
/// The module rejects symlinks and revalidates canonical paths and, on Unix,
/// directory device/inode identity. Standard path-based filesystem APIs still
/// leave a narrow time-of-check/time-of-use window. Eliminating it requires a
/// future handle-relative `openat`/no-follow implementation.
#[derive(Clone, Debug)]
pub struct StateStore {
    workspace: PathBuf,
    state_directory: PathBuf,
    instance: InstanceIdentity,
    max_state_bytes: u64,
    max_journal_bytes: u64,
    max_journal_records: usize,
}

impl StateStore {
    /// Binds state below an existing canonical workspace to one instance.
    pub fn new(
        workspace: impl AsRef<Path>,
        instance: InstanceIdentity,
    ) -> Result<Self, StateStoreError> {
        Self::with_max_state_bytes(workspace, instance, DEFAULT_MAX_STATE_BYTES)
    }

    /// Binds state with an explicit defensive read limit.
    pub fn with_max_state_bytes(
        workspace: impl AsRef<Path>,
        instance: InstanceIdentity,
        max_state_bytes: u64,
    ) -> Result<Self, StateStoreError> {
        let workspace = fs::canonicalize(workspace.as_ref())
            .map_err(|source| StateStoreError::io("canonicalize workspace", source))?;
        if !workspace.is_dir() {
            return Err(StateStoreError::WorkspaceNotDirectory);
        }

        let store = Self {
            state_directory: workspace.join(STATE_DIRECTORY),
            workspace,
            instance,
            max_state_bytes,
            max_journal_bytes: DEFAULT_MAX_JOURNAL_BYTES,
            max_journal_records: DEFAULT_MAX_JOURNAL_RECORDS,
        };
        store.verified_state_directory()?;

        Ok(store)
    }

    /// Reads one atomic state snapshot without taking the writer lock.
    pub fn inspect(&self) -> Result<Option<StateFile>, StateStoreError> {
        self.load_current()
            .map(|loaded| loaded.map(|item| item.state))
    }

    /// Acquires the fail-fast exclusive writer lock.
    pub fn begin_write(&self) -> Result<WriteSession<'_>, StateStoreError> {
        let session = self.acquire_write_session()?;
        match scan_recovery(self).map_err(StateStoreError::from_recovery_scan)? {
            RecoveryStatus::Clean => Ok(session),
            RecoveryStatus::RecoveryRequired(summary) => {
                Err(StateStoreError::RecoveryRequired { summary })
            }
        }
    }

    pub(crate) fn begin_recovery_write(&self) -> Result<WriteSession<'_>, StateStoreError> {
        self.acquire_write_session()
    }

    fn acquire_write_session(&self) -> Result<WriteSession<'_>, StateStoreError> {
        let state_directory = self.ensure_state_directory()?;
        self.harden_existing_artifacts()?;

        let lock_path = self.state_directory.join(LOCK_FILE);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600);
        let lock = options
            .open(&lock_path)
            .map_err(|source| StateStoreError::io("open state lock", source))?;
        harden_file_permissions(&lock)
            .map_err(|source| StateStoreError::io("harden state lock", source))?;

        match FileExt::try_lock(&lock) {
            Ok(()) => Ok(WriteSession {
                store: self,
                lock,
                state_directory,
                journal_pending: false,
            }),
            Err(TryLockError::WouldBlock) => Err(StateStoreError::LockContended),
            Err(TryLockError::Error(source)) => {
                Err(StateStoreError::io("acquire state lock", source))
            }
        }
    }

    fn load_current(&self) -> Result<Option<LoadedState>, StateStoreError> {
        if self.verified_state_directory()?.is_none() {
            return Ok(None);
        }
        self.verify_existing_artifacts()?;

        let state_path = self.state_directory.join(STATE_FILE);
        let backup_path = self.state_directory.join(BACKUP_FILE);
        let bytes = match read_bounded(&state_path, self.max_state_bytes)? {
            Some(bytes) => bytes,
            None => {
                if entry_exists(&backup_path)
                    .map_err(|source| StateStoreError::io("inspect state backup", source))?
                {
                    return Err(StateStoreError::OrphanBackup);
                }

                return Ok(None);
            }
        };

        let state =
            StateFile::from_json_slice(&bytes).map_err(|_| StateStoreError::StateCorrupt)?;
        state
            .ensure_instance(&self.instance)
            .map_err(|_| StateStoreError::StateInstanceMismatch)?;

        Ok(Some(LoadedState { state, bytes }))
    }

    fn state_path(&self) -> PathBuf {
        self.state_directory.join(STATE_FILE)
    }

    pub(crate) fn state_directory_path(&self) -> &Path {
        &self.state_directory
    }

    pub(crate) fn journal_limits(&self) -> (u64, usize) {
        (self.max_journal_bytes, self.max_journal_records)
    }

    pub(crate) fn current_state_for_journal(&self) -> Result<Option<StateFile>, StateStoreError> {
        self.load_current()
            .map(|loaded| loaded.map(|item| item.state))
    }

    fn ensure_state_directory(&self) -> Result<DirectoryIdentity, StateStoreError> {
        let created = match self.verified_state_directory()? {
            Some(path) => return self.harden_state_directory(path),
            None => match fs::create_dir(&self.state_directory) {
                Ok(()) => true,
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => false,
                Err(source) => {
                    return Err(StateStoreError::io("create state directory", source));
                }
            },
        };

        let path = self
            .verified_state_directory()?
            .ok_or(StateStoreError::UnsafeStateDirectory)?;
        let path = self.harden_state_directory(path)?;
        if created {
            sync_directory(&self.workspace)
                .map_err(|source| StateStoreError::io("sync workspace directory", source))?;
        }

        Ok(path)
    }

    fn verified_state_directory(&self) -> Result<Option<DirectoryIdentity>, StateStoreError> {
        let metadata = match fs::symlink_metadata(&self.state_directory) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StateStoreError::io("inspect state directory", source));
            }
        };

        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StateStoreError::UnsafeStateDirectory);
        }

        let canonical = fs::canonicalize(&self.state_directory)
            .map_err(|source| StateStoreError::io("canonicalize state directory", source))?;
        if canonical != self.state_directory {
            return Err(StateStoreError::UnsafeStateDirectory);
        }

        Ok(Some(DirectoryIdentity::new(canonical, &metadata)))
    }

    fn harden_state_directory(
        &self,
        identity: DirectoryIdentity,
    ) -> Result<DirectoryIdentity, StateStoreError> {
        harden_directory_permissions(&identity.path)
            .map_err(|source| StateStoreError::io("harden state directory", source))?;

        Ok(identity)
    }

    fn verify_existing_artifacts(&self) -> Result<(), StateStoreError> {
        for name in [STATE_FILE, BACKUP_FILE, LOCK_FILE] {
            let path = self.state_directory.join(name);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(StateStoreError::io("inspect state artifact", source));
                }
            };

            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(StateStoreError::UnsafeStateArtifact);
            }
        }

        Ok(())
    }

    fn harden_existing_artifacts(&self) -> Result<(), StateStoreError> {
        self.verify_existing_artifacts()?;

        for name in [STATE_FILE, BACKUP_FILE, LOCK_FILE] {
            let path = self.state_directory.join(name);
            if !entry_exists(&path)
                .map_err(|source| StateStoreError::io("inspect state artifact", source))?
            {
                continue;
            }

            harden_path_permissions(&path)
                .map_err(|source| StateStoreError::io("harden state artifact", source))?;
        }

        Ok(())
    }
}

/// An exclusive state writer. Dropping it releases the advisory lock.
pub struct WriteSession<'store> {
    store: &'store StateStore,
    lock: File,
    state_directory: DirectoryIdentity,
    journal_pending: bool,
}

impl WriteSession<'_> {
    /// Persists a checked next snapshot atomically.
    pub fn checkpoint(
        &mut self,
        expected: ExpectedState,
        proposed: &StateFile,
    ) -> Result<(), StateStoreError> {
        if self.journal_pending {
            return Err(StateStoreError::JournalOperationPending);
        }

        self.checkpoint_internal(expected, proposed)
    }

    pub(crate) fn checkpoint_from_journal(
        &mut self,
        expected: ExpectedState,
        proposed: &StateFile,
    ) -> Result<(), StateStoreError> {
        debug_assert!(self.journal_pending);
        self.checkpoint_internal(expected, proposed)
    }

    fn checkpoint_internal(
        &mut self,
        expected: ExpectedState,
        proposed: &StateFile,
    ) -> Result<(), StateStoreError> {
        self.revalidate_state_directory()?;
        self.store.verify_existing_artifacts()?;

        let current = self.store.load_current()?;

        match (&expected.0, current.as_ref()) {
            (ExpectedStateKind::Absent, None) => {
                proposed
                    .ensure_instance(&self.store.instance)
                    .map_err(|_| StateStoreError::ProposedInstanceMismatch)?;
                if proposed.serial() != 0 {
                    return Err(StateStoreError::InvalidInitialSerial {
                        found: proposed.serial(),
                    });
                }
            }
            (ExpectedStateKind::Absent, Some(_)) => {
                return Err(StateStoreError::StateAlreadyExists);
            }
            (ExpectedStateKind::Present { .. }, None) => {
                return Err(StateStoreError::StateMissing);
            }
            (ExpectedStateKind::Present { instance, revision }, Some(current)) => {
                validate_expected(instance, *revision, &current.state)?;
                validate_proposed(&current.state, proposed)?;
            }
        }

        let bytes = serialize_state(proposed)?;
        if bytes.len() as u64 > self.store.max_state_bytes {
            return Err(StateStoreError::ProposedStateTooLarge {
                max_bytes: self.store.max_state_bytes,
            });
        }

        if let Some(current) = current {
            let backup_path = self.store.state_directory.join(BACKUP_FILE);
            write_atomically(&backup_path, &current.bytes).map_err(|error| {
                map_atomic_write_error(error, PersistenceTarget::Backup, current.state.revision())
            })?;
        }

        write_atomically(&self.store.state_path(), &bytes).map_err(|error| {
            map_atomic_write_error(error, PersistenceTarget::Primary, proposed.revision())
        })
    }

    pub(crate) fn current_state(&self) -> Result<StateFile, StateStoreError> {
        self.store
            .current_state_for_journal()?
            .ok_or(StateStoreError::StateMissing)
    }

    pub(crate) fn state_directory(&self) -> &Path {
        &self.state_directory.path
    }

    pub(crate) fn journal_limits(&self) -> (u64, usize) {
        self.store.journal_limits()
    }

    pub(crate) fn reserve_journal(&mut self) -> Result<(), StateStoreError> {
        if self.journal_pending {
            return Err(StateStoreError::JournalOperationPending);
        }
        self.journal_pending = true;

        Ok(())
    }

    pub(crate) fn revalidate_state_directory(&self) -> Result<(), StateStoreError> {
        let state_directory = self
            .store
            .verified_state_directory()?
            .ok_or(StateStoreError::UnsafeStateDirectory)?;
        if state_directory != self.state_directory {
            return Err(StateStoreError::UnsafeStateDirectory);
        }

        Ok(())
    }

    pub(crate) fn complete_journal(&mut self) {
        debug_assert!(self.journal_pending);
        self.journal_pending = false;
    }

    pub(crate) fn assert_current_revision(
        &self,
        expected: StateRevision,
    ) -> Result<(), StateStoreError> {
        self.revalidate_state_directory()?;
        self.store.verify_existing_artifacts()?;
        let current = self
            .store
            .load_current()?
            .ok_or(StateStoreError::StateMissing)?;
        if current.state.revision() != expected {
            return Err(StateStoreError::StaleState {
                expected,
                actual: current.state.revision(),
            });
        }

        Ok(())
    }
}

impl Drop for WriteSession<'_> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
    }
}

struct LoadedState {
    state: StateFile,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl DirectoryIdentity {
    fn new(path: PathBuf, metadata: &fs::Metadata) -> Self {
        Self {
            path,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

fn validate_expected(
    expected_instance: &InstanceIdentity,
    expected_revision: StateRevision,
    current: &StateFile,
) -> Result<(), StateStoreError> {
    if current.instance() != expected_instance {
        return Err(StateStoreError::ExpectedInstanceMismatch);
    }

    if current.revision() != expected_revision {
        return Err(StateStoreError::StaleState {
            expected: expected_revision,
            actual: current.revision(),
        });
    }

    Ok(())
}

fn validate_proposed(current: &StateFile, proposed: &StateFile) -> Result<(), StateStoreError> {
    if proposed.instance() != current.instance() {
        return Err(StateStoreError::ProposedInstanceMismatch);
    }

    if proposed.lineage() != current.lineage() {
        return Err(StateStoreError::ProposedLineageMismatch);
    }

    let expected_serial = current
        .serial()
        .checked_add(1)
        .ok_or(StateStoreError::SerialOverflow)?;
    if proposed.serial() != expected_serial {
        return Err(StateStoreError::InvalidSerialTransition {
            expected: expected_serial,
            found: proposed.serial(),
        });
    }

    Ok(())
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, StateStoreError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            if entry_exists(path)
                .map_err(|source| StateStoreError::io("inspect state snapshot", source))?
            {
                return Err(StateStoreError::StateCorrupt);
            }

            return Ok(None);
        }
        Err(source) => return Err(StateStoreError::io("open state snapshot", source)),
    };

    let length = file
        .metadata()
        .map_err(|source| StateStoreError::io("inspect state snapshot", source))?
        .len();
    if length > max_bytes {
        return Err(StateStoreError::StateTooLarge { max_bytes });
    }

    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| StateStoreError::io("read state snapshot", source))?;
    if bytes.len() as u64 > max_bytes {
        return Err(StateStoreError::StateTooLarge { max_bytes });
    }

    Ok(Some(bytes))
}

fn entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(source),
    }
}

fn serialize_state(state: &StateFile) -> Result<Vec<u8>, StateStoreError> {
    let mut bytes = serde_json::to_vec_pretty(state).map_err(|_| StateStoreError::Serialization)?;
    bytes.push(b'\n');

    Ok(bytes)
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), AtomicWriteError> {
    write_atomically_with_sync(path, bytes, sync_directory)
}

fn write_atomically_with_sync(
    path: &Path,
    bytes: &[u8],
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), AtomicWriteError> {
    let parent = path.parent().ok_or_else(|| {
        AtomicWriteError::BeforePersist(io::Error::new(
            io::ErrorKind::InvalidInput,
            "state path has no parent directory",
        ))
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(AtomicWriteError::BeforePersist)?;
    harden_path_permissions(temporary.path()).map_err(AtomicWriteError::BeforePersist)?;
    temporary
        .as_file_mut()
        .write_all(bytes)
        .map_err(AtomicWriteError::BeforePersist)?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(AtomicWriteError::BeforePersist)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AtomicWriteError::BeforePersist)?;
    temporary
        .persist(path)
        .map_err(|error| AtomicWriteError::BeforePersist(error.error))?;
    sync_parent(parent).map_err(|source| AtomicWriteError::AfterPersist {
        stage: DurabilityStage::ParentDirectorySync,
        source,
    })
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    match File::open(path)?.sync_all() {
        Ok(()) => Ok(()),
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        Err(source) => Err(source),
    }
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn harden_directory_permissions(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
pub(crate) fn harden_directory_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn harden_path_permissions(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub(crate) fn harden_path_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn harden_file_permissions(file: &File) -> io::Result<()> {
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn harden_file_permissions(_file: &File) -> io::Result<()> {
    Ok(())
}

enum AtomicWriteError {
    BeforePersist(io::Error),
    AfterPersist {
        stage: DurabilityStage,
        source: io::Error,
    },
}

fn map_atomic_write_error(
    error: AtomicWriteError,
    target: PersistenceTarget,
    revision: StateRevision,
) -> StateStoreError {
    match error {
        AtomicWriteError::BeforePersist(source) => {
            StateStoreError::io("write atomic state file", source)
        }
        AtomicWriteError::AfterPersist { stage, source } => {
            StateStoreError::DurabilityOutcomeUnknown {
                target,
                revision,
                stage,
                source,
            }
        }
    }
}

/// The durable artifact whose post-persist outcome is uncertain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistenceTarget {
    Backup,
    Primary,
}

/// The durability operation that failed after an atomic persist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurabilityStage {
    ParentDirectorySync,
}

/// A safe persistence or optimistic-concurrency failure.
#[derive(Debug, Error)]
pub enum StateStoreError {
    #[error("another process holds the state write lock")]
    LockContended,
    #[error("state mutation requires recovery for operation {summary:?}")]
    RecoveryRequired {
        summary: Box<crate::RecoverySummary>,
    },
    #[error("operation journal is corrupt")]
    JournalCorrupt,
    #[error("multiple incomplete operation journals require manual recovery")]
    RecoveryAmbiguous,
    #[error("the write session has an active or recovery-pending operation journal")]
    JournalOperationPending,
    #[error("state backup exists without a primary state file; recovery is required")]
    OrphanBackup,
    #[error("workspace path is not a directory")]
    WorkspaceNotDirectory,
    #[error("state directory is not a trusted canonical directory")]
    UnsafeStateDirectory,
    #[error("state artifact is not a regular file")]
    UnsafeStateArtifact,
    #[error("state file exceeds the configured {max_bytes}-byte limit")]
    StateTooLarge { max_bytes: u64 },
    #[error("proposed state exceeds the configured {max_bytes}-byte limit")]
    ProposedStateTooLarge { max_bytes: u64 },
    #[error("state file is malformed or violates state invariants")]
    StateCorrupt,
    #[error("state serialization failed")]
    Serialization,
    #[error("state was initialized by another writer")]
    StateAlreadyExists,
    #[error("expected state is missing")]
    StateMissing,
    #[error("the observed state belongs to another Dokploy instance")]
    StateInstanceMismatch,
    #[error("the caller's expected state belongs to another Dokploy instance")]
    ExpectedInstanceMismatch,
    #[error("state changed after it was inspected: expected {expected:?}, found {actual:?}")]
    StaleState {
        expected: StateRevision,
        actual: StateRevision,
    },
    #[error("proposed state belongs to another Dokploy instance")]
    ProposedInstanceMismatch,
    #[error("proposed state belongs to another lineage")]
    ProposedLineageMismatch,
    #[error("initial state must have serial 0, found {found}")]
    InvalidInitialSerial { found: u64 },
    #[error("proposed state must have serial {expected}, found {found}")]
    InvalidSerialTransition { expected: u64, found: u64 },
    #[error("state serial cannot advance beyond its maximum value")]
    SerialOverflow,
    #[error(
        "{target:?} state revision {revision:?} was persisted but {stage:?} failed; durability is unknown"
    )]
    DurabilityOutcomeUnknown {
        target: PersistenceTarget,
        revision: StateRevision,
        stage: DurabilityStage,
        #[source]
        source: io::Error,
    },
    #[error("could not {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

impl StateStoreError {
    fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }

    pub(crate) fn from_recovery_scan(error: RecoveryScanError) -> Self {
        match error {
            RecoveryScanError::Corrupt => Self::JournalCorrupt,
            RecoveryScanError::Ambiguous => Self::RecoveryAmbiguous,
            RecoveryScanError::Io(source) => Self::io("scan operation journals", source),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use semver::Version;
    use tempfile::tempdir;

    use crate::{InstanceIdentity, StateFile};

    use super::{
        AtomicWriteError, DurabilityStage, PersistenceTarget, StateStoreError,
        map_atomic_write_error, write_atomically_with_sync,
    };

    #[test]
    fn reports_post_persist_sync_failure_as_outcome_unknown_stage() {
        let directory = tempdir().expect("temporary directory must be created");
        let path = directory.path().join("state.json");

        let error = write_atomically_with_sync(&path, b"new bytes\n", |_| {
            Err(io::Error::other("injected parent sync failure"))
        })
        .expect_err("the injected sync must fail");

        assert!(matches!(
            &error,
            AtomicWriteError::AfterPersist {
                stage: DurabilityStage::ParentDirectorySync,
                ..
            }
        ));
        let revision = StateFile::new(
            Version::new(0, 1, 0),
            InstanceIdentity::parse("https://deploy.example.com").expect("instance must be valid"),
        )
        .revision();
        assert!(matches!(
            map_atomic_write_error(error, PersistenceTarget::Primary, revision),
            StateStoreError::DurabilityOutcomeUnknown {
                target: PersistenceTarget::Primary,
                revision: actual_revision,
                stage: DurabilityStage::ParentDirectorySync,
                ..
            } if actual_revision == revision
        ));
        let backup_error = AtomicWriteError::AfterPersist {
            stage: DurabilityStage::ParentDirectorySync,
            source: io::Error::other("injected backup sync failure"),
        };
        assert!(matches!(
            map_atomic_write_error(backup_error, PersistenceTarget::Backup, revision),
            StateStoreError::DurabilityOutcomeUnknown {
                target: PersistenceTarget::Backup,
                revision: actual_revision,
                stage: DurabilityStage::ParentDirectorySync,
                ..
            } if actual_revision == revision
        ));
        assert_eq!(
            std::fs::read(path).expect("persisted bytes must remain visible"),
            b"new bytes\n"
        );
    }
}
