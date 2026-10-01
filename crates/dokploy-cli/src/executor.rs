//! Journaled execution of immutable reconciliation plans.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use dokploy_core::{
    ChangeKind, CheckpointMaterializationError, CheckpointValueRef, ConfigDigest, MoveAction, Plan,
    PropertyPath, ReplacementOrder, StoredState,
};
use dokploy_sdk::{
    ApplicationId, ChangeLibSqlPassword, ComposeId, ComposeVolumePolicy, CreateApplication,
    CreateCompose, CreateDomain, CreateEnvironment, CreateLibSql, CreateMariaDb, CreateMongo,
    CreateMySql, CreatePort, CreatePostgres, CreateProject, CreateRedirect, CreateRedis,
    CreateSecurity, Dokploy, DomainId, EnvironmentId, Error as SdkError, LibSqlId, LibSqlNode,
    MariaDbId, MongoId, MySqlId, Nullable, PortId, PortProtocol, PostgresId, ProjectId,
    PublishMode, RedirectId, RedisId, SecurityId, UpdateApplication, UpdateCompose, UpdateDomain,
    UpdateEnvironment, UpdateLibSql, UpdateMariaDb, UpdateMongo, UpdateMySql, UpdatePort,
    UpdatePostgres, UpdateProject, UpdateRedirect, UpdateRedis, UpdateSecurity,
};
use dokploy_state::{
    ExpectedCheckpoint, ExpectedCheckpointError, ExpectedState, FailureCode, InstanceIdentity,
    JournalAction, JournalError, ManagedInputs, OperationJournal, PlanDigest, RemoteId,
    ResourceAddress, ResourceKind, ResourceState, StateError, StateFile, StateStore,
    StateStoreError, StepToken,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

use crate::desired::{CompileDesiredError, compile_desired_for_instance};
use crate::remote::{DiscoverRemoteError, DiscoveryAuthority, discover_remote};
use crate::saved_plan::{SavedPlan, SavedPlanError};
use crate::sensitive::SensitiveFingerprinter;

/// Outcome of a completed apply operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplySummary {
    applied: usize,
}

/// Bounded execution settings selected by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplyOptions {
    parallelism: usize,
}

impl ApplyOptions {
    /// Validates an explicit executor concurrency limit.
    pub fn new(parallelism: usize) -> Result<Self, ApplyWorkspaceError> {
        if !(1..=64).contains(&parallelism) {
            return Err(ApplyWorkspaceError::InvalidParallelism);
        }

        Ok(Self { parallelism })
    }

    /// Returns the maximum number of remote mutations allowed in flight.
    #[must_use]
    pub const fn parallelism(self) -> usize {
        self.parallelism
    }
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self { parallelism: 4 }
    }
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
    apply_workspace_with_approval(client, config_file, ApplyOptions::default(), |_| Ok(true)).await
}

