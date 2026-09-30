//! Journaled execution of immutable reconciliation plans.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use dokploy_core::{
    ChangeKind, CheckpointMaterializationError, CheckpointValueRef, ConfigDigest, Plan,
    PropertyPath, StoredState,
};
use dokploy_sdk::{
    ApplicationId, CreateApplication, CreateDomain, CreateEnvironment, CreatePostgres,
    CreateProject, CreateRedis, Dokploy, DomainId, EnvironmentId, Error as SdkError, Nullable,
    PostgresId, ProjectId, RedisId, UpdateApplication, UpdateDomain, UpdateEnvironment,
    UpdatePostgres, UpdateProject, UpdateRedis,
};
use dokploy_state::{
    ExpectedState, FailureCode, InstanceIdentity, JournalAction, JournalError, ManagedInputs,
    OperationJournal, PlanDigest, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateError, StateFile, StateStore, StateStoreError,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

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
    let mut compiled =
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
    let mut default_environments = BTreeMap::new();

    for change in plan.changes() {
        if change.kind() != ChangeKind::Create {
            execute_existing_change(client, &mut compiled, change, &mut state, &mut journal)
                .await?;
            applied += 1;
            continue;
        }

        let token = journal.start_step(change.address().clone(), JournalAction::Create)?;
        let checkpoint = change
            .checkpoint()
            .present()
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
        let remote_id = match change.address().kind() {
            ResourceKind::Project => {
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
                let environment_id = RemoteId::new(created.default_environment_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
                default_environments.insert(
                    change.address().clone(),
                    DefaultEnvironment {
                        name: created.default_environment_name().to_owned(),
                        remote_id: environment_id,
                    },
                );
                remote_id
            }
            ResourceKind::Environment => {
                let parent = checkpoint
                    .containment()
                    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
                if default_environments
                    .get(parent)
                    .is_some_and(|default| default.name == change.address().name().as_str())
                {
                    let remote_id = default_environments
                        .remove(parent)
                        .expect("matching default environment exists")
                        .remote_id;
                    if let Some(description) = checkpoint.property(&PropertyPath::Description) {
                        let interim = minimal_resource_state(
                            ResourceKind::Environment,
                            checkpoint,
                            remote_id.clone(),
                        )?;
                        state.upsert_resource(change.address().clone(), interim)?;
                        journal.succeed(token, Some(remote_id.clone()), &state)?;

                        let update_token =
                            journal.start_step(change.address().clone(), JournalAction::Update)?;
                        let input = UpdateEnvironment::new(
                            EnvironmentId::new(remote_id.as_str()),
                            nullable_string(description)?,
                        );
                        if let Err(error) = client.environments().update(input).await {
                            let code = failure_code(&error);
                            journal.fail(update_token, code)?;
                            return Err(ApplyWorkspaceError::RemoteMutation { code });
                        }
                        let resource =
                            checkpoint.materialize(change.address(), remote_id.clone())?;
                        state.upsert_resource(change.address().clone(), resource)?;
                        journal.succeed(update_token, Some(remote_id), &state)?;
                        applied += 1;
                        continue;
                    }
                    remote_id
                } else {
                    let parent_id = state
                        .resource(parent)
                        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                        .remote_id();
                    let input = environment_create_input(
                        change.address(),
                        checkpoint,
                        ProjectId::new(parent_id.as_str()),
                    )?;
                    let created = match client.environments().create(input).await {
                        Ok(created) => created,
                        Err(error) => {
                            let code = failure_code(&error);
                            journal.fail(token, code)?;
                            return Err(ApplyWorkspaceError::RemoteMutation { code });
                        }
                    };
                    RemoteId::new(created.environment_id().as_str())
                        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
                }
            }
            ResourceKind::Application => {
                let parent = checkpoint
                    .containment()
                    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
                let parent_id = state
                    .resource(parent)
                    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                    .remote_id();
                let input = CreateApplication::new(
                    change.address().name().as_str(),
                    EnvironmentId::new(parent_id.as_str()),
                );
                let created = match client.applications().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        journal.fail(token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                let remote_id = RemoteId::new(created.application_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
                let property_paths = checkpoint
                    .property_paths()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                if property_paths.is_empty() {
                    remote_id
                } else {
                    let interim = minimal_resource_state(
                        ResourceKind::Application,
                        checkpoint,
                        remote_id.clone(),
                    )?;
                    state.upsert_resource(change.address().clone(), interim)?;
                    journal.succeed(token, Some(remote_id.clone()), &state)?;

                    let update = application_update_input(
                        &mut compiled,
                        change.address(),
                        checkpoint,
                        created.application_id().clone(),
                        &property_paths,
                        None,
                    )?;
                    let update_token =
                        journal.start_step(change.address().clone(), JournalAction::Update)?;
                    if let Err(error) = client.applications().update(update).await {
                        let code = failure_code(&error);
                        journal.fail(update_token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
                    state.upsert_resource(change.address().clone(), resource.clone())?;
                    journal.succeed(update_token, Some(remote_id.clone()), &state)?;

                    if application_requires_deploy(property_paths.iter()) {
                        let deploy_token =
                            journal.start_step(change.address().clone(), JournalAction::Deploy)?;
                        if let Err(error) = client
                            .applications()
                            .deploy(created.application_id().clone())
                            .await
                        {
                            let code = failure_code(&error);
                            journal.fail(deploy_token, code)?;
                            return Err(ApplyWorkspaceError::RemoteMutation { code });
                        }
                        state.upsert_resource(change.address().clone(), resource)?;
                        journal.succeed(deploy_token, Some(remote_id), &state)?;
                    }

                    applied += 1;
                    continue;
                }
            }
            ResourceKind::Postgres => {
                let environment_id = checkpoint_environment_id(checkpoint, &state)?;
                let database = required_string(checkpoint, &PropertyPath::Database)?;
                let username = required_string(checkpoint, &PropertyPath::Username)?;
                let password = take_sensitive_string(
                    &mut compiled,
                    change.address(),
                    &PropertyPath::Password,
                )?;
                let input = CreatePostgres::new(
                    change.address().name().as_str(),
                    environment_id,
                    database,
                    username,
                    password,
                );
                let created = match client.postgres().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        journal.fail(token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.postgres_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
            ResourceKind::Redis => {
                let environment_id = checkpoint_environment_id(checkpoint, &state)?;
                let password = take_sensitive_string(
                    &mut compiled,
                    change.address(),
                    &PropertyPath::Password,
                )?;
                let input =
                    CreateRedis::new(change.address().name().as_str(), environment_id, password);
                let created = match client.redis().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        journal.fail(token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.redis_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
            ResourceKind::Domain => {
                let application = compiled
                    .bindings()
                    .domain_application(change.address())
                    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
                let application_id = state
                    .resource(application)
                    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                    .remote_id();
                let host = required_string(checkpoint, &PropertyPath::Host)?;
                let input = CreateDomain::new(host, ApplicationId::new(application_id.as_str()));
                let created = match client.domains().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        journal.fail(token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.domain_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
        };
        let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
        state.upsert_resource(change.address().clone(), resource)?;
        journal.succeed(token, Some(remote_id), &state)?;
        applied += 1;
    }

    journal.commit()?;

    Ok(ApplySummary { applied })
}

fn preflight(plan: &Plan) -> Result<(), ApplyWorkspaceError> {
    if plan.changes().iter().all(|change| match change.kind() {
        ChangeKind::Create | ChangeKind::NoOp => {
            matches!(
                change.address().kind(),
                ResourceKind::Project
                    | ResourceKind::Environment
                    | ResourceKind::Application
                    | ResourceKind::Postgres
                    | ResourceKind::Redis
                    | ResourceKind::Domain
            )
        }
        ChangeKind::Update => true,
        ChangeKind::Reparent => change.address().kind() == ResourceKind::Application,
        ChangeKind::Replace | ChangeKind::Delete | ChangeKind::Move | ChangeKind::Forget => false,
    }) {
        Ok(())
    } else {
        Err(ApplyWorkspaceError::UnsupportedChange)
    }
}

fn checkpoint_environment_id(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<EnvironmentId, ApplyWorkspaceError> {
    let parent = checkpoint
        .containment()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let parent_id = state
        .resource(parent)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .remote_id();

    Ok(EnvironmentId::new(parent_id.as_str()))
}

fn required_string(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    path: &PropertyPath,
) -> Result<String, ApplyWorkspaceError> {
    match checkpoint.property(path) {
        Some(CheckpointValueRef::NonSensitive(value)) => value
            .as_str()
            .map(str::to_owned)
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint),
        None
        | Some(
            CheckpointValueRef::Null
            | CheckpointValueRef::EmptyCollection
            | CheckpointValueRef::Sensitive,
        ) => Err(ApplyWorkspaceError::InvalidCheckpoint),
    }
}

fn take_sensitive_string(
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    path: &PropertyPath,
) -> Result<Zeroizing<String>, ApplyWorkspaceError> {
    let (mut value, _) = compiled
        .take_sensitive(address, path)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .into_parts();
    let bytes = std::mem::take(&mut *value);

    match String::from_utf8(bytes) {
        Ok(value) => Ok(Zeroizing::new(value)),
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            Err(ApplyWorkspaceError::InvalidCheckpoint)
        }
    }
}

fn minimal_resource_state(
    kind: ResourceKind,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    remote_id: RemoteId,
) -> Result<ResourceState, ApplyWorkspaceError> {
    let empty = ManagedInputs::try_from_json(serde_json::json!({}))
        .expect("the empty managed input object is valid");

    ResourceState::try_new(
        kind,
        remote_id,
        checkpoint.protected(),
        empty,
        dokploy_state::SensitiveInputs::default(),
        checkpoint.containment().cloned(),
        checkpoint.dependencies().to_vec(),
    )
    .map_err(|_| ApplyWorkspaceError::InvalidCheckpoint)
}

fn application_update_input(
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    application_id: ApplicationId,
    selected_paths: &[PropertyPath],
    current_environment: Option<Zeroizing<String>>,
) -> Result<UpdateApplication, ApplyWorkspaceError> {
    let mut input = UpdateApplication::new(application_id);
    if selected_paths.contains(&PropertyPath::Description)
        && let Some(value) = checkpoint.property(&PropertyPath::Description)
    {
        input = input.with_description(nullable_string(value)?);
    }
    if selected_paths.contains(&PropertyPath::Replicas)
        && let Some(value) = checkpoint.property(&PropertyPath::Replicas)
    {
        let replicas = match value {
            CheckpointValueRef::Null => Nullable::Null,
            CheckpointValueRef::NonSensitive(value) => {
                let replicas = value
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
                Nullable::Value(replicas)
            }
            CheckpointValueRef::EmptyCollection | CheckpointValueRef::Sensitive => {
                return Err(ApplyWorkspaceError::InvalidCheckpoint);
            }
        };
        input = input.with_replicas(replicas);
    }
    let source_selected = selected_paths.iter().any(|path| {
        matches!(
            path,
            PropertyPath::Source | PropertyPath::SourceRepository | PropertyPath::SourceBranch
        )
    });
    if source_selected {
        if matches!(
            checkpoint.property(&PropertyPath::Source),
            Some(CheckpointValueRef::Null)
        ) {
            input = input.clearing_source();
        } else if let Some(repository) = checkpoint.property(&PropertyPath::SourceRepository) {
            let repository = required_checkpoint_string(repository)?;
            input = input.with_github_repository(repository);
            if let Some(branch) = checkpoint.property(&PropertyPath::SourceBranch) {
                input = input.with_branch(nullable_string(branch)?);
            }
        }
    }

    let environment_selected = selected_paths.iter().any(|path| {
        matches!(
            path,
            PropertyPath::Environment | PropertyPath::EnvironmentVariable(_)
        )
    });
    if environment_selected {
        let environment =
            application_environment(compiled, address, checkpoint, current_environment)?;
        input = input.with_environment(environment);
    }

    Ok(input)
}

fn nullable_string(value: CheckpointValueRef<'_>) -> Result<Nullable<String>, ApplyWorkspaceError> {
    match value {
        CheckpointValueRef::Null => Ok(Nullable::Null),
        CheckpointValueRef::NonSensitive(value) => Ok(Nullable::Value(
            value
                .as_str()
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                .to_owned(),
        )),
        CheckpointValueRef::EmptyCollection | CheckpointValueRef::Sensitive => {
            Err(ApplyWorkspaceError::InvalidCheckpoint)
        }
    }
}

fn required_checkpoint_string(
    value: CheckpointValueRef<'_>,
) -> Result<String, ApplyWorkspaceError> {
    match value {
        CheckpointValueRef::NonSensitive(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint),
        CheckpointValueRef::Null
        | CheckpointValueRef::EmptyCollection
        | CheckpointValueRef::Sensitive => Err(ApplyWorkspaceError::InvalidCheckpoint),
    }
}

fn application_environment(
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    current: Option<Zeroizing<String>>,
) -> Result<Nullable<Zeroizing<String>>, ApplyWorkspaceError> {
    match checkpoint.property(&PropertyPath::Environment) {
        Some(CheckpointValueRef::Null) => return Ok(Nullable::Null),
        Some(CheckpointValueRef::EmptyCollection) => {
            return Ok(Nullable::Value(Zeroizing::new(String::new())));
        }
        None => {}
        Some(CheckpointValueRef::NonSensitive(_) | CheckpointValueRef::Sensitive) => {
            return Err(ApplyWorkspaceError::InvalidCheckpoint);
        }
    }

    let managed_names = checkpoint
        .property_paths()
        .into_iter()
        .filter_map(|path| match path {
            PropertyPath::EnvironmentVariable(name) => Some(name.as_str().to_owned()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut document = Zeroizing::new(String::new());
    if let Some(current) = current {
        for line in current.split('\n') {
            let is_managed =
                environment_line_name(line).is_some_and(|name| managed_names.contains(name));
            if !is_managed {
                append_environment_line(&mut document, line);
            }
        }
    }

    for path in checkpoint.property_paths() {
        let PropertyPath::EnvironmentVariable(name) = path else {
            continue;
        };
        match checkpoint.property(path) {
            Some(CheckpointValueRef::Null) => {}
            Some(CheckpointValueRef::Sensitive) => {
                let value = take_sensitive_string(compiled, address, path)?;
                let encoded = encode_environment_value(&value);
                let mut line = Zeroizing::new(String::with_capacity(
                    name.as_str().len() + encoded.len() + 1,
                ));
                line.push_str(name.as_str());
                line.push('=');
                line.push_str(&encoded);
                append_environment_line(&mut document, &line);
            }
            None
            | Some(CheckpointValueRef::EmptyCollection | CheckpointValueRef::NonSensitive(_)) => {
                return Err(ApplyWorkspaceError::InvalidCheckpoint);
            }
        }
    }

    Ok(Nullable::Value(document))
}

fn environment_line_name(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (name, _) = line.split_once('=')?;
    let name = name.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }

    Some(name)
}

fn append_environment_line(document: &mut String, line: &str) {
    if !document.is_empty() {
        document.push('\n');
    }
    document.push_str(line);
}

fn encode_environment_value(value: &str) -> Zeroizing<String> {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b':' | b'-')
        })
    {
        return Zeroizing::new(value.to_owned());
    }

    let mut encoded = Zeroizing::new(String::with_capacity(value.len().saturating_add(2)));
    encoded.push('"');
    for character in value.chars() {
        match character {
            '\\' => encoded.push_str("\\\\"),
            '"' => encoded.push_str("\\\""),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            _ => encoded.push(character),
        }
    }
    encoded.push('"');
    encoded
}

fn application_requires_deploy<'a>(mut paths: impl Iterator<Item = &'a PropertyPath>) -> bool {
    paths.any(|path| {
        matches!(
            path,
            PropertyPath::Replicas
                | PropertyPath::Source
                | PropertyPath::SourceRepository
                | PropertyPath::SourceBranch
                | PropertyPath::Environment
                | PropertyPath::EnvironmentVariable(_)
        )
    })
}

async fn execute_existing_change(
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
    let remote_id = state
        .resource(change.address())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .remote_id()
        .clone();

    if change.kind() == ChangeKind::NoOp {
        let token = journal.start_step(change.address().clone(), JournalAction::Update)?;
        let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
        state.upsert_resource(change.address().clone(), resource)?;
        journal.succeed(token, Some(remote_id), state)?;
        return Ok(());
    }

    if !matches!(change.kind(), ChangeKind::Update | ChangeKind::Reparent) {
        return Err(ApplyWorkspaceError::UnsupportedChange);
    }

    let selected_paths = change
        .fields()
        .iter()
        .map(|field| field.key().clone())
        .collect::<Vec<_>>();
    let mutation = match change.address().kind() {
        ResourceKind::Project => {
            let description = checkpoint
                .property(&PropertyPath::Description)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)
                .and_then(nullable_string)?;
            ExistingMutation::Project(UpdateProject::new(
                ProjectId::new(remote_id.as_str()),
                description,
            ))
        }
        ResourceKind::Environment => {
            let description = checkpoint
                .property(&PropertyPath::Description)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)
                .and_then(nullable_string)?;
            ExistingMutation::Environment(UpdateEnvironment::new(
                EnvironmentId::new(remote_id.as_str()),
                description,
            ))
        }
        ResourceKind::Application => {
            let current_environment = if selected_paths.iter().any(|path| {
                matches!(
                    path,
                    PropertyPath::Environment | PropertyPath::EnvironmentVariable(_)
                )
            }) {
                Some(
                    client
                        .applications()
                        .environment(ApplicationId::new(remote_id.as_str()))
                        .await
                        .map_err(|error| ApplyWorkspaceError::RemotePreparation {
                            code: failure_code(&error),
                        })?
                        .into_document(),
                )
            } else {
                None
            };
            let mut input = application_update_input(
                compiled,
                change.address(),
                checkpoint,
                ApplicationId::new(remote_id.as_str()),
                &selected_paths,
                current_environment,
            )?;
            if change.kind() == ChangeKind::Reparent {
                input = input.with_environment_id(checkpoint_environment_id(checkpoint, state)?);
            }
            ExistingMutation::Application(input)
        }
        ResourceKind::Postgres => {
            let mut input = UpdatePostgres::new(PostgresId::new(remote_id.as_str()));
            for path in &selected_paths {
                match path {
                    PropertyPath::Database => {
                        input = input.with_database(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Username => {
                        input = input.with_username(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Password => {
                        input = input.with_password(take_sensitive_string(
                            compiled,
                            change.address(),
                            path,
                        )?);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }
            ExistingMutation::Postgres(input)
        }
        ResourceKind::Redis => {
            if selected_paths.as_slice() != [PropertyPath::Password] {
                return Err(ApplyWorkspaceError::InvalidCheckpoint);
            }
            let password =
                take_sensitive_string(compiled, change.address(), &PropertyPath::Password)?;
            ExistingMutation::Redis(UpdateRedis::new(RedisId::new(remote_id.as_str()), password))
        }
        ResourceKind::Domain => {
            let host = required_string(checkpoint, &PropertyPath::Host)?;
            ExistingMutation::Domain(UpdateDomain::new(DomainId::new(remote_id.as_str()), host))
        }
    };

    let token = journal.start_step(change.address().clone(), JournalAction::Update)?;
    if let Err(error) = mutation.execute(client).await {
        let code = failure_code(&error);
        journal.fail(token, code)?;
        return Err(ApplyWorkspaceError::RemoteMutation { code });
    }
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource.clone())?;
    journal.succeed(token, Some(remote_id.clone()), state)?;

    if change.address().kind() == ResourceKind::Application
        && application_requires_deploy(selected_paths.iter())
    {
        let token = journal.start_step(change.address().clone(), JournalAction::Deploy)?;
        if let Err(error) = client
            .applications()
            .deploy(ApplicationId::new(remote_id.as_str()))
            .await
        {
            let code = failure_code(&error);
            journal.fail(token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
        state.upsert_resource(change.address().clone(), resource)?;
        journal.succeed(token, Some(remote_id), state)?;
    }

    Ok(())
}

enum ExistingMutation {
    Project(UpdateProject),
    Environment(UpdateEnvironment),
    Application(UpdateApplication),
    Postgres(UpdatePostgres),
    Redis(UpdateRedis),
    Domain(UpdateDomain),
}

impl ExistingMutation {
    async fn execute(self, client: &Dokploy) -> Result<(), SdkError> {
        match self {
            Self::Project(input) => client.projects().update(input).await,
            Self::Environment(input) => client.environments().update(input).await,
            Self::Application(input) => client.applications().update(input).await,
            Self::Postgres(input) => client.postgres().update(input).await,
            Self::Redis(input) => client.redis().update(input).await,
            Self::Domain(input) => client.domains().update(input).await,
        }
    }
}

fn project_create_input(
    address: &ResourceAddress,
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

fn environment_create_input(
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    project_id: ProjectId,
) -> Result<CreateEnvironment, ApplyWorkspaceError> {
    let mut input = CreateEnvironment::new(address.name().as_str(), project_id);
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

struct DefaultEnvironment {
    name: String,
    remote_id: RemoteId,
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
    #[error("remote mutation preparation failed with {code:?}")]
    RemotePreparation { code: FailureCode },
    #[error("the operation journal failed")]
    Journal(#[from] JournalError),
    #[error("the planned checkpoint cannot become durable state")]
    Checkpoint(#[from] CheckpointMaterializationError),
    #[error("the next durable state is invalid")]
    State(#[from] StateError),
}
