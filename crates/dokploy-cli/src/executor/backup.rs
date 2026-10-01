//! Journaled database Backup mutations.
//!
//! Every mutation input is fully validated before its journal step starts, so a
//! pre-mutation rejection can never leave an open step behind. A definitive
//! Dokploy rejection fails the step; an outcome-unknown result stays in
//! progress for recovery and is never retried. Backups are only ever created,
//! updated, and removed as policies: nothing here triggers a backup run, tests a
//! destination, or deploys a service.

use std::num::NonZeroU32;

use dokploy_sdk::{BackupId, BackupTarget, CreateBackup, DestinationId, UpdateBackup};

use super::*;
use crate::remote::backup_target;

/// Creates one Backup through the journaled create protocol.
pub(super) async fn execute_backup_create(
    client: &Dokploy,
    compiled: &crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let input = backup_create_input(compiled, change.address(), checkpoint, state)?;
    let placeholder = RemoteId::new("recovery-pending")
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Create,
        ExpectedCheckpoint::create(checkpoint.materialize(change.address(), placeholder)?)?,
    )?;
    let created = match client.backups().create(input).await {
        Ok(created) => created,
        Err(error) => {
            let code = failure_code(&error);
            fail_if_definitive(journal, token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let remote_id = RemoteId::new(created.backup_id().as_str())
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(token, Some(remote_id), state)?;

    Ok(())
}

/// Replaces one Backup by deleting its old identity before creating the new one.
pub(super) async fn execute_backup_replacement(
    client: &Dokploy,
    compiled: &crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    // The replacement and the removal are both proven constructible before the old
    // identity is removed.
    let input = backup_create_input(compiled, change.address(), checkpoint, state)?;
    let before = state
        .resource(change.address())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    let old_target = stored_backup_target(&before, state)?;
    let delete_token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Delete,
        ExpectedCheckpoint::remove(before.clone()),
    )?;
    if let Err(error) = client
        .backups()
        .delete(BackupId::new(before.remote_id().as_str()), old_target)
        .await
        && !is_already_missing(&error)
    {
        let code = failure_code(&error);
        fail_if_definitive(journal, delete_token, code)?;
        return Err(ApplyWorkspaceError::RemoteMutation { code });
    }
    state.remove_resource(change.address())?;
    journal.succeed(delete_token, None, state)?;

    let placeholder = RemoteId::new("recovery-pending")
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let create_token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Create,
        ExpectedCheckpoint::create(checkpoint.materialize(change.address(), placeholder)?)?,
    )?;
    let created = match client.backups().create(input).await {
        Ok(created) => created,
        Err(error) => {
            let code = failure_code(&error);
            fail_if_definitive(journal, create_token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let remote_id = RemoteId::new(created.backup_id().as_str())
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(create_token, Some(remote_id), state)?;

    Ok(())
}

/// Removes one Backup after proving its stored database target.
pub(super) async fn execute_backup_delete(
    client: &Dokploy,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    if !change.checkpoint().is_absent() {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let before = state
        .resource(change.address())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    // The target proof is resolved before the journal step opens.
    let target = stored_backup_target(&before, state)?;
    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Delete,
        ExpectedCheckpoint::remove(before.clone()),
    )?;
    if let Err(error) = client
        .backups()
        .delete(BackupId::new(before.remote_id().as_str()), target)
        .await
        && !is_already_missing(&error)
    {
        let code = failure_code(&error);
        fail_if_definitive(journal, token, code)?;
        return Err(ApplyWorkspaceError::RemoteMutation { code });
    }
    state.remove_resource(change.address())?;
    journal.succeed(token, None, state)?;

    Ok(())
}

/// Updates one Backup in place after a fresh identity-checked read.
///
/// `backup.update` replaces every mutable field, so each field carries the
/// managed checkpoint value, and an ignored or unmanaged field carries the value
/// freshly read from Dokploy. The destination is always the freshly resolved
/// identity of the desired name selector.
pub(super) async fn execute_backup_update(
    client: &Dokploy,
    compiled: &crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let before = state
        .resource(change.address())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    let remote_id = before.remote_id().clone();
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    // A target change is a replacement: the freshly read Backup must already belong
    // to the checkpoint's physical database, or the update is refused.
    let (_, target) = backup_checkpoint_target(checkpoint, state)?;
    let current = client
        .backups()
        .get(BackupId::new(remote_id.as_str()))
        .await
        .map_err(|error| ApplyWorkspaceError::RemotePreparation {
            code: failure_code(&error),
        })?;
    if current.backup_id.as_str() != remote_id.as_str() || current.target != target {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    let preserved = change.preserved_paths();
    let schedule = if preserved.contains(&PropertyPath::Schedule) {
        current.schedule.clone()
    } else {
        required_string(checkpoint, &PropertyPath::Schedule)?
    };
    let enabled = if preserved.contains(&PropertyPath::Enabled) {
        // A null remote flag cannot be carried through the complete replacement.
        current
            .enabled
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
    } else {
        required_bool(checkpoint, &PropertyPath::Enabled)?
    };
    let include_encryption_key = if preserved.contains(&PropertyPath::IncludeEncryptionKey) {
        current.include_encryption_key
    } else {
        required_bool(checkpoint, &PropertyPath::IncludeEncryptionKey)?
    };
    let keep_latest = if preserved.contains(&PropertyPath::KeepLatest)
        || checkpoint.property(&PropertyPath::KeepLatest).is_none()
    {
        current.keep_latest_count
    } else {
        checkpoint_keep_latest(checkpoint)?
    };
    let input = UpdateBackup::new(
        BackupId::new(remote_id.as_str()),
        target,
        resolved_destination(compiled, change.address())?,
        schedule,
        enabled,
        required_string(checkpoint, &PropertyPath::Prefix)?,
        required_string(checkpoint, &PropertyPath::Database)?,
        keep_latest,
        include_encryption_key,
    );

    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Update,
        ExpectedCheckpoint::update(before, resource.clone())?,
    )?;
    if let Err(error) = client.backups().update(input).await {
        let code = failure_code(&error);
        fail_if_definitive(journal, token, code)?;
        return Err(ApplyWorkspaceError::RemoteMutation { code });
    }
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(token, Some(remote_id), state)?;

    Ok(())
}

/// Builds the complete create request before any journal step is opened.
fn backup_create_input(
    compiled: &crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<CreateBackup, ApplyWorkspaceError> {
    let (_, target) = backup_checkpoint_target(checkpoint, state)?;
    let schedule = required_string(checkpoint, &PropertyPath::Schedule)?;
    let prefix = required_string(checkpoint, &PropertyPath::Prefix)?;
    let database = required_string(checkpoint, &PropertyPath::Database)?;
    if schedule.is_empty() || prefix.is_empty() || database.is_empty() {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    Ok(CreateBackup::new(
        target,
        resolved_destination(compiled, address)?,
        schedule,
        required_bool(checkpoint, &PropertyPath::Enabled)?,
        prefix,
        database,
        checkpoint_keep_latest(checkpoint)?,
        required_bool(checkpoint, &PropertyPath::IncludeEncryptionKey)?,
    ))
}

fn checkpoint_keep_latest(
    checkpoint: &dokploy_core::ResourceCheckpoint,
) -> Result<Option<NonZeroU32>, ApplyWorkspaceError> {
    match checkpoint.property(&PropertyPath::KeepLatest) {
        None | Some(CheckpointValueRef::Null) => Ok(None),
        Some(CheckpointValueRef::NonSensitive(value)) => value
            .as_u64()
            .and_then(|count| u32::try_from(count).ok())
            .and_then(NonZeroU32::new)
            .map(Some)
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint),
        Some(CheckpointValueRef::EmptyCollection | CheckpointValueRef::Sensitive) => {
            Err(ApplyWorkspaceError::InvalidCheckpoint)
        }
    }
}

/// Returns the freshly resolved physical destination identity for one Backup.
fn resolved_destination(
    compiled: &crate::desired::CompiledDesired,
    address: &ResourceAddress,
) -> Result<DestinationId, ApplyWorkspaceError> {
    match compiled
        .bindings()
        .external_id(address, &PropertyPath::Destination)
    {
        Some(ExternalExecutionId::Remote(remote_id)) => Ok(DestinationId::new(remote_id.as_str())),
        Some(ExternalExecutionId::Local) | None => {
            Err(ApplyWorkspaceError::ExternalResolutionMissing)
        }
    }
}

/// Resolves the checkpoint's typed logical target to its stored physical identity.
fn backup_checkpoint_target(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<(ResourceAddress, BackupTarget), ApplyWorkspaceError> {
    let target: ResourceAddress = required_string(checkpoint, &PropertyPath::Target)?
        .parse()
        .map_err(|_| ApplyWorkspaceError::InvalidCheckpoint)?;
    if !target.kind().is_backup_target()
        || !checkpoint.dependencies().contains(&target)
        || checkpoint.containment().map(ResourceAddress::kind) != Some(ResourceKind::Environment)
    {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let stored = state
        .resource(&target)
        .filter(|stored| stored.kind() == target.kind())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let service = backup_target(target.kind(), stored.remote_id().as_str())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;

    Ok((target, service))
}

/// Resolves a stored Backup's recorded target to its stored physical identity.
fn stored_backup_target(
    before: &ResourceState,
    state: &StateFile,
) -> Result<BackupTarget, ApplyWorkspaceError> {
    let target: ResourceAddress = stored_target_text(before)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .parse()
        .map_err(|_| ApplyWorkspaceError::InvalidCheckpoint)?;
    if !target.kind().is_backup_target() || !before.dependencies().contains(&target) {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let stored = state
        .resource(&target)
        .filter(|stored| stored.kind() == target.kind())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;

    backup_target(target.kind(), stored.remote_id().as_str())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)
}

fn stored_target_text(before: &ResourceState) -> Option<String> {
    before
        .last_applied()
        .as_json()
        .get("target")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}
