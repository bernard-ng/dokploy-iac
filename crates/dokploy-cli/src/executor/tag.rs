//! Journaled tag mutations and project tag association.
//!
//! Every mutation input is fully validated, and every name is resolved, before
//! its journal step starts, so a pre-mutation rejection can never leave an open
//! step behind. A definitive Dokploy rejection fails the step; an
//! outcome-unknown result stays in progress for recovery and is never retried.
//! Tags are only ever created, renamed, recolored, associated, and removed:
//! nothing here deploys or restarts a service.

use std::collections::BTreeSet;

use dokploy_sdk::{CreateTag, ResponseField, TagId, UpdateTag};

use super::*;

/// Creates one tag through the journaled create protocol.
pub(super) async fn execute_tag_create(
    client: &Dokploy,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let input = CreateTag::new(
        required_string(checkpoint, &PropertyPath::Name)?,
        optional_color(checkpoint)?,
    );
    let placeholder = RemoteId::new("recovery-pending")
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Create,
        ExpectedCheckpoint::create(checkpoint.materialize(change.address(), placeholder)?)?,
    )?;
    let created = match client.tags().create(input).await {
        Ok(created) => created,
        Err(error) => {
            let code = failure_code(&error);
            fail_if_definitive(journal, token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let remote_id = RemoteId::new(created.tag_id().as_str())
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(token, Some(remote_id), state)?;

    Ok(())
}

/// Builds the update for exactly the tag fields the plan selected.
pub(super) fn tag_update_input(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    remote_id: &RemoteId,
    selected_paths: &[PropertyPath],
) -> Result<UpdateTag, ApplyWorkspaceError> {
    let mut input = UpdateTag::new(TagId::new(remote_id.as_str()));
    for path in selected_paths {
        match path {
            PropertyPath::Name => input = input.name(required_string(checkpoint, path)?),
            PropertyPath::Color => input = input.color(required_string(checkpoint, path)?),
            _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
        }
    }

    Ok(input)
}

/// The color a tag is created with; `null` is unrepresentable for a color.
fn optional_color(
    checkpoint: &dokploy_core::ResourceCheckpoint,
) -> Result<Option<String>, ApplyWorkspaceError> {
    match checkpoint.property(&PropertyPath::Color) {
        None => Ok(None),
        Some(_) => required_string(checkpoint, &PropertyPath::Color).map(Some),
    }
}

/// The association changes that converge one project onto its desired tag names.
pub(super) struct ProjectTagChanges {
    project_id: ProjectId,
    remove: Vec<TagId>,
    assign: Vec<TagId>,
}

impl ProjectTagChanges {
    /// Resolves the desired names and the current association from fresh reads.
    ///
    /// A name that no longer matches exactly one tag fails here, before any
    /// mutation, so nothing is half-applied.
    pub(super) async fn prepare(
        client: &Dokploy,
        project_id: ProjectId,
        checkpoint: &dokploy_core::ResourceCheckpoint,
    ) -> Result<Self, ApplyWorkspaceError> {
        let desired = match checkpoint.property(&PropertyPath::Tags) {
            Some(CheckpointValueRef::NonSensitive(value)) => value
                .as_array()
                .and_then(|names| {
                    names
                        .iter()
                        .map(|name| name.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?,
            _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
        };
        let preparation = |error: &SdkError| ApplyWorkspaceError::RemotePreparation {
            code: failure_code(error),
        };
        let tags = client.tags().all().await.map_err(|e| preparation(&e))?;
        let project = client
            .projects()
            .get(project_id.clone())
            .await
            .map_err(|e| preparation(&e))?;
        let ResponseField::Value(current) = project.tags else {
            // Without a conclusive current association a diff would be a guess.
            return Err(ApplyWorkspaceError::UnresolvedProjectTag);
        };

        let mut wanted = BTreeSet::new();
        for name in &desired {
            let tag = tags
                .tags()
                .iter()
                .find(|tag| &tag.name == name)
                .ok_or(ApplyWorkspaceError::UnresolvedProjectTag)?;
            wanted.insert(tag.tag_id.clone());
        }
        let current = current
            .into_iter()
            .map(|tag| tag.tag_id)
            .collect::<BTreeSet<_>>();

        Ok(Self {
            project_id,
            remove: current.difference(&wanted).cloned().collect(),
            assign: wanted.difference(&current).cloned().collect(),
        })
    }

    /// Removes stale associations, then adds the missing ones, in identity order.
    pub(super) async fn execute(self, client: &Dokploy) -> Result<(), SdkError> {
        for tag_id in self.remove {
            client
                .tags()
                .remove_from_project(self.project_id.clone(), tag_id)
                .await?;
        }
        for tag_id in self.assign {
            client
                .tags()
                .assign_to_project(self.project_id.clone(), tag_id)
                .await?;
        }

        Ok(())
    }
}
