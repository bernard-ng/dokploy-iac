//! Journaled Schedule mutations.
//!
//! Every mutation input is fully validated before its journal step starts, so a
//! pre-mutation rejection can never leave an open step behind. A definitive
//! Dokploy rejection fails the step; an outcome-unknown result stays in
//! progress for recovery and is never retried. A Schedule is never run or
//! deployed by any operation here: execution is the sole concern of Dokploy's
//! own scheduler, and the live acceptance keeps every Schedule disabled.
//!
//! Command and script bytes only ever travel from the one-shot sensitive
//! sidecar into a zeroizing SDK input. An unmanaged command, or a script that
//! exists remotely but is not managed, can never be resent, so any write that
//! would need it is refused as unsupported instead of being guessed.

use dokploy_sdk::{
    CreateSchedule, ScheduleDetails, ScheduleId, ScheduleTarget, ShellType, UpdateSchedule,
};
use zeroize::Zeroizing;

use super::*;
use crate::remote::{schedule_sdk_target, shell_label, stored_schedule_target};

/// Creates one Schedule through the journaled create protocol.
pub(super) async fn execute_schedule_create(
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
    let input = schedule_create_input(compiled, change.address(), checkpoint, state)?;
    let placeholder = RemoteId::new("recovery-pending")
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Create,
        ExpectedCheckpoint::create(checkpoint.materialize(change.address(), placeholder)?)?,
    )?;
    let created = match client.schedules().create(input).await {
        Ok(created) => created,
        Err(error) => {
            let code = failure_code(&error);
            fail_if_definitive(journal, token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let remote_id = RemoteId::new(created.schedule_id().as_str())
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(token, Some(remote_id), state)?;

    Ok(())
}

/// Deletes one Schedule through the exact stored target.
pub(super) async fn execute_schedule_delete(
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
    // The typed target is proven before the journal step opens.
    let target = stored_sdk_target(change.address(), state)?;
    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Delete,
        ExpectedCheckpoint::remove(before.clone()),
    )?;
    if let Err(error) = client
        .schedules()
        .delete(ScheduleId::new(before.remote_id().as_str()), target)
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

/// Replaces one Schedule by deleting its old identity before creating the new one.
pub(super) async fn execute_schedule_replacement(
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
    // The replacement and the old target are proven constructible before the old
    // identity is removed.
    let input = schedule_create_input(compiled, change.address(), checkpoint, state)?;
    let old_target = stored_sdk_target(change.address(), state)?;
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
        .schedules()
        .delete(ScheduleId::new(before.remote_id().as_str()), old_target)
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
    let created = match client.schedules().create(input).await {
        Ok(created) => created,
        Err(error) => {
            let code = failure_code(&error);
            fail_if_definitive(journal, create_token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let remote_id = RemoteId::new(created.schedule_id().as_str())
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(create_token, Some(remote_id), state)?;

    Ok(())
}

/// Updates one Schedule in place after a fresh identity-checked read.
pub(super) async fn execute_schedule_update(
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
    // A target or Compose-service change is a replacement and can never reach an
    // in-place update.
    if change.fields().iter().any(|field| {
        matches!(
            field.key(),
            PropertyPath::Target | PropertyPath::ServiceName
        )
    }) {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    let (target, _) = checkpoint_sdk_target(checkpoint, state)?;
    let current = client
        .schedules()
        .get(ScheduleId::new(remote_id.as_str()))
        .await
        .map_err(|error| ApplyWorkspaceError::RemotePreparation {
            code: failure_code(&error),
        })?;
    if current.schedule_id.as_str() != remote_id.as_str() || current.target != target {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    let input = schedule_update_input(
        compiled,
        change,
        checkpoint,
        &current,
        &remote_id,
        target.clone(),
    )?;
    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Update,
        ExpectedCheckpoint::update(before, resource.clone())?,
    )?;
    if let Err(error) = client.schedules().update(input).await {
        let code = failure_code(&error);
        fail_if_definitive(journal, token, code)?;
        return Err(ApplyWorkspaceError::RemoteMutation { code });
    }
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(token, Some(remote_id), state)?;

    Ok(())
}

/// Builds the complete create request before any journal step is opened.
fn schedule_create_input(
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<CreateSchedule, ApplyWorkspaceError> {
    let (target, _) = checkpoint_sdk_target(checkpoint, state)?;
    let name = required_string(checkpoint, &PropertyPath::ScheduleName)?;
    let cron_expression = required_string(checkpoint, &PropertyPath::CronExpression)?;
    let shell_type = shell_from_label(&required_string(checkpoint, &PropertyPath::ShellType)?)?;
    let enabled = required_bool(checkpoint, &PropertyPath::Enabled)?;
    let description = optional_string(checkpoint, &PropertyPath::Description)?;
    let timezone = optional_string(checkpoint, &PropertyPath::Timezone)?;
    if name.is_empty()
        || cron_expression.is_empty()
        || description.as_deref().is_some_and(str::is_empty)
        || timezone.as_deref().is_some_and(str::is_empty)
    {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    // An unmanaged command is never invented as an empty command.
    if !matches!(
        checkpoint.property(&PropertyPath::Command),
        Some(CheckpointValueRef::Sensitive)
    ) {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let command = non_empty(take_sensitive_string(
        compiled,
        address,
        &PropertyPath::Command,
    )?)?;
    let script =
        take_optional_sensitive_string(compiled, address, checkpoint, &PropertyPath::Script)?
            .map(non_empty)
            .transpose()?;

    Ok(CreateSchedule::new(
        target,
        name,
        description,
        cron_expression,
        shell_type,
        command,
        script,
        enabled,
        timezone,
    ))
}

/// Builds the complete replacement update before any journal step is opened.
///
/// Each safe field comes from the checkpoint, except an ignored or unmanaged
/// field, which is preserved from the fresh identity-checked read. Executable
/// text can only be resent from the sensitive sidecar.
fn schedule_update_input(
    compiled: &mut crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    current: &ScheduleDetails,
    remote_id: &RemoteId,
    target: ScheduleTarget,
) -> Result<UpdateSchedule, ApplyWorkspaceError> {
    let preserved = change.preserved_paths();
    let managed =
        |path: &PropertyPath| !preserved.contains(path) && checkpoint.property(path).is_some();
    let text = |path: &PropertyPath, remote: &str| -> Result<String, ApplyWorkspaceError> {
        if managed(path) {
            required_string(checkpoint, path)
        } else {
            Ok(remote.to_owned())
        }
    };
    let optional_text = |path: &PropertyPath,
                         remote: Option<&String>|
     -> Result<Option<String>, ApplyWorkspaceError> {
        if managed(path) {
            optional_string(checkpoint, path)
        } else {
            Ok(remote.cloned())
        }
    };

    let name = text(&PropertyPath::ScheduleName, &current.name)?;
    let cron_expression = text(&PropertyPath::CronExpression, &current.cron_expression)?;
    let shell_type = if managed(&PropertyPath::ShellType) {
        shell_from_label(&required_string(checkpoint, &PropertyPath::ShellType)?)?
    } else {
        current.shell_type
    };
    let enabled = if managed(&PropertyPath::Enabled) {
        required_bool(checkpoint, &PropertyPath::Enabled)?
    } else {
        current.enabled
    };
    let description = optional_text(&PropertyPath::Description, current.description.as_ref())?;
    let timezone = optional_text(&PropertyPath::Timezone, current.timezone.as_ref())?;
    if name.is_empty()
        || cron_expression.is_empty()
        || description.as_deref().is_some_and(str::is_empty)
        || timezone.as_deref().is_some_and(str::is_empty)
    {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    // Executable text cannot be read back, so an update can resend only what the
    // configuration manages. Anything else would silently change or drop it.
    let command_managed = matches!(
        checkpoint.property(&PropertyPath::Command),
        Some(CheckpointValueRef::Sensitive)
    ) && !preserved.contains(&PropertyPath::Command);
    if !command_managed {
        return Err(ApplyWorkspaceError::UnsupportedChange);
    }
    let script_managed = matches!(
        checkpoint.property(&PropertyPath::Script),
        Some(CheckpointValueRef::Sensitive)
    ) && !preserved.contains(&PropertyPath::Script);
    if !script_managed && current.script_present {
        return Err(ApplyWorkspaceError::UnsupportedChange);
    }
    let command = non_empty(take_sensitive_string(
        compiled,
        change.address(),
        &PropertyPath::Command,
    )?)?;
    let script = if script_managed {
        Some(non_empty(take_sensitive_string(
            compiled,
            change.address(),
            &PropertyPath::Script,
        )?)?)
    } else {
        None
    };

    Ok(UpdateSchedule::new(
        ScheduleId::new(remote_id.as_str()),
        target,
        name,
        description,
        cron_expression,
        shell_type,
        command,
        script,
        enabled,
        timezone,
    ))
}

fn non_empty(value: Zeroizing<String>) -> Result<Zeroizing<String>, ApplyWorkspaceError> {
    if value.is_empty() {
        Err(ApplyWorkspaceError::InvalidCheckpoint)
    } else {
        Ok(value)
    }
}

fn shell_from_label(label: &str) -> Result<ShellType, ApplyWorkspaceError> {
    [ShellType::Bash, ShellType::Sh]
        .into_iter()
        .find(|shell| shell_label(*shell) == label)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)
}

/// Resolves the checkpoint's typed logical target and service to a physical SDK target.
fn checkpoint_sdk_target(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<(ScheduleTarget, ResourceAddress), ApplyWorkspaceError> {
    let target: ResourceAddress = required_string(checkpoint, &PropertyPath::Target)?
        .parse()
        .map_err(|_| ApplyWorkspaceError::InvalidCheckpoint)?;
    if !target.kind().is_schedule_target()
        || !checkpoint.dependencies().contains(&target)
        || checkpoint.containment().map(ResourceAddress::kind) != Some(ResourceKind::Environment)
    {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let service = optional_string(checkpoint, &PropertyPath::ServiceName)?;
    let stored = state
        .resource(&target)
        .filter(|stored| stored.kind() == target.kind())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let sdk_target = schedule_sdk_target(
        target.kind(),
        stored.remote_id().as_str(),
        service.as_deref(),
    )
    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;

    Ok((sdk_target, target))
}

/// Resolves the durable typed target of a stored Schedule to a physical SDK target.
fn stored_sdk_target(
    address: &ResourceAddress,
    state: &StateFile,
) -> Result<ScheduleTarget, ApplyWorkspaceError> {
    let (target, service) = stored_schedule_target(address, state)
        .map_err(|_| ApplyWorkspaceError::InvalidCheckpoint)?;
    let stored = state
        .resource(&target)
        .filter(|stored| stored.kind() == target.kind())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;

    schedule_sdk_target(
        target.kind(),
        stored.remote_id().as_str(),
        service.as_deref(),
    )
    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)
}