/// Builds and executes a dependent-first deletion plan for every tracked resource.
pub async fn destroy_workspace_with_approval(
    client: &Dokploy,
    config_file: &Path,
    approval: impl FnOnce(&Plan) -> io::Result<bool>,
) -> Result<ApplySummary, ApplyWorkspaceError> {
    let workspace = canonical_workspace(config_file)?;
    let instance = InstanceIdentity::parse(client.base_url().as_str())?;
    let store = StateStore::new(&workspace, instance.clone())?;
    let digest = ConfigDigest::parse(hex_digest(Sha256::digest(b"dokploy-destroy-all-v1").into()))
        .expect("a SHA-256 digest is canonical lowercase hexadecimal");

    if store.inspect()?.is_none() {
        let state = StateFile::new(
            env!("CARGO_PKG_VERSION")
                .parse()
                .expect("crate version is valid semver"),
            instance.clone(),
        );
        let compiled = crate::desired::CompiledDesired::destroy_all(&state, digest)?;
        let stored = StoredState::absent(instance.clone());
        let remote = dokploy_core::RemoteState::try_new(
            instance,
            std::iter::empty::<(ResourceAddress, dokploy_core::RemoteObservation)>(),
        )
        .expect("an empty remote snapshot is valid");
        let plan = dokploy_core::plan(compiled.desired_state(), &stored, &remote);
        if !approval(&plan).map_err(|source| ApplyWorkspaceError::Approval { source })? {
            return Err(ApplyWorkspaceError::Declined);
        }

        return Ok(ApplySummary { applied: 0 });
    }

    let mut session = store.begin_write()?;
    let mut state = store
        .inspect()?
        .ok_or(ApplyWorkspaceError::StateDisappeared)?;
    let compiled = crate::desired::CompiledDesired::destroy_all(&state, digest)?;
    let stored = StoredState::try_from_state(&state)?;
    let remote = discover_remote(
        client,
        &compiled,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await?;
    let plan = dokploy_core::plan(compiled.desired_state(), &stored, &remote);

    if !approval(&plan).map_err(|source| ApplyWorkspaceError::Approval { source })? {
        return Err(ApplyWorkspaceError::Declined);
    }
    if !plan.complete() || !plan.applyable() {
        return Err(ApplyWorkspaceError::PlanBlocked);
    }
    preflight(&plan)?;
    if plan.changes().is_empty() {
        return Ok(ApplySummary { applied: 0 });
    }
    if plan
        .changes()
        .iter()
        .any(|change| change.kind() != ChangeKind::Delete)
    {
        return Err(ApplyWorkspaceError::UnsupportedChange);
    }

    let digest = plan_digest(&plan);
    let mut journal = OperationJournal::begin(&mut session, digest)?;
    let mut applied = 0;
    for change in plan.changes() {
        execute_removal_change(client, change, &mut state, &mut journal).await?;
        applied += 1;
    }
    journal.commit()?;

    Ok(ApplySummary { applied })
}

/// Builds, presents, approves, and executes one fresh plan under the writer lock.
pub async fn apply_workspace_with_approval(
    client: &Dokploy,
    config_file: &Path,
    options: ApplyOptions,
    approval: impl FnOnce(&Plan) -> io::Result<bool>,
) -> Result<ApplySummary, ApplyWorkspaceError> {
    apply_workspace_with_expectation(client, config_file, options, None, approval).await
}

/// Rebuilds and executes a saved plan only when every bound input remains fresh.
pub async fn apply_saved_plan_with_approval(
    client: &Dokploy,
    config_file: &Path,
    options: ApplyOptions,
    saved_plan: &SavedPlan,
    approval: impl FnOnce(&Plan) -> io::Result<bool>,
) -> Result<ApplySummary, ApplyWorkspaceError> {
    apply_workspace_with_expectation(client, config_file, options, Some(saved_plan), approval).await
}

async fn apply_workspace_with_expectation(
    client: &Dokploy,
    config_file: &Path,
    options: ApplyOptions,
    saved_plan: Option<&SavedPlan>,
    approval: impl FnOnce(&Plan) -> io::Result<bool>,
) -> Result<ApplySummary, ApplyWorkspaceError> {
    let parallelism = options.parallelism();
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
            instance.clone(),
        )
    });
    let stored = match durable.as_ref() {
        Some(state) => StoredState::try_from_state(state)?,
        None => StoredState::absent(instance.clone()),
    };
    let remote = discover_remote(
        client,
        &compiled,
        &state,
        DiscoveryAuthority::reconciliation(),
    )
    .await?;
    let plan = dokploy_core::plan(compiled.desired_state(), &stored, &remote);

    if let Some(saved_plan) = saved_plan {
        let fingerprinter = SensitiveFingerprinter::load(instance.clone())
            .map_err(|_| ApplyWorkspaceError::SavedPlanBindingKey)?;
        let remote_receipt = fingerprinter.remote_binding_receipt(&remote);
        saved_plan.verify_fresh(&instance, &plan, remote_receipt)?;
    }

    if !approval(&plan).map_err(|source| ApplyWorkspaceError::Approval { source })? {
        return Err(ApplyWorkspaceError::Declined);
    }
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
    let mut default_environments = BTreeMap::<ResourceAddress, DefaultEnvironment>::new();

    let mut change_index = 0;
    while change_index < plan.changes().len() {
        let database_batch = database_batch_len(&plan.changes()[change_index..], parallelism);
        if database_batch > 0 {
            match execute_database_batch(
                client,
                &mut compiled,
                &plan.changes()[change_index..change_index + database_batch],
                &mut state,
                &mut journal,
            )
            .await
            {
                Ok(checkpointed) => {
                    applied += checkpointed;
                    change_index += database_batch;
                    continue;
                }
                Err(error) => return Err(error),
            }
        }

        let change = &plan.changes()[change_index];
        if change.kind() == ChangeKind::Replace {
            execute_delete_before_create_replacement(
                client,
                &mut compiled,
                change,
                &mut state,
                &mut journal,
            )
            .await?;
            applied += 1;
            change_index += 1;
            continue;
        }
        if matches!(change.kind(), ChangeKind::Delete | ChangeKind::Forget) {
            execute_removal_change(client, change, &mut state, &mut journal).await?;
            applied += 1;
            change_index += 1;
            continue;
        }
        if change.kind() == ChangeKind::Move {
            execute_move_change(client, &mut compiled, change, &mut state, &mut journal).await?;
            applied += 1;
            change_index += 1;
            continue;
        }
        if change.kind() != ChangeKind::Create {
            execute_existing_change(client, &mut compiled, change, &mut state, &mut journal)
                .await?;
            applied += 1;
            change_index += 1;
            continue;
        }

        let checkpoint = change
            .checkpoint()
            .present()
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
        let placeholder = RemoteId::new("recovery-pending")
            .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
        let recovery_target = match change.address().kind() {
            ResourceKind::Environment => {
                let parent = checkpoint
                    .containment()
                    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
                if default_environments
                    .get(parent)
                    .is_some_and(|default| default.name == change.address().name().as_str())
                    && checkpoint.property(&PropertyPath::Description).is_some()
                {
                    minimal_resource_state(ResourceKind::Environment, checkpoint, placeholder)?
                } else {
                    checkpoint.materialize(change.address(), placeholder)?
                }
            }
            ResourceKind::Application if !checkpoint.property_paths().is_empty() => {
                minimal_resource_state(ResourceKind::Application, checkpoint, placeholder)?
            }
            _ => checkpoint.materialize(change.address(), placeholder)?,
        };
        let expected_checkpoint = ExpectedCheckpoint::create(recovery_target)?;
        let token = journal.start_recoverable_step(
            change.address().clone(),
            JournalAction::Create,
            expected_checkpoint,
        )?;
        let remote_id = match change.address().kind() {
            ResourceKind::Project => {
                let input = project_create_input(change.address(), checkpoint)?;
                let created = match client.projects().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        fail_if_definitive(&mut journal, token, code)?;
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

                        let input = UpdateEnvironment::new(
                            EnvironmentId::new(remote_id.as_str()),
                            nullable_string(description)?,
                        );
                        let before = state
                            .resource(change.address())
                            .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                            .clone();
                        let resource =
                            checkpoint.materialize(change.address(), remote_id.clone())?;
                        let update_token = journal.start_recoverable_step(
                            change.address().clone(),
                            JournalAction::Update,
                            ExpectedCheckpoint::update(before, resource.clone())?,
                        )?;
                        if let Err(error) = client.environments().update(input).await {
                            let code = failure_code(&error);
                            fail_if_definitive(&mut journal, update_token, code)?;
                            return Err(ApplyWorkspaceError::RemoteMutation { code });
                        }
                        state.upsert_resource(change.address().clone(), resource)?;
                        journal.succeed(update_token, Some(remote_id), &state)?;
                        applied += 1;
                        change_index += 1;
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
                            fail_if_definitive(&mut journal, token, code)?;
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
                        fail_if_definitive(&mut journal, token, code)?;
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
                    let before = state
                        .resource(change.address())
                        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                        .clone();
                    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
                    let update_token = journal.start_recoverable_step(
                        change.address().clone(),
                        JournalAction::Update,
                        ExpectedCheckpoint::update(before, resource.clone())?,
                    )?;
                    if let Err(error) = client.applications().update(update).await {
                        let code = failure_code(&error);
                        fail_if_definitive(&mut journal, update_token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                    state.upsert_resource(change.address().clone(), resource.clone())?;
                    journal.succeed(update_token, Some(remote_id.clone()), &state)?;

                    if application_requires_deploy(property_paths.iter()) {
                        let deploy_token = journal.start_recoverable_step(
                            change.address().clone(),
                            JournalAction::Deploy,
                            ExpectedCheckpoint::update(resource.clone(), resource.clone())?,
                        )?;
                        if let Err(error) = client
                            .applications()
                            .deploy(created.application_id().clone())
                            .await
                        {
                            let code = failure_code(&error);
                            fail_if_definitive(&mut journal, deploy_token, code)?;
                            return Err(ApplyWorkspaceError::RemoteMutation { code });
                        }
                        state.upsert_resource(change.address().clone(), resource)?;
                        journal.succeed(deploy_token, Some(remote_id), &state)?;
                    }

                    applied += 1;
                    change_index += 1;
                    continue;
                }
            }
            ResourceKind::Compose => {
                let environment_id = checkpoint_environment_id(checkpoint, &state)?;
                let document = take_sensitive_string(
                    &mut compiled,
                    change.address(),
                    &PropertyPath::ComposeDocument,
                )?;
                let mut input =
                    CreateCompose::new(change.address().name().as_str(), environment_id, document);
                if let Some(description) = optional_string(checkpoint, &PropertyPath::Description)?
                {
                    input = input.with_description(description);
                }
                let created = match client.composes().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        fail_if_definitive(&mut journal, token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.compose_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
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
                        fail_if_definitive(&mut journal, token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.postgres_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
            ResourceKind::MySql => {
                let environment_id = checkpoint_environment_id(checkpoint, &state)?;
                let database = required_string(checkpoint, &PropertyPath::Database)?;
                let username = required_string(checkpoint, &PropertyPath::Username)?;
                let password = take_sensitive_string(
                    &mut compiled,
                    change.address(),
                    &PropertyPath::Password,
                )?;
                let root_password = take_sensitive_string(
                    &mut compiled,
                    change.address(),
                    &PropertyPath::RootPassword,
                )?;
                let input = CreateMySql::new(
                    change.address().name().as_str(),
                    environment_id,
                    database,
                    username,
                    password,
                    root_password,
                );
                let created = match client.mysql().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        fail_if_definitive(&mut journal, token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.mysql_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
            ResourceKind::MariaDb => {
                let environment_id = checkpoint_environment_id(checkpoint, &state)?;
                let database = required_string(checkpoint, &PropertyPath::Database)?;
                let username = required_string(checkpoint, &PropertyPath::Username)?;
                let password = take_sensitive_string(
                    &mut compiled,
                    change.address(),
                    &PropertyPath::Password,
                )?;
                let mut input = CreateMariaDb::new(
                    change.address().name().as_str(),
                    environment_id,
                    database,
                    username,
                    password,
                );
                if let Some(root_password) = take_optional_sensitive_string(
                    &mut compiled,
                    change.address(),
                    checkpoint,
                    &PropertyPath::RootPassword,
                )? {
                    input = input.with_root_password(root_password);
                }
                let created = match client.mariadb().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        fail_if_definitive(&mut journal, token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.mariadb_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
            ResourceKind::Mongo => {
                let environment_id = checkpoint_environment_id(checkpoint, &state)?;
                let username = required_string(checkpoint, &PropertyPath::Username)?;
                let password = take_sensitive_string(
                    &mut compiled,
                    change.address(),
                    &PropertyPath::Password,
                )?;
                let mut input = CreateMongo::new(
                    change.address().name().as_str(),
                    environment_id,
                    username,
                    password,
                );
                if let Some(replica_sets) = optional_bool(checkpoint, &PropertyPath::ReplicaSets)? {
                    input = input.with_replica_sets(replica_sets);
                }
                let created = match client.mongo().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        fail_if_definitive(&mut journal, token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.mongo_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
            ResourceKind::LibSql => {
                return Err(ApplyWorkspaceError::UnsupportedChange);
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
                        fail_if_definitive(&mut journal, token, code)?;
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
                        fail_if_definitive(&mut journal, token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.domain_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
            ResourceKind::Mount => return Err(ApplyWorkspaceError::UnsupportedChange),
            ResourceKind::Port => {
                let input = port_create_input(checkpoint, &state)?;
                let created = match client.ports().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        fail_if_definitive(&mut journal, token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.port_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
            ResourceKind::Redirect => {
                let input = redirect_create_input(checkpoint, &state)?;
                let created = match client.redirects().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        fail_if_definitive(&mut journal, token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.redirect_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
            ResourceKind::Security => {
                let input =
                    security_create_input(&mut compiled, change.address(), checkpoint, &state)?;
                let created = match client.security().create(input).await {
                    Ok(created) => created,
                    Err(error) => {
                        let code = failure_code(&error);
                        fail_if_definitive(&mut journal, token, code)?;
                        return Err(ApplyWorkspaceError::RemoteMutation { code });
                    }
                };
                RemoteId::new(created.security_id().as_str())
                    .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?
            }
        };
        let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
        state.upsert_resource(change.address().clone(), resource)?;
        journal.succeed(token, Some(remote_id), &state)?;
        applied += 1;
        change_index += 1;
    }

    journal.commit()?;

    Ok(ApplySummary { applied })
}

fn preflight(plan: &Plan) -> Result<(), ApplyWorkspaceError> {
    if plan.changes().iter().all(|change| match change.kind() {
        ChangeKind::Create => {
            matches!(
                change.address().kind(),
                ResourceKind::Project
                    | ResourceKind::Environment
                    | ResourceKind::Application
                    | ResourceKind::Compose
                    | ResourceKind::Postgres
                    | ResourceKind::MySql
                    | ResourceKind::MariaDb
                    | ResourceKind::Mongo
                    | ResourceKind::LibSql
                    | ResourceKind::Redis
                    | ResourceKind::Domain
                    | ResourceKind::Port
                    | ResourceKind::Redirect
                    | ResourceKind::Security
            )
        }
        ChangeKind::NoOp | ChangeKind::Forget => true,
        ChangeKind::Update | ChangeKind::Delete | ChangeKind::Move => true,
        ChangeKind::Reparent => change.address().kind() == ResourceKind::Application,
        ChangeKind::Replace => {
            matches!(
                change.address().kind(),
                ResourceKind::LibSql
                    | ResourceKind::Port
                    | ResourceKind::Redirect
                    | ResourceKind::Security
            ) && change.replacement_order() == Some(ReplacementOrder::DeleteBeforeCreate)
        }
    }) {
        Ok(())
    } else {
        Err(ApplyWorkspaceError::UnsupportedChange)
    }
}

async fn execute_move_change(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    let source = change
        .previous_address()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    if change.checkpoint().move_from() != Some(source) {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let checkpoint = change
        .checkpoint()
        .move_target()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let move_action = change
        .move_action()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let before = state
        .resource(source)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    if state.resource(change.address()).is_some() {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let remote_id = before.remote_id().clone();
    let source_target = checkpoint.materialize(source, remote_id.clone())?;
    let selected_paths = change
        .fields()
        .iter()
        .map(|field| field.key().clone())
        .collect::<Vec<_>>();

    if before != source_target {
        let token = journal.start_recoverable_step(
            source.clone(),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, source_target.clone())?,
        )?;
        if move_action == MoveAction::Update {
            let mutation = prepare_move_mutation(
                client,
                compiled,
                change.address(),
                checkpoint,
                &remote_id,
                &selected_paths,
            )
            .await?;
            if let Err(error) = mutation.execute(client).await {
                let code = failure_code(&error);
                fail_if_definitive(journal, token, code)?;
                return Err(ApplyWorkspaceError::RemoteMutation { code });
            }
        } else if !selected_paths.is_empty() {
            return Err(ApplyWorkspaceError::InvalidCheckpoint);
        }
        state.upsert_resource(source.clone(), source_target.clone())?;
        journal.succeed(token, Some(remote_id.clone()), state)?;

        if move_action == MoveAction::Update
            && source.kind() == ResourceKind::Application
            && application_requires_deploy(selected_paths.iter())
        {
            let token = journal.start_recoverable_step(
                source.clone(),
                JournalAction::Deploy,
                ExpectedCheckpoint::update(source_target.clone(), source_target.clone())?,
            )?;
            if let Err(error) = client
                .applications()
                .deploy(ApplicationId::new(remote_id.as_str()))
                .await
            {
                let code = failure_code(&error);
                fail_if_definitive(journal, token, code)?;
                return Err(ApplyWorkspaceError::RemoteMutation { code });
            }
            state.upsert_resource(source.clone(), source_target)?;
            journal.succeed(token, Some(remote_id.clone()), state)?;
        }
    } else if move_action == MoveAction::Update {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    let mut proposed = state.clone();
    proposed.move_resource(source, change.address().clone())?;
    let exact_target = checkpoint.materialize(change.address(), remote_id)?;
    if proposed.resource(change.address()) != Some(&exact_target) {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let move_before = state
        .resource(source)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Move,
        ExpectedCheckpoint::move_resource(source.clone(), move_before, exact_target)?,
    )?;
    state.move_resource(source, change.address().clone())?;
    journal.succeed(token, None, state)?;

    Ok(())
}

async fn prepare_move_mutation(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
    target: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    remote_id: &RemoteId,
    selected_paths: &[PropertyPath],
) -> Result<ExistingMutation, ApplyWorkspaceError> {
    match target.kind() {
        ResourceKind::Project => {
            let description = checkpoint
                .property(&PropertyPath::Description)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)
                .and_then(nullable_string)?;
            Ok(ExistingMutation::Project(UpdateProject::new(
                ProjectId::new(remote_id.as_str()),
                description,
            )))
        }
        ResourceKind::Environment => {
            let description = checkpoint
                .property(&PropertyPath::Description)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)
                .and_then(nullable_string)?;
            Ok(ExistingMutation::Environment(UpdateEnvironment::new(
                EnvironmentId::new(remote_id.as_str()),
                description,
            )))
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
            Ok(ExistingMutation::Application(application_update_input(
                compiled,
                target,
                checkpoint,
                ApplicationId::new(remote_id.as_str()),
                selected_paths,
                current_environment,
            )?))
        }
        ResourceKind::Compose => Ok(ExistingMutation::Compose(compose_update_input(
            compiled,
            target,
            checkpoint,
            ComposeId::new(remote_id.as_str()),
            selected_paths,
        )?)),
        ResourceKind::Postgres => {
            let mut input = UpdatePostgres::new(PostgresId::new(remote_id.as_str()));
            for path in selected_paths {
                match path {
                    PropertyPath::Database => {
                        input = input.with_database(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Username => {
                        input = input.with_username(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Password => {
                        input = input.with_password(take_sensitive_string(compiled, target, path)?);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }
            Ok(ExistingMutation::Postgres(input))
        }
        ResourceKind::MySql => {
            let mut input = UpdateMySql::new(MySqlId::new(remote_id.as_str()));
            for path in selected_paths {
                match path {
                    PropertyPath::Database => {
                        input = input.with_database(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Username => {
                        input = input.with_username(required_string(checkpoint, path)?);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }
            Ok(ExistingMutation::MySql(input))
        }
        ResourceKind::MariaDb => {
            let mut input = UpdateMariaDb::new(MariaDbId::new(remote_id.as_str()));
            for path in selected_paths {
                match path {
                    PropertyPath::Database => {
                        input = input.with_database(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Username => {
                        input = input.with_username(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Password | PropertyPath::RootPassword => {
                        return Err(ApplyWorkspaceError::UnsupportedChange);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }
            Ok(ExistingMutation::MariaDb(input))
        }
        ResourceKind::Mongo => {
            let mut input = UpdateMongo::new(MongoId::new(remote_id.as_str()));
            for path in selected_paths {
                match path {
                    PropertyPath::Username => {
                        input = input.with_username(required_string(checkpoint, path)?);
                    }
                    PropertyPath::ReplicaSets => {
                        input = input.with_replica_sets(required_bool(checkpoint, path)?);
                    }
                    PropertyPath::Password => {
                        return Err(ApplyWorkspaceError::UnsupportedChange);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }
            Ok(ExistingMutation::Mongo(input))
        }
        ResourceKind::LibSql => Err(ApplyWorkspaceError::UnsupportedChange),
        ResourceKind::Redis => {
            if selected_paths != [PropertyPath::Password] {
                return Err(ApplyWorkspaceError::InvalidCheckpoint);
            }
            let password = take_sensitive_string(compiled, target, &PropertyPath::Password)?;
            Ok(ExistingMutation::Redis(UpdateRedis::new(
                RedisId::new(remote_id.as_str()),
                password,
            )))
        }
        ResourceKind::Domain => {
            let host = required_string(checkpoint, &PropertyPath::Host)?;
            Ok(ExistingMutation::Domain(UpdateDomain::new(
                DomainId::new(remote_id.as_str()),
                host,
            )))
        }
        ResourceKind::Port
        | ResourceKind::Redirect
        | ResourceKind::Security
        | ResourceKind::Mount => Err(ApplyWorkspaceError::UnsupportedChange),
    }
}

async fn execute_removal_change(
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
    let action = match change.kind() {
        ChangeKind::Delete => JournalAction::Delete,
        ChangeKind::Forget => JournalAction::Forget,
        _ => return Err(ApplyWorkspaceError::UnsupportedChange),
    };
    let token = journal.start_recoverable_step(
        change.address().clone(),
        action,
        ExpectedCheckpoint::remove(before.clone()),
    )?;

    if change.kind() == ChangeKind::Delete {
        let Some(result) = delete_remote_resource(client, before.kind(), before.remote_id()).await
        else {
            return Err(ApplyWorkspaceError::UnsupportedChange);
        };
        if let Err(error) = result
            && !is_already_missing(&error)
        {
            let code = failure_code(&error);
            fail_if_definitive(journal, token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    }

    state.remove_resource(change.address())?;
    journal.succeed(token, None, state)?;

    Ok(())
}

async fn delete_remote_resource(
    client: &Dokploy,
    kind: ResourceKind,
    remote_id: &RemoteId,
) -> Option<Result<(), SdkError>> {
    match kind {
        ResourceKind::Project => Some(
            client
                .projects()
                .delete(ProjectId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::Environment => Some(
            client
                .environments()
                .delete(EnvironmentId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::Application => Some(
            client
                .applications()
                .delete(ApplicationId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::Compose => Some(
            client
                .composes()
                .delete(
                    ComposeId::new(remote_id.as_str()),
                    ComposeVolumePolicy::Preserve,
                )
                .await,
        ),
        ResourceKind::Postgres => Some(
            client
                .postgres()
                .delete(PostgresId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::MySql => Some(
            client
                .mysql()
                .delete(MySqlId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::MariaDb => Some(
            client
                .mariadb()
                .delete(MariaDbId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::Mongo => Some(
            client
                .mongo()
                .delete(MongoId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::LibSql => Some(
            client
                .libsql()
                .delete(LibSqlId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::Redis => Some(
            client
                .redis()
                .delete(RedisId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::Domain => Some(
            client
                .domains()
                .delete(DomainId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::Port => Some(client.ports().delete(PortId::new(remote_id.as_str())).await),
        ResourceKind::Redirect => Some(
            client
                .redirects()
                .delete(RedirectId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::Security => Some(
            client
                .security()
                .delete(SecurityId::new(remote_id.as_str()))
                .await,
        ),
        ResourceKind::Mount => None,
    }
}

fn is_already_missing(error: &SdkError) -> bool {
    matches!(error, SdkError::Api(error) if error.status() == 404)
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

fn checkpoint_application_id(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<ApplicationId, ApplyWorkspaceError> {
    let parent = checkpoint
        .containment()
        .filter(|parent| parent.kind() == ResourceKind::Application)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let parent_id = state
        .resource(parent)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .remote_id();

    Ok(ApplicationId::new(parent_id.as_str()))
}

fn port_create_input(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<CreatePort, ApplyWorkspaceError> {
    Ok(CreatePort::new(
        checkpoint_application_id(checkpoint, state)?,
        required_port_number(checkpoint, &PropertyPath::PublishedPort)?,
        required_port_number(checkpoint, &PropertyPath::TargetPort)?,
        required_publish_mode(checkpoint)?,
        required_port_protocol(checkpoint)?,
    ))
}

fn port_update_input(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    remote_id: &RemoteId,
) -> Result<UpdatePort, ApplyWorkspaceError> {
    Ok(UpdatePort::new(
        PortId::new(remote_id.as_str()),
        required_port_number(checkpoint, &PropertyPath::PublishedPort)?,
        required_port_number(checkpoint, &PropertyPath::TargetPort)?,
        required_publish_mode(checkpoint)?,
        required_port_protocol(checkpoint)?,
    ))
}

fn redirect_create_input(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<CreateRedirect, ApplyWorkspaceError> {
    Ok(CreateRedirect::new(
        checkpoint_application_id(checkpoint, state)?,
        required_string(checkpoint, &PropertyPath::Regex)?,
        required_string(checkpoint, &PropertyPath::Replacement)?,
        required_bool(checkpoint, &PropertyPath::Permanent)?,
    ))
}

fn security_create_input(
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<CreateSecurity, ApplyWorkspaceError> {
    let application_id = checkpoint_application_id(checkpoint, state)?;
    let username = required_string(checkpoint, &PropertyPath::Username)?;
    let password = take_sensitive_string(compiled, address, &PropertyPath::Password)?;

    Ok(CreateSecurity::new(application_id, username, password))
}

fn required_port_number(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    path: &PropertyPath,
) -> Result<NonZeroU16, ApplyWorkspaceError> {
    let value = match checkpoint.property(path) {
        Some(CheckpointValueRef::NonSensitive(value)) => value.as_u64(),
        _ => None,
    }
    .and_then(|value| u16::try_from(value).ok())
    .and_then(NonZeroU16::new)
    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    Ok(value)
}

fn required_publish_mode(
    checkpoint: &dokploy_core::ResourceCheckpoint,
) -> Result<PublishMode, ApplyWorkspaceError> {
    match required_string(checkpoint, &PropertyPath::PublishMode)?.as_str() {
        "ingress" => Ok(PublishMode::Ingress),
        "host" => Ok(PublishMode::Host),
        _ => Err(ApplyWorkspaceError::InvalidCheckpoint),
    }
}

fn required_port_protocol(
    checkpoint: &dokploy_core::ResourceCheckpoint,
) -> Result<PortProtocol, ApplyWorkspaceError> {
    match required_string(checkpoint, &PropertyPath::Protocol)?.as_str() {
        "tcp" => Ok(PortProtocol::Tcp),
        "udp" => Ok(PortProtocol::Udp),
        _ => Err(ApplyWorkspaceError::InvalidCheckpoint),
    }
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

fn required_bool(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    path: &PropertyPath,
) -> Result<bool, ApplyWorkspaceError> {
    match checkpoint.property(path) {
        Some(CheckpointValueRef::NonSensitive(value)) => value
            .as_bool()
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint),
        None
        | Some(
            CheckpointValueRef::Null
            | CheckpointValueRef::EmptyCollection
            | CheckpointValueRef::Sensitive,
        ) => Err(ApplyWorkspaceError::InvalidCheckpoint),
    }
}

fn optional_bool(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    path: &PropertyPath,
) -> Result<Option<bool>, ApplyWorkspaceError> {
    match checkpoint.property(path) {
        None | Some(CheckpointValueRef::Null) => Ok(None),
        Some(CheckpointValueRef::NonSensitive(value)) => value
            .as_bool()
            .map(Some)
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint),
        Some(CheckpointValueRef::EmptyCollection | CheckpointValueRef::Sensitive) => {
            Err(ApplyWorkspaceError::InvalidCheckpoint)
        }
    }
}

fn optional_string(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    path: &PropertyPath,
) -> Result<Option<String>, ApplyWorkspaceError> {
    match checkpoint.property(path) {
        None | Some(CheckpointValueRef::Null) => Ok(None),
        Some(CheckpointValueRef::NonSensitive(value)) => value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint),
        Some(CheckpointValueRef::EmptyCollection | CheckpointValueRef::Sensitive) => {
            Err(ApplyWorkspaceError::InvalidCheckpoint)
        }
    }
}

fn required_libsql_node(
    checkpoint: &dokploy_core::ResourceCheckpoint,
) -> Result<LibSqlNode, ApplyWorkspaceError> {
    let Some(CheckpointValueRef::NonSensitive(value)) = checkpoint.property(&PropertyPath::Node)
    else {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    };
    let node_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    match node_type {
        "primary" if value.as_object().is_some_and(|node| node.len() == 1) => {
            Ok(LibSqlNode::Primary)
        }
        "replica" if value.as_object().is_some_and(|node| node.len() == 2) => {
            let primary_url = value
                .get("primary_url")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
            Ok(LibSqlNode::Replica {
                primary_url: primary_url.to_owned(),
            })
        }
        _ => Err(ApplyWorkspaceError::InvalidCheckpoint),
    }
}

fn libsql_scope_ids(
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
) -> Result<(ProjectId, EnvironmentId), ApplyWorkspaceError> {
    let environment = checkpoint
        .containment()
        .filter(|address| address.kind() == ResourceKind::Environment)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let environment_state = state
        .resource(environment)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let project = environment_state
        .containment()
        .filter(|address| address.kind() == ResourceKind::Project)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let project_state = state
        .resource(project)
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    Ok((
        ProjectId::new(project_state.remote_id().as_str()),
        EnvironmentId::new(environment_state.remote_id().as_str()),
    ))
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

fn take_optional_sensitive_string(
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    path: &PropertyPath,
) -> Result<Option<Zeroizing<String>>, ApplyWorkspaceError> {
    match checkpoint.property(path) {
        None | Some(CheckpointValueRef::Null) => Ok(None),
        Some(CheckpointValueRef::Sensitive) => {
            take_sensitive_string(compiled, address, path).map(Some)
        }
        Some(CheckpointValueRef::EmptyCollection | CheckpointValueRef::NonSensitive(_)) => {
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

fn compose_update_input(
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    compose_id: ComposeId,
    selected_paths: &[PropertyPath],
) -> Result<UpdateCompose, ApplyWorkspaceError> {
    let mut input = UpdateCompose::new(compose_id);
    for path in selected_paths {
        match path {
            PropertyPath::Description => {
                input = match checkpoint
                    .property(path)
                    .ok_or(ApplyWorkspaceError::InvalidCheckpoint)
                    .and_then(nullable_string)?
                {
                    Nullable::Null => input.clear_description(),
                    Nullable::Value(description) => input.with_description(description),
                };
            }
            PropertyPath::ComposeDocument => {
                input = input.with_compose_file(take_sensitive_string(compiled, address, path)?);
            }
            _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
        }
    }

    Ok(input)
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

fn database_batch_len(changes: &[dokploy_core::PlannedChange], parallelism: usize) -> usize {
    changes
        .iter()
        .take(parallelism)
        .take_while(|change| {
            matches!(change.kind(), ChangeKind::Create | ChangeKind::Update)
                && (change.address().kind() != ResourceKind::LibSql
                    || change.kind() == ChangeKind::Create)
                && matches!(
                    change.address().kind(),
                    ResourceKind::Postgres
                        | ResourceKind::MySql
                        | ResourceKind::MariaDb
                        | ResourceKind::Mongo
                        | ResourceKind::LibSql
                        | ResourceKind::Redis
                )
        })
        .count()
}

async fn execute_database_batch(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
    changes: &[dokploy_core::PlannedChange],
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<usize, ApplyWorkspaceError> {
    let mut prepared = Vec::with_capacity(changes.len());
    for change in changes {
        prepared.push(prepare_database_mutation(compiled, change, state)?);
    }

    let mut in_flight = Vec::with_capacity(prepared.len());
    for prepared in prepared {
        let token = journal.start_recoverable_step(
            prepared.address.clone(),
            prepared.action,
            prepared.expected_checkpoint,
        )?;
        let task_client = client.clone();
        let handle = tokio::spawn(async move { prepared.mutation.execute(&task_client).await });
        in_flight.push((token, prepared.address, prepared.checkpoint, handle));
    }

    let mut completed = Vec::with_capacity(in_flight.len());
    for (token, address, checkpoint, handle) in in_flight {
        let result = match handle.await {
            Ok(result) => result,
            Err(_) => Err(DatabaseMutationFailure::Internal),
        };
        completed.push((token, address, checkpoint, result));
    }

    let mut first_failure = None;
    let mut checkpointed = 0;
    for (token, address, checkpoint, result) in completed {
        match result {
            Ok(remote_id) => {
                let resource = checkpoint.materialize(&address, remote_id.clone())?;
                state.upsert_resource(address, resource)?;
                journal.succeed(token, Some(remote_id), state)?;
                checkpointed += 1;
            }
            Err(error) => {
                let code = error.code();
                if error.is_definitive() {
                    journal.fail(token, code)?;
                }
                first_failure.get_or_insert(code);
            }
        }
    }

    if let Some(code) = first_failure {
        return Err(ApplyWorkspaceError::RemoteMutation { code });
    }

    Ok(checkpointed)
}

fn prepare_database_mutation(
    compiled: &mut crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &StateFile,
) -> Result<PreparedDatabaseMutation, ApplyWorkspaceError> {
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    let address = change.address().clone();
    let (action, mutation) = match (change.kind(), change.address().kind()) {
        (ChangeKind::Create, ResourceKind::Postgres) => {
            let environment_id = checkpoint_environment_id(&checkpoint, state)?;
            let database = required_string(&checkpoint, &PropertyPath::Database)?;
            let username = required_string(&checkpoint, &PropertyPath::Username)?;
            let password = take_sensitive_string(compiled, &address, &PropertyPath::Password)?;
            let input = CreatePostgres::new(
                address.name().as_str(),
                environment_id,
                database,
                username,
                password,
            );

            (
                JournalAction::Create,
                DatabaseMutation::CreatePostgres(input),
            )
        }
        (ChangeKind::Create, ResourceKind::MySql) => {
            let environment_id = checkpoint_environment_id(&checkpoint, state)?;
            let database = required_string(&checkpoint, &PropertyPath::Database)?;
            let username = required_string(&checkpoint, &PropertyPath::Username)?;
            let password = take_sensitive_string(compiled, &address, &PropertyPath::Password)?;
            let root_password =
                take_sensitive_string(compiled, &address, &PropertyPath::RootPassword)?;
            let input = CreateMySql::new(
                address.name().as_str(),
                environment_id,
                database,
                username,
                password,
                root_password,
            );

            (JournalAction::Create, DatabaseMutation::CreateMySql(input))
        }
        (ChangeKind::Create, ResourceKind::MariaDb) => {
            let environment_id = checkpoint_environment_id(&checkpoint, state)?;
            let database = required_string(&checkpoint, &PropertyPath::Database)?;
            let username = required_string(&checkpoint, &PropertyPath::Username)?;
            let password = take_sensitive_string(compiled, &address, &PropertyPath::Password)?;
            let mut input = CreateMariaDb::new(
                address.name().as_str(),
                environment_id,
                database,
                username,
                password,
            );
            if let Some(root_password) = take_optional_sensitive_string(
                compiled,
                &address,
                &checkpoint,
                &PropertyPath::RootPassword,
            )? {
                input = input.with_root_password(root_password);
            }

            (
                JournalAction::Create,
                DatabaseMutation::CreateMariaDb(input),
            )
        }
        (ChangeKind::Create, ResourceKind::Mongo) => {
            let environment_id = checkpoint_environment_id(&checkpoint, state)?;
            let username = required_string(&checkpoint, &PropertyPath::Username)?;
            let password = take_sensitive_string(compiled, &address, &PropertyPath::Password)?;
            let mut input =
                CreateMongo::new(address.name().as_str(), environment_id, username, password);
            if let Some(replica_sets) = optional_bool(&checkpoint, &PropertyPath::ReplicaSets)? {
                input = input.with_replica_sets(replica_sets);
            }

            (JournalAction::Create, DatabaseMutation::CreateMongo(input))
        }
        (ChangeKind::Create, ResourceKind::LibSql) => {
            let (project_id, environment_id) = libsql_scope_ids(&checkpoint, state)?;
            let username = required_string(&checkpoint, &PropertyPath::Username)?;
            let password = take_sensitive_string(compiled, &address, &PropertyPath::Password)?;
            let node = required_libsql_node(&checkpoint)?;
            let mut input = CreateLibSql::new(
                address.name().as_str(),
                address.name().as_str(),
                project_id,
                environment_id.clone(),
                username,
                password,
                node,
            );
            if let Some(description) = optional_string(&checkpoint, &PropertyPath::Description)? {
                input = input.with_description(description);
            }

            (
                JournalAction::Create,
                DatabaseMutation::CreateLibSql {
                    input,
                    name: address.name().as_str().to_owned(),
                    environment_id,
                },
            )
        }
        (ChangeKind::Create, ResourceKind::Redis) => {
            let environment_id = checkpoint_environment_id(&checkpoint, state)?;
            let password = take_sensitive_string(compiled, &address, &PropertyPath::Password)?;
            let input = CreateRedis::new(address.name().as_str(), environment_id, password);

            (JournalAction::Create, DatabaseMutation::CreateRedis(input))
        }
        (ChangeKind::Update, ResourceKind::Postgres) => {
            let remote_id = state
                .resource(&address)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                .remote_id()
                .clone();
            let mut input = UpdatePostgres::new(PostgresId::new(remote_id.as_str()));
            for path in change.fields().iter().map(|field| field.key()) {
                match path {
                    PropertyPath::Database => {
                        input = input.with_database(required_string(&checkpoint, path)?);
                    }
                    PropertyPath::Username => {
                        input = input.with_username(required_string(&checkpoint, path)?);
                    }
                    PropertyPath::Password => {
                        input =
                            input.with_password(take_sensitive_string(compiled, &address, path)?);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }

            (
                JournalAction::Update,
                DatabaseMutation::UpdatePostgres(input, remote_id),
            )
        }
        (ChangeKind::Update, ResourceKind::MySql) => {
            let remote_id = state
                .resource(&address)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                .remote_id()
                .clone();
            let mut input = UpdateMySql::new(MySqlId::new(remote_id.as_str()));
            for path in change.fields().iter().map(|field| field.key()) {
                match path {
                    PropertyPath::Database => {
                        input = input.with_database(required_string(&checkpoint, path)?);
                    }
                    PropertyPath::Username => {
                        input = input.with_username(required_string(&checkpoint, path)?);
                    }
                    PropertyPath::Password | PropertyPath::RootPassword => {
                        return Err(ApplyWorkspaceError::UnsupportedChange);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }

            (
                JournalAction::Update,
                DatabaseMutation::UpdateMySql(input, remote_id),
            )
        }
        (ChangeKind::Update, ResourceKind::MariaDb) => {
            let remote_id = state
                .resource(&address)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                .remote_id()
                .clone();
            let mut input = UpdateMariaDb::new(MariaDbId::new(remote_id.as_str()));
            for path in change.fields().iter().map(|field| field.key()) {
                match path {
                    PropertyPath::Database => {
                        input = input.with_database(required_string(&checkpoint, path)?);
                    }
                    PropertyPath::Username => {
                        input = input.with_username(required_string(&checkpoint, path)?);
                    }
                    PropertyPath::Password | PropertyPath::RootPassword => {
                        return Err(ApplyWorkspaceError::UnsupportedChange);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }

            (
                JournalAction::Update,
                DatabaseMutation::UpdateMariaDb(input, remote_id),
            )
        }
        (ChangeKind::Update, ResourceKind::Mongo) => {
            let remote_id = state
                .resource(&address)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                .remote_id()
                .clone();
            let mut input = UpdateMongo::new(MongoId::new(remote_id.as_str()));
            for path in change.fields().iter().map(|field| field.key()) {
                match path {
                    PropertyPath::Username => {
                        input = input.with_username(required_string(&checkpoint, path)?);
                    }
                    PropertyPath::ReplicaSets => {
                        input = input.with_replica_sets(required_bool(&checkpoint, path)?);
                    }
                    PropertyPath::Password => {
                        return Err(ApplyWorkspaceError::UnsupportedChange);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }

            (
                JournalAction::Update,
                DatabaseMutation::UpdateMongo(input, remote_id),
            )
        }
        (ChangeKind::Update, ResourceKind::Redis) => {
            if change.fields().len() != 1 || change.fields()[0].key() != &PropertyPath::Password {
                return Err(ApplyWorkspaceError::InvalidCheckpoint);
            }
            let remote_id = state
                .resource(&address)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                .remote_id()
                .clone();
            let password = take_sensitive_string(compiled, &address, &PropertyPath::Password)?;

            (
                JournalAction::Update,
                DatabaseMutation::UpdateRedis(
                    UpdateRedis::new(RedisId::new(remote_id.as_str()), password),
                    remote_id,
                ),
            )
        }
        _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
    };
    let expected_checkpoint = match action {
        JournalAction::Create => {
            let placeholder = RemoteId::new("recovery-pending")
                .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
            ExpectedCheckpoint::create(checkpoint.materialize(&address, placeholder)?)?
        }
        JournalAction::Update => {
            let before = state
                .resource(&address)
                .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
                .clone();
            let target = checkpoint.materialize(&address, before.remote_id().clone())?;
            ExpectedCheckpoint::update(before, target)?
        }
        JournalAction::Delete
        | JournalAction::Forget
        | JournalAction::Move
        | JournalAction::Deploy => {
            return Err(ApplyWorkspaceError::InvalidCheckpoint);
        }
    };

    Ok(PreparedDatabaseMutation {
        address,
        checkpoint,
        action,
        expected_checkpoint,
        mutation,
    })
}

struct PreparedDatabaseMutation {
    address: ResourceAddress,
    checkpoint: dokploy_core::ResourceCheckpoint,
    action: JournalAction,
    expected_checkpoint: ExpectedCheckpoint,
    mutation: DatabaseMutation,
}

enum DatabaseMutation {
    CreatePostgres(CreatePostgres),
    CreateMySql(CreateMySql),
    CreateMariaDb(CreateMariaDb),
    CreateMongo(CreateMongo),
    CreateLibSql {
        input: CreateLibSql,
        name: String,
        environment_id: EnvironmentId,
    },
    CreateRedis(CreateRedis),
    UpdatePostgres(UpdatePostgres, RemoteId),
    UpdateMySql(UpdateMySql, RemoteId),
    UpdateMariaDb(UpdateMariaDb, RemoteId),
    UpdateMongo(UpdateMongo, RemoteId),
    UpdateRedis(UpdateRedis, RemoteId),
}

impl DatabaseMutation {
    async fn execute(self, client: &Dokploy) -> Result<RemoteId, DatabaseMutationFailure> {
        match self {
            Self::CreatePostgres(input) => {
                let created = client
                    .postgres()
                    .create(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                RemoteId::new(created.postgres_id().as_str())
                    .map_err(|_| DatabaseMutationFailure::InvalidIdentity)
            }
            Self::CreateMySql(input) => {
                let created = client
                    .mysql()
                    .create(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                RemoteId::new(created.mysql_id().as_str())
                    .map_err(|_| DatabaseMutationFailure::InvalidIdentity)
            }
            Self::CreateMariaDb(input) => {
                let created = client
                    .mariadb()
                    .create(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                RemoteId::new(created.mariadb_id().as_str())
                    .map_err(|_| DatabaseMutationFailure::InvalidIdentity)
            }
            Self::CreateMongo(input) => {
                let created = client
                    .mongo()
                    .create(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                RemoteId::new(created.mongo_id().as_str())
                    .map_err(|_| DatabaseMutationFailure::InvalidIdentity)
            }
            Self::CreateLibSql {
                input,
                name,
                environment_id,
            } => {
                let created = client
                    .libsql()
                    .create(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                let details = client
                    .libsql()
                    .get(created.libsql_id().clone())
                    .await
                    .map_err(|error| {
                        DatabaseMutationFailure::OutcomeUnknown(failure_code(&error))
                    })?;
                if details.libsql_id != *created.libsql_id()
                    || details.environment_id != environment_id
                    || details.name != name
                {
                    return Err(DatabaseMutationFailure::Internal);
                }
                RemoteId::new(created.libsql_id().as_str())
                    .map_err(|_| DatabaseMutationFailure::InvalidIdentity)
            }
            Self::CreateRedis(input) => {
                let created = client
                    .redis()
                    .create(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                RemoteId::new(created.redis_id().as_str())
                    .map_err(|_| DatabaseMutationFailure::InvalidIdentity)
            }
            Self::UpdatePostgres(input, remote_id) => {
                client
                    .postgres()
                    .update(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                Ok(remote_id)
            }
            Self::UpdateMySql(input, remote_id) => {
                client
                    .mysql()
                    .update(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                Ok(remote_id)
            }
            Self::UpdateMariaDb(input, remote_id) => {
                client
                    .mariadb()
                    .update(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                Ok(remote_id)
            }
            Self::UpdateMongo(input, remote_id) => {
                client
                    .mongo()
                    .update(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                Ok(remote_id)
            }
            Self::UpdateRedis(input, remote_id) => {
                client
                    .redis()
                    .update(input)
                    .await
                    .map_err(DatabaseMutationFailure::Sdk)?;
                Ok(remote_id)
            }
        }
    }
}

enum DatabaseMutationFailure {
    Sdk(SdkError),
    OutcomeUnknown(FailureCode),
    InvalidIdentity,
    Internal,
}

impl DatabaseMutationFailure {
    fn code(&self) -> FailureCode {
        match self {
            Self::Sdk(error) => failure_code(error),
            Self::OutcomeUnknown(code) => *code,
            Self::InvalidIdentity | Self::Internal => FailureCode::Internal,
        }
    }

    fn is_definitive(&self) -> bool {
        match self {
            Self::Sdk(error) => failure_code(error) != FailureCode::TransportOutcomeUnknown,
            Self::OutcomeUnknown(_) | Self::InvalidIdentity | Self::Internal => false,
        }
    }
}

async fn execute_delete_before_create_replacement(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    if change.address().kind() == ResourceKind::Port
        && change.replacement_order() == Some(ReplacementOrder::DeleteBeforeCreate)
    {
        return execute_port_replacement(client, change, state, journal).await;
    }
    if matches!(
        change.address().kind(),
        ResourceKind::Redirect | ResourceKind::Security
    ) && change.replacement_order() == Some(ReplacementOrder::DeleteBeforeCreate)
    {
        return execute_application_leaf_replacement(client, compiled, change, state, journal)
            .await;
    }
    if change.address().kind() != ResourceKind::LibSql
        || change.replacement_order() != Some(ReplacementOrder::DeleteBeforeCreate)
    {
        return Err(ApplyWorkspaceError::UnsupportedChange);
    }
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
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
        .libsql()
        .delete(LibSqlId::new(before.remote_id().as_str()))
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
    let (project_id, environment_id) = libsql_scope_ids(checkpoint, state)?;
    let username = required_string(checkpoint, &PropertyPath::Username)?;
    let password = take_sensitive_string(compiled, change.address(), &PropertyPath::Password)?;
    let node = required_libsql_node(checkpoint)?;
    let mut input = CreateLibSql::new(
        change.address().name().as_str(),
        change.address().name().as_str(),
        project_id,
        environment_id.clone(),
        username,
        password,
        node,
    );
    if let Some(description) = optional_string(checkpoint, &PropertyPath::Description)? {
        input = input.with_description(description);
    }
    let result = DatabaseMutation::CreateLibSql {
        input,
        name: change.address().name().as_str().to_owned(),
        environment_id,
    }
    .execute(client)
    .await;
    let remote_id = match result {
        Ok(remote_id) => remote_id,
        Err(error) => {
            let code = error.code();
            if error.is_definitive() {
                journal.fail(create_token, code)?;
            }
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(create_token, Some(remote_id), state)?;

    Ok(())
}

async fn execute_port_replacement(
    client: &Dokploy,
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
    let delete_token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Delete,
        ExpectedCheckpoint::remove(before.clone()),
    )?;
    if let Err(error) = client
        .ports()
        .delete(PortId::new(before.remote_id().as_str()))
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
    let input = port_create_input(checkpoint, state)?;
    let created = match client.ports().create(input).await {
        Ok(created) => created,
        Err(error) => {
            let code = failure_code(&error);
            fail_if_definitive(journal, create_token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let remote_id = RemoteId::new(created.port_id().as_str())
        .map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(create_token, Some(remote_id), state)?;

    Ok(())
}

/// Replaces a Redirect or Security entry whose containing application changed.
///
/// The old identity is deleted and checkpointed first. The create step carries a
/// recovery placeholder so an uncertain create can be adopted by its exact
/// application-scoped collision key.
async fn execute_application_leaf_replacement(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
    change: &dokploy_core::PlannedChange,
    state: &mut StateFile,
    journal: &mut OperationJournal<'_, '_>,
) -> Result<(), ApplyWorkspaceError> {
    let kind = change.address().kind();
    let checkpoint = change
        .checkpoint()
        .present()
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
    let before = state
        .resource(change.address())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    let delete_token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Delete,
        ExpectedCheckpoint::remove(before.clone()),
    )?;
    let Some(deleted) = delete_remote_resource(client, kind, before.remote_id()).await else {
        return Err(ApplyWorkspaceError::UnsupportedChange);
    };
    if let Err(error) = deleted
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
    let created = match kind {
        ResourceKind::Redirect => {
            let input = redirect_create_input(checkpoint, state)?;
            client
                .redirects()
                .create(input)
                .await
                .map(|created| created.redirect_id().as_str().to_owned())
        }
        ResourceKind::Security => {
            let input = security_create_input(compiled, change.address(), checkpoint, state)?;
            client
                .security()
                .create(input)
                .await
                .map(|created| created.security_id().as_str().to_owned())
        }
        _ => return Err(ApplyWorkspaceError::UnsupportedChange),
    };
    let created = match created {
        Ok(created) => created,
        Err(error) => {
            let code = failure_code(&error);
            fail_if_definitive(journal, create_token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
    };
    let remote_id =
        RemoteId::new(created).map_err(|_| ApplyWorkspaceError::InvalidRemoteIdentity)?;
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    state.upsert_resource(change.address().clone(), resource)?;
    journal.succeed(create_token, Some(remote_id), state)?;

    Ok(())
}

/// Prepares a complete Redirect replacement from a fresh read.
///
/// Fields selected by the plan come from the checkpoint. Every other field,
/// including an ignored one, keeps its current remote value, so the complete
/// replacement never overwrites a value this configuration does not own.
async fn redirect_update_input(
    client: &Dokploy,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
    remote_id: &RemoteId,
    selected_paths: &[PropertyPath],
) -> Result<UpdateRedirect, ApplyWorkspaceError> {
    if selected_paths.iter().any(|path| {
        !matches!(
            path,
            PropertyPath::Regex | PropertyPath::Replacement | PropertyPath::Permanent
        )
    }) {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let application_id = checkpoint_application_id(checkpoint, state)?;
    let current = client
        .redirects()
        .get(RedirectId::new(remote_id.as_str()))
        .await
        .map_err(|error| ApplyWorkspaceError::RemotePreparation {
            code: failure_code(&error),
        })?;
    if current.redirect_id.as_str() != remote_id.as_str()
        || current.application_id != application_id
    {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let regex = if selected_paths.contains(&PropertyPath::Regex) {
        required_string(checkpoint, &PropertyPath::Regex)?
    } else {
        current.regex.clone()
    };
    let replacement = if selected_paths.contains(&PropertyPath::Replacement) {
        required_string(checkpoint, &PropertyPath::Replacement)?
    } else {
        current.replacement.clone()
    };
    let permanent = if selected_paths.contains(&PropertyPath::Permanent) {
        required_bool(checkpoint, &PropertyPath::Permanent)?
    } else {
        current.permanent
    };
    let collection = client
        .redirects()
        .by_application(application_id)
        .await
        .map_err(|error| ApplyWorkspaceError::RemotePreparation {
            code: failure_code(&error),
        })?;
    if collection
        .redirects()
        .iter()
        .any(|other| other.redirect_id != current.redirect_id && other.regex == regex)
    {
        return Err(ApplyWorkspaceError::RemotePreparation {
            code: FailureCode::Validation,
        });
    }

    Ok(UpdateRedirect::new(
        RedirectId::new(remote_id.as_str()),
        regex,
        replacement,
        permanent,
    ))
}

/// Prepares a complete Security replacement from a fresh read.
///
/// Dokploy requires the username and password together and offers no way to
/// leave the password unchanged, so the declared password descriptor must be
/// available for every update. A username-only change with an unmanaged password
/// fails here, before any remote mutation or journal step.
async fn security_update_input(
    client: &Dokploy,
    compiled: &mut crate::desired::CompiledDesired,
    address: &ResourceAddress,
    checkpoint: &dokploy_core::ResourceCheckpoint,
    state: &StateFile,
    remote_id: &RemoteId,
    selected_paths: &[PropertyPath],
) -> Result<UpdateSecurity, ApplyWorkspaceError> {
    if selected_paths
        .iter()
        .any(|path| !matches!(path, PropertyPath::Username | PropertyPath::Password))
    {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let application_id = checkpoint_application_id(checkpoint, state)?;
    let current = client
        .security()
        .get(SecurityId::new(remote_id.as_str()))
        .await
        .map_err(|error| ApplyWorkspaceError::RemotePreparation {
            code: failure_code(&error),
        })?;
    if current.security_id.as_str() != remote_id.as_str()
        || current.application_id != application_id
    {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }
    let username = if selected_paths.contains(&PropertyPath::Username) {
        required_string(checkpoint, &PropertyPath::Username)?
    } else {
        current.username.clone()
    };
    let collection = client
        .security()
        .by_application(application_id)
        .await
        .map_err(|error| ApplyWorkspaceError::RemotePreparation {
            code: failure_code(&error),
        })?;
    if collection
        .entries()
        .iter()
        .any(|other| other.security_id != current.security_id && other.username == username)
    {
        return Err(ApplyWorkspaceError::RemotePreparation {
            code: FailureCode::Validation,
        });
    }
    let password = take_sensitive_string(compiled, address, &PropertyPath::Password)?;

    Ok(UpdateSecurity::new(
        SecurityId::new(remote_id.as_str()),
        username,
        password,
    ))
}

async fn execute_libsql_update(
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
    let final_resource = checkpoint.materialize(change.address(), remote_id.clone())?;
    let selected = change
        .fields()
        .iter()
        .map(|field| field.key())
        .collect::<Vec<_>>();
    if selected.iter().any(|path| {
        !matches!(
            path,
            PropertyPath::Description | PropertyPath::Username | PropertyPath::Password
        )
    }) {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    let metadata = selected
        .iter()
        .any(|path| matches!(path, PropertyPath::Description | PropertyPath::Username));
    if metadata {
        let mut input = UpdateLibSql::new(LibSqlId::new(remote_id.as_str()));
        if selected.contains(&&PropertyPath::Description) {
            input =
                input.with_description(required_string(checkpoint, &PropertyPath::Description)?);
        }
        if selected.contains(&&PropertyPath::Username) {
            input = input.with_username(required_string(checkpoint, &PropertyPath::Username)?);
        }
        let interim = ResourceState::try_new(
            ResourceKind::LibSql,
            remote_id.clone(),
            final_resource.is_protected(),
            final_resource.last_applied().clone(),
            before.sensitive_inputs().clone(),
            final_resource.containment().cloned(),
            final_resource.dependencies().to_vec(),
        )
        .map_err(|_| ApplyWorkspaceError::InvalidCheckpoint)?;
        let token = journal.start_recoverable_step(
            change.address().clone(),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, interim.clone())?,
        )?;
        if let Err(error) = client.libsql().update(input).await {
            let code = failure_code(&error);
            fail_if_definitive(journal, token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
        state.upsert_resource(change.address().clone(), interim)?;
        journal.succeed(token, Some(remote_id.clone()), state)?;
    }

    if selected.contains(&&PropertyPath::Password) {
        let before_password = state
            .resource(change.address())
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
            .clone();
        let password = take_sensitive_string(compiled, change.address(), &PropertyPath::Password)?;
        let token = journal.start_recoverable_step(
            change.address().clone(),
            JournalAction::Update,
            ExpectedCheckpoint::update(before_password, final_resource.clone())?,
        )?;
        if let Err(error) = client
            .libsql()
            .change_password(ChangeLibSqlPassword::new(
                LibSqlId::new(remote_id.as_str()),
                password,
            ))
            .await
        {
            let code = failure_code(&error);
            fail_if_definitive(journal, token, code)?;
            return Err(ApplyWorkspaceError::RemoteMutation { code });
        }
        state.upsert_resource(change.address().clone(), final_resource)?;
        journal.succeed(token, Some(remote_id), state)?;
    } else if metadata {
        let current = state
            .resource(change.address())
            .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?;
        if current != &final_resource {
            return Err(ApplyWorkspaceError::InvalidCheckpoint);
        }
    } else {
        return Err(ApplyWorkspaceError::InvalidCheckpoint);
    }

    Ok(())
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
    let before = state
        .resource(change.address())
        .ok_or(ApplyWorkspaceError::InvalidCheckpoint)?
        .clone();
    let remote_id = before.remote_id().clone();
    let resource = checkpoint.materialize(change.address(), remote_id.clone())?;

    if change.kind() == ChangeKind::NoOp {
        let token = journal.start_recoverable_step(
            change.address().clone(),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, resource.clone())?,
        )?;
        state.upsert_resource(change.address().clone(), resource)?;
        journal.succeed(token, Some(remote_id), state)?;
        return Ok(());
    }

    if change.address().kind() == ResourceKind::LibSql && change.kind() == ChangeKind::Update {
        return execute_libsql_update(client, compiled, change, state, journal).await;
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
        ResourceKind::Compose => ExistingMutation::Compose(compose_update_input(
            compiled,
            change.address(),
            checkpoint,
            ComposeId::new(remote_id.as_str()),
            &selected_paths,
        )?),
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
        ResourceKind::MySql => {
            let mut input = UpdateMySql::new(MySqlId::new(remote_id.as_str()));
            for path in &selected_paths {
                match path {
                    PropertyPath::Database => {
                        input = input.with_database(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Username => {
                        input = input.with_username(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Password | PropertyPath::RootPassword => {
                        return Err(ApplyWorkspaceError::UnsupportedChange);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }
            ExistingMutation::MySql(input)
        }
        ResourceKind::MariaDb => {
            let mut input = UpdateMariaDb::new(MariaDbId::new(remote_id.as_str()));
            for path in &selected_paths {
                match path {
                    PropertyPath::Database => {
                        input = input.with_database(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Username => {
                        input = input.with_username(required_string(checkpoint, path)?);
                    }
                    PropertyPath::Password | PropertyPath::RootPassword => {
                        return Err(ApplyWorkspaceError::UnsupportedChange);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }
            ExistingMutation::MariaDb(input)
        }
        ResourceKind::Mongo => {
            let mut input = UpdateMongo::new(MongoId::new(remote_id.as_str()));
            for path in &selected_paths {
                match path {
                    PropertyPath::Username => {
                        input = input.with_username(required_string(checkpoint, path)?);
                    }
                    PropertyPath::ReplicaSets => {
                        input = input.with_replica_sets(required_bool(checkpoint, path)?);
                    }
                    PropertyPath::Password => {
                        return Err(ApplyWorkspaceError::UnsupportedChange);
                    }
                    _ => return Err(ApplyWorkspaceError::InvalidCheckpoint),
                }
            }
            ExistingMutation::Mongo(input)
        }
        ResourceKind::LibSql => {
            return Err(ApplyWorkspaceError::UnsupportedChange);
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
        ResourceKind::Mount => return Err(ApplyWorkspaceError::UnsupportedChange),
        ResourceKind::Port => {
            if selected_paths.iter().any(|path| {
                !matches!(
                    path,
                    PropertyPath::PublishedPort
                        | PropertyPath::TargetPort
                        | PropertyPath::PublishMode
                        | PropertyPath::Protocol
                )
            }) {
                return Err(ApplyWorkspaceError::InvalidCheckpoint);
            }
            let application_id = checkpoint_application_id(checkpoint, state)?;
            let current = client
                .ports()
                .get(PortId::new(remote_id.as_str()))
                .await
                .map_err(|error| ApplyWorkspaceError::RemotePreparation {
                    code: failure_code(&error),
                })?;
            if current.port_id.as_str() != remote_id.as_str()
                || current.application_id != application_id
            {
                return Err(ApplyWorkspaceError::InvalidCheckpoint);
            }
            ExistingMutation::Port(port_update_input(checkpoint, &remote_id)?)
        }
        ResourceKind::Redirect => ExistingMutation::Redirect(
            redirect_update_input(client, checkpoint, state, &remote_id, &selected_paths).await?,
        ),
        ResourceKind::Security => ExistingMutation::Security(
            security_update_input(
                client,
                compiled,
                change.address(),
                checkpoint,
                state,
                &remote_id,
                &selected_paths,
            )
            .await?,
        ),
    };

    let token = journal.start_recoverable_step(
        change.address().clone(),
        JournalAction::Update,
        ExpectedCheckpoint::update(before, resource.clone())?,
    )?;
    if let Err(error) = mutation.execute(client).await {
        let code = failure_code(&error);
        fail_if_definitive(journal, token, code)?;
        return Err(ApplyWorkspaceError::RemoteMutation { code });
    }
    state.upsert_resource(change.address().clone(), resource.clone())?;
    journal.succeed(token, Some(remote_id.clone()), state)?;

    if change.address().kind() == ResourceKind::Application
        && application_requires_deploy(selected_paths.iter())
    {
        let token = journal.start_recoverable_step(
            change.address().clone(),
            JournalAction::Deploy,
            ExpectedCheckpoint::update(resource.clone(), resource.clone())?,
        )?;
        if let Err(error) = client
            .applications()
            .deploy(ApplicationId::new(remote_id.as_str()))
            .await
        {
            let code = failure_code(&error);
            fail_if_definitive(journal, token, code)?;
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
    Compose(UpdateCompose),
    Postgres(UpdatePostgres),
    MySql(UpdateMySql),
    MariaDb(UpdateMariaDb),
    Mongo(UpdateMongo),
    Redis(UpdateRedis),
    Domain(UpdateDomain),
    Port(UpdatePort),
    Redirect(UpdateRedirect),
    Security(UpdateSecurity),
}

impl ExistingMutation {
    async fn execute(self, client: &Dokploy) -> Result<(), SdkError> {
        match self {
            Self::Project(input) => client.projects().update(input).await,
            Self::Environment(input) => client.environments().update(input).await,
            Self::Application(input) => client.applications().update(input).await,
            Self::Compose(input) => client.composes().update(input).await,
            Self::Postgres(input) => client.postgres().update(input).await,
            Self::MySql(input) => client.mysql().update(input).await,
            Self::MariaDb(input) => client.mariadb().update(input).await,
            Self::Mongo(input) => client.mongo().update(input).await,
            Self::Redis(input) => client.redis().update(input).await,
            Self::Domain(input) => client.domains().update(input).await,
            Self::Port(input) => client.ports().update(input).await,
            Self::Redirect(input) => client.redirects().update(input).await,
            Self::Security(input) => client.security().update(input).await,
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

fn fail_if_definitive(
    journal: &mut OperationJournal<'_, '_>,
    token: StepToken,
    code: FailureCode,
) -> Result<(), JournalError> {
    if code == FailureCode::TransportOutcomeUnknown {
        return Ok(());
    }

    journal.fail(token, code)
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
    #[error("durable workspace state disappeared while destruction was starting")]
    StateDisappeared,
    #[error("desired configuration cannot be compiled safely")]
    Desired(#[from] CompileDesiredError),
    #[error("durable state cannot be projected into planner input")]
    StoredState(#[from] dokploy_core::StoredStateError),
    #[error("fresh Dokploy state cannot be discovered safely")]
    Remote(#[from] DiscoverRemoteError),
    #[error("the fresh plan is incomplete or blocked")]
    PlanBlocked,
    #[error("the saved plan cannot be applied safely")]
    SavedPlan(#[from] SavedPlanError),
    #[error("the saved plan remote binding key is unavailable or invalid")]
    SavedPlanBindingKey,
    #[error("apply was declined")]
    Declined,
    #[error("apply parallelism must be between 1 and 64")]
    InvalidParallelism,
    #[error("failed to read or render apply confirmation")]
    Approval {
        #[source]
        source: io::Error,
    },
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
    #[error("the journal recovery checkpoint is inconsistent with the planned change")]
    ExpectedCheckpoint(#[from] ExpectedCheckpointError),
    #[error("the planned checkpoint cannot become durable state")]
    Checkpoint(#[from] CheckpointMaterializationError),
    #[error("the next durable state is invalid")]
    State(#[from] StateError),
}
