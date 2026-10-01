//! Journaled Mount mutations.
//!
//! Every mutation input is fully validated before its journal step starts, so a
//! pre-mutation rejection can never leave an open step behind. A definitive
//! Dokploy rejection fails the step; an outcome-unknown result stays in
//! progress for recovery and is never retried. Mounts never trigger a
//! deployment.

use dokploy_sdk::{CreateMount, MountId, ServiceTarget, UpdateMount};

use super::*;
use crate::remote::{mount_service_target, mount_type_label};

/// Creates one Mount through the journaled create protocol.
pub(super) async fn execute_mount_create(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let input = mount_create_input(compiled, change.address(), checkpoint, state)?;
    let placeholder = RemoteId::new("recovery-pending")
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Create,
        ExpectedCheckpoint::create(checkpoint.materialize(change.address(), placeholder)?)?,
    )?;
    let created = match client.mounts().create(input).await {
        Ok(created) => created,
        Err(error) => {
            let code = failure_code(&error);
            fail_if_definitive(journal, token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let remote_id = RemoteId::new(created.mount_id().as_str())
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(token, Some(remote_id), state)?;

    Ok(())
}

/// Replaces one Mount by deleting its old identity before creating the new one.
pub(super) async fn execute_mount_replacement(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    // The replacement is proven constructible before the old identity is removed.
    let input = mount_create_input(compiled, change.address(), checkpoint, state)?;
    let before = state
        .resource(change.address())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    let delete_token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Delete,
        ExpectedCheckpoint::remove(before.clone()),
    )?;
    if let Err(error) = client
        .mounts()
        .delete(MountId::new(before.remote_id().as_str()))
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
    let created = match client.mounts().create(input).await {
        Ok(created) => created,
        Err(error) => {
            let code = failure_code(&error);
            fail_if_definitive(journal, create_token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let remote_id = RemoteId::new(created.mount_id().as_str())
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(create_token, Some(remote_id), state)?;

    Ok(())
}

/// Updates one Mount in place after a fresh identity-checked read.
pub(super) async fn execute_mount_update(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
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
    let selected_paths = change
        .fields()
        .iter()
        .map(|field| field.key().clone())
        .collect::<Vec<_>>();
    // A storage-type change is a replacement and can never reach an in-place update.
    if selected_paths.contains(&PropertyPath::MountType) {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    let (_, target) = mount_checkpoint_target(checkpoint, state)?;
    let mount_type = required_string(checkpoint, &PropertyPath::MountType)?;
    let current = client
        .mounts()
        .get(MountId::new(remote_id.as_str()))
        .await
        .map_err(|error| ApplyWorkspaceError::RemotePreparation {
            code: failure_code(&error),
        })?;
    if current.mount_id.as_str() != remote_id.as_str()
        || current.target != target
        || mount_type_label(current.mount_type) != mount_type
    {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    let mut input = UpdateMount::new(MountId::new(remote_id.as_str()));
    let mut has_fields = false;
    if let Some(mount_path) = optional_string(checkpoint, &PropertyPath::MountPath)? {
        input = input.with_mount_path(mount_path);
        has_fields = true;
    }
    match mount_type.as_str() {
        "bind" => {
            if let Some(host_path) = optional_string(checkpoint, &PropertyPath::HostPath)? {
                input = input.with_bind(host_path);
                has_fields = true;
            }
        }
        "volume" => {
            if let Some(volume_name) = optional_string(checkpoint, &PropertyPath::VolumeName)? {
                input = input.with_volume(volume_name);
                has_fields = true;
            }
        }
        "file" => match checkpoint.property(&PropertyPath::FileContent) {
            Some(CheckpointValueRef::Sensitive) => {
                let file_path = required_string(checkpoint, &PropertyPath::FilePath)?;
                let content =
                    take_sensitive_string(compiled, change.address(), &PropertyPath::FileContent)?;
                input = input.with_file(file_path, content);
                has_fields = true;
            }
            None if !selected_paths.contains(&PropertyPath::FilePath) => {}
            // Unmanaged content cannot be resent, so a file-path change is unsafe.
            None | Some(_) => return Err(ApplyWorkspaceError::InvalidCheckpoint),
        },
        _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
    }
    if !has_fields {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Update,
        ExpectedCheckpoint::update(before, resource.clone())?,
    )?;
    if let Err(error) = client.mounts().update(input).await {
        let code = failure_code(&error);
        fail_if_definitive(journal, token, code)?;
        return Err(ApplyWorkspaceError::RemoteMutation { code });
    }
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(token, Some(remote_id), state)?;

    Ok(())
}

/// Builds the complete create request before any journal step is opened.
fn mount_create_input(
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<CreateMount, ApplyWorkspaceError> {
    let (_, target) = mount_checkpoint_target(checkpoint, state)?;
    let mount_path = required_string(checkpoint, &PropertyPath::MountPath)?;

    match required_string(checkpoint, &PropertyPath::MountType)?.as_str() {
        "bind" => Ok(CreateMount::bind(
            target,
            required_string(checkpoint, &PropertyPath::HostPath)?,
            mount_path,
        )),
        "volume" => Ok(CreateMount::volume(
            target,
            required_string(checkpoint, &PropertyPath::VolumeName)?,
            mount_path,
        )),
        "file" => {
            let file_path = required_string(checkpoint, &PropertyPath::FilePath)?;
            // Unmanaged content is never invented as an empty file.
            if !matches!(
                checkpoint.property(&PropertyPath::FileContent),
                Some(CheckpointValueRef::Sensitive)
            ) {
                return Err(ApplyWorkspaceError::InvalidCheckpoint);
            }
            let content = take_sensitive_string(compiled, address, &PropertyPath::FileContent)?;
            Ok(CreateMount::file(target, file_path, mount_path, content))
        }
        _ => Err(ApplyWorkspaceError::InvalidCheckpoint),
    }
}

/// Resolves the checkpoint's typed logical target to its stored physical identity.
fn mount_checkpoint_target(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<(ResourceAddress, ServiceTarget), ApplyWorkspaceError> {
    let target: ResourceAddress = required_string(checkpoint, &PropertyPath::Target)?
        .parse()
        .map_err(|_| ApplyWorkspaceError::InvalidCheckpoint)?;
    if !target.kind().is_mount_target()
        || !checkpoint.dependencies().contains(&target)
        || checkpoint.containment().map(ResourceAddress::kind) != Some(ResourceKind::Environment)
    {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let stored = state
        .resource(&target)
        .filter(|stored| stored.kind() == target.kind())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let service = mount_service_target(target.kind(), stored.remote_id().as_str())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;

    Ok((target, service))
}
