//! Create-time server placement and placement replacement for Compose and databases.
//!
//! Dokploy accepts a service's server only on its create endpoint, so a changed
//! placement is converged by deleting the service and creating it again on the
//! newly selected server. The replacement is journaled as two recoverable steps,
//! refused while durable state holds a resource contained by or depending on the
//! service, and fully prepared (placement, secrets, scope) before the destructive
//! step so a stale selector or missing secret can never strand the workspace.
//! Nothing here deploys, starts, or contacts a server.

use dokploy_core::Plan;

use super::*;

/// Returns whether Dokploy lets a create call of this kind place the service.
pub(super) const fn is_placed_service(kind: ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::Compose
            | ResourceKind::Postgres
            | ResourceKind::MySql
            | ResourceKind::MariaDb
            | ResourceKind::Mongo
            | ResourceKind::LibSql
            | ResourceKind::Redis
    )
}

/// Returns the resolved create-time placement when the checkpoint manages `server`.
///
/// An unmanaged placement returns `None` so the create omits `serverId`; the
/// local selector is an explicit null and a named selector is the freshly
/// resolved identity held only in the execution sidecar.
pub(super) fn requested_placement(
    compiled: &crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
) -> Result<Option<ServerPlacement>, ApplyWorkspaceError> {
    if checkpoint.property(&PropertyPath::Server).is_none() {
        return Ok(None);
    }
    server_placement(compiled, address).map(Some)
}

/// Proves every managed placement was resolved before the first remote mutation.
///
/// A selector that is ignored by lifecycle rules is neither resolved nor read, so
/// a create or replacement that would need it is refused rather than guessed.
pub(super) fn preflight_placements(
    plan: &Plan,
    compiled: &crate::desired::CompiledDesired,
) -> Result<(), ApplyWorkspaceError> {
    for change in plan.changes().iter().filter(|change| {
        matches!(change.kind(), ChangeKind::Create | ChangeKind::Replace)
            && is_placed_service(change.address().kind())
    }) {
        if let Some(checkpoint) = change.checkpoint().present() {
            requested_placement(compiled, change.address(), checkpoint)?;
        }
    }

    Ok(())
}

/// Builds the Compose create input, including its managed placement.
pub(super) fn compose_create_input(
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<CreateCompose, ApplyWorkspaceError> {
    let environment_id = checkpoint_environment_id(checkpoint, state)?;
    let placement = requested_placement(compiled, address, checkpoint)?;
    let document = take_sensitive_string(compiled, address, &PropertyPath::ComposeDocument)?;
    let mut input = CreateCompose::new(address.name().as_str(), environment_id, document);
    if let Some(description) = optional_string(checkpoint, &PropertyPath::Description)? {
        input = input.with_description(description);
    }
    if let Some(placement) = placement {
        input = input.with_server_placement(placement);
    }

    Ok(input)
}

/// Replaces a Compose or database whose create-only server placement changed.
pub(super) async fn execute_service_replacement(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    let address = change.address();
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    if service_has_dependents(state, address) {
        return Err(ApplyWorkspaceError::ReplacementBlockedByDependents);
    }
    let create = prepare_service_create(compiled, change, state)?;
    let before = state
        .resource(address)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    let delete_token = journal.start_recoverable_step(
        address.clone(),
        JournalAction::Delete,
        ExpectedCheckpoint::remove(before.clone()),
    )?;
    let Some(deleted) = delete_remote_resource(client, before.kind(), before.remote_id()).await
    else {
        return Err(ApplyWorkspaceError::UnsupportedChange);
    };
    if let Err(error) = deleted
        && !is_already_missing(&error)
    {
        let code = failure_code(&error);
        fail_if_definitive(journal, delete_token, code)?;
        return Err(ApplyWorkspaceError::RemoteMutation { code });
    }
    state.remove_resource(address)?;
    journal.succeed(delete_token, None, state)?;

    let placeholder = RemoteId::new("recovery-pending")
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let create_token = journal.start_recoverable_step(
        address.clone(),
        JournalAction::Create,
        ExpectedCheckpoint::create(checkpoint.materialize(address, placeholder)?)?,
    )?;
    let remote_id = match create.execute(client).await {
        Ok(remote_id) => remote_id,
        Err(failure) => {
            if failure.definitive {
                journal.fail(create_token, failure.code)?;
            }
            return Err(ApplyWorkspaceError::RemoteMutation { code: failure.code });
        }
    };
    let resource = checkpoint.materialize(address, remote_id.clone())?;
    state.upsert_resource(address.clone(), resource)?;
    journal.succeed(create_token, Some(remote_id), state)?;

    Ok(())
}

/// Returns whether durable state holds a resource contained by or depending on `service`.
///
/// Deleting a service removes its mounts, schedules, backups, and ports in
/// Dokploy while the planner does not cascade a replacement to them, so any such
/// dependent blocks the replacement before the first remote mutation.
fn service_has_dependents(state: &StateFile, service: &ResourceAddress) -> bool {
    application_has_dependents(state, service)
}

/// A create call that is fully built and ready to run after the delete step.
enum ServiceCreate {
    Compose(CreateCompose),
    Database(DatabaseMutation),
}

struct ServiceCreateFailure {
    code: FailureCode,
    definitive: bool,
}

impl ServiceCreate {
    async fn execute(self, client: &Dokploy) -> Result<RemoteId, ServiceCreateFailure> {
        match self {
            Self::Compose(input) => {
                let created = client.composes().create(input).await.map_err(|error| {
                    let code = failure_code(&error);
                    ServiceCreateFailure {
                        code,
                        definitive: code != FailureCode::TransportOutcomeUnknown,
                    }
                })?;
                RemoteId::new(created.compose_id().as_str()).map_err(|_| ServiceCreateFailure {
                    code: FailureCode::Internal,
                    definitive: false,
                })
            }
            Self::Database(mutation) => {
                mutation
                    .execute(client)
                    .await
                    .map_err(|failure| ServiceCreateFailure {
                        code: failure.code(),
                        definitive: failure.is_definitive(),
                    })
            }
        }
    }
}

fn prepare_service_create(
    compiled: &mut crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &StateFile,
) -> Result<ServiceCreate, ApplyWorkspaceError> {
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    match change.address().kind() {
        ResourceKind::Compose => {
            compose_create_input(compiled, change.address(), checkpoint, state)
                .map(ServiceCreate::Compose)
        }
        kind if is_placed_service(kind) => prepare_database_mutation(compiled, change, state)
            .map(|prepared| ServiceCreate::Database(prepared.mutation)),
        _ => Err(ApplyWorkspaceError::UnsupportedChange),
    }
}
