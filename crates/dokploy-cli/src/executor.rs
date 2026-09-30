//! Journaled execution of immutable reconciliation plans.

use std::fs;
use std::path::{Path, PathBuf};

use dokploy_core::{
    ChangeKind, CheckpointMaterializationError, CheckpointValueRef, ConfigDigest, Plan, StoredState,
};
use dokploy_sdk::{CreateProject, Dokploy, Error as SdkError};
use dokploy_state::{
    ExpectedState, FailureCode, InstanceIdentity, JournalAction, JournalError, OperationJournal,
    PlanDigest, RemoteId, StateError, StateFile, StateStore, StateStoreError,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::desired::{CompileDesiredError, compile_desired_for_instance};
use crate::remote::{DiscoverRemoteError, DiscoveryAuthority, discover_remote};

/// Outcome of a completed apply operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplySummary {
    applied: usize,
}

impl ApplySummary {
    /// Returns the number of planned changes checkpointed successfully.
    #[must_use]
    pub const fn applied(self) -> usize {
        self.applied
    }
}

/// Builds and executes one fresh plan while holding the workspace writer lock.
pub async fn apply_workspace(
    client: &Dokploy,
    config_file: &Path,
) -> Result<ApplySummary, ApplyWorkspaceError> {
    let workspace = canonical_workspace(config_file)?;
    let instance = InstanceIdentity::parse(client.base_url().as_str())?;
    let store = StateStore::new(&workspace, instance.clone())?;
    let mut session = store.begin_write()?;
    let loaded = dokploy_config::load_with_digest(config_file)?;
    let source_digest = ConfigDigest::parse(hex_digest(loaded.source_sha256))
        .expect("a SHA-256 digest is canonical lowercase hexadecimal");
    let compiled =
        compile_desired_for_instance(&loaded.config, source_digest, instance.clone(), &workspace)?;
    let durable = store.inspect()?;
    let mut state = durable.clone().unwrap_or_else(|| {
        StateFile::new(
            env!("CARGO_PKG_VERSION")
                .parse()
                .expect("crate version is valid semver"),
            instance,
        )
    });
    let stored = StoredState::try_from_state(&state)?;
    let remote = discover_remote(
        client,
        &compiled,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await?;
    let plan = dokploy_core::plan(compiled.desired_state(), &stored, &remote);

    if !plan.complete() || !plan.applyable() {
        return Err(ApplyWorkspaceError::PlanBlocked);
    }
    preflight(&plan)?;
    if plan.changes().is_empty() {
        return Ok(ApplySummary { applied: 0 });
    }

    if durable.is_none() {
        session.checkpoint(ExpectedState::absent(), &state)?;
    }

    let digest = plan_digest(&plan);
    let mut journal = OperationJournal::begin(&mut session, digest)?;
    let mut applied = 0;

    for change in plan.changes() {
        let token = journal.start_step(change.address().clone(), JournalAction::Create)?;
        let checkpoint = change
            .checkpoint()
            .present()
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
        let input = project_create_input(change.address(), checkpoint)?;
        let created = match client.projects().create(input).await {
            Ok(created) => created,
            Err(error) => {
                let code = failure_code(&error);
                journal.fail(token, code)?;
                return Err(ApplyWorkspaceError::RemoteMutation { code });
            }
        };
        let remote_id = RemoteId::new(created.project_id().as_str())
            .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
        let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
        state.upsert_resource(change.address().clone(), resource)?;
        journal.succeed(token, Some(remote_id), &state)?;
        applied += 1;
    }

    journal.commit()?;

    Ok(ApplySummary { applied })
}

fn preflight(plan: &Plan) -> Result<(), ApplyWorkspaceError> {
    if plan.changes().iter().all(|change| {
        change.kind() == ChangeKind::Create
            && change.address().kind() == dokploy_state::ResourceKind::Project
    }) {
        Ok(())
    } else {
        Err(ApplyWorkspaceError::UnsupportedChange)
    }
}

fn project_create_input(
    address: &dokploy_state::ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
) -> Result<CreateProject, ApplyWorkspaceError> {
    let mut input = CreateProject::new(address.name().as_str());
    match checkpoint.property(&dokploy_core::PropertyPath::Description) {
        None | Some(CheckpointValueRef::Null) => {}
        Some(CheckpointValueRef::NonSensitive(value)) => {
            let description = value
                .as_str()
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
            input = input.with_description(description);
        }
        Some(CheckpointValueRef::EmptyCollection | CheckpointValueRef::Sensitive) => {
            return Err(ApplyWorkspaceError::InvalidCheckpoint);
        }
    }

    Ok(input)
}

fn plan_digest(plan: &Plan) -> PlanDigest {
    let bytes = Sha256::digest(plan.to_json_bytes());
    PlanDigest::parse(hex_digest(bytes.into()))
        .expect("a SHA-256 digest is canonical lowercase hexadecimal")
}

fn canonical_workspace(config_file: &Path) -> Result<PathBuf, ApplyWorkspaceError> {
    let parent = config_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::canonicalize(parent).map_err(|source| ApplyWorkspaceError::Workspace { source })
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

fn failure_code(error: &SdkError) -> FailureCode {
    match error {
        SdkError::Api(error) if matches!(error.status(), 401 | 403) => FailureCode::Unauthorized,
        SdkError::Api(error) if matches!(error.status(), 400 | 422) => FailureCode::Validation,
        SdkError::Api(_) => FailureCode::RemoteRejected,
        SdkError::OutcomeUnknown { .. }
        | SdkError::Decode { .. }
        | SdkError::UnexpectedResponse { .. } => FailureCode::TransportOutcomeUnknown,
        SdkError::InvalidRequest { .. } => FailureCode::Validation,
        SdkError::Request { .. } => FailureCode::Internal,
    }
}

/// A redaction-safe apply failure.
#[derive(Debug, Error)]
pub enum ApplyWorkspaceError {
    #[error("failed to load the declarative configuration")]
    Config(#[from] dokploy_config::ConfigFileError),
    #[error("failed to resolve the configuration workspace")]
    Workspace {
        #[source]
        source: std::io::Error,
    },
    #[error("the Dokploy client URL is not a valid instance identity")]
    Instance(#[from] dokploy_state::InstanceIdentityError),
    #[error("failed to access durable workspace state")]
    StateStore(#[from] StateStoreError),
    #[error("desired configuration cannot be compiled safely")]
    Desired(#[from] CompileDesiredError),
    #[error("durable state cannot be projected into planner input")]
    StoredState(#[from] dokploy_core::StoredStateError),
    #[error("fresh Dokploy state cannot be discovered safely")]
    Remote(#[from] DiscoverRemoteError),
    #[error("the fresh plan is incomplete or blocked")]
    PlanBlocked,
    #[error("the plan contains a change unsupported by this executor checkpoint")]
    UnsupportedChange,
    #[error("the planned checkpoint is invalid for execution")]
    InvalidCheckpoint,
    #[error("Dokploy returned an invalid physical identity")]
    InvalidRemoteIdentity,
    #[error("the remote mutation failed with {code:?}")]
    RemoteMutation { code: FailureCode },
    #[error("the operation journal failed")]
    Journal(#[from] JournalError),
    #[error("the planned checkpoint cannot become durable state")]
    Checkpoint(#[from] CheckpointMaterializationError),
    #[error("the next durable state is invalid")]
    State(#[from] StateError),
}
