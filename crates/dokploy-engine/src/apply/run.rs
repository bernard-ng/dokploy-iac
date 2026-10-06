//! The steps of an apply.

use dokploy_core::{
    ChangeKind, ComparableValue, PlannedChange, PropertyObservation, PropertyPath,
    ResourceCheckpoint, ValueState,
};
use dokploy_sdk::{Error as SdkError, OperationRequest, Transport};
use dokploy_spec::{CreateIdentity, KindSpec};
use dokploy_state::{
    ExpectedCheckpoint, FailureCode, JournalAction, OperationJournal, RemoteId, ResourceAddress,
    ResourceState, StateFile, StepToken,
};
use serde_json::Value as Json;

use super::{ApplyError, request};
use crate::Engine;
use crate::compile::Compiled;
use crate::project::{list_field_value, list_item_id, project};
use crate::selectors::SelectorIndex;

/// What an apply did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplySummary {
    pub(crate) applied: usize,
}

impl ApplySummary {
    /// How many planned changes were applied and checkpointed.
    #[must_use]
    pub const fn applied(self) -> usize {
        self.applied
    }
}

/// The properties a change writes: everything it manages that is not being relinquished.
pub(crate) fn written(change: &PlannedChange) -> Vec<&PropertyPath> {
    change
        .fields()
        .iter()
        .filter(|field| field.desired() != ValueState::Unmanaged)
        .map(dokploy_core::FieldChange::key)
        .collect()
}

pub(crate) async fn run<T: Transport>(
    engine: &Engine<T>,
    compiled: &Compiled,
    plan: &dokploy_core::Plan,
    state: StateFile,
    journal: OperationJournal<'_, '_>,
) -> Result<ApplySummary, ApplyError> {
    // Selectors are resolved against what Dokploy holds now, once, before the first write.
    let kinds: Vec<String> = plan
        .changes()
        .iter()
        .map(|change| change.address().kind().as_str().to_owned())
        .collect();
    let targets = SelectorIndex::targets(&engine.specs, kinds.iter().map(String::as_str));
    let selectors = SelectorIndex::load(&engine.transport, &engine.specs, &targets).await;
    // The organization is read once, and only when a create is attached to it.
    let attached_to_organization = plan.changes().iter().any(|change| {
        engine
            .specs
            .get(change.address().kind().as_str())
            .and_then(|spec| spec.api.create.as_ref())
            .and_then(dokploy_spec::CreateOperation::single)
            .is_some_and(|create| {
                create
                    .attach
                    .values()
                    .any(|source| source == "organization_id")
            })
    });
    let organization = if attached_to_organization {
        engine
            .transport
            .call(OperationRequest::new("organization.active"))
            .await
            .ok()
            .and_then(|active| active.get("id").and_then(Json::as_str).map(str::to_owned))
    } else {
        None
    };
    let mut run = Run {
        engine,
        compiled,
        state,
        journal,
        selectors,
        organization,
        applied: 0,
    };
    for change in plan.changes() {
        match run.execute(change).await {
            Ok(()) => run.applied += 1,
            // The step was recorded and its state checkpointed; only the read-back differs.
            Err(error @ ApplyError::Verification { .. }) => {
                run.journal.commit()?;
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
    run.journal.commit()?;

    Ok(ApplySummary {
        applied: run.applied,
    })
}

struct Run<'e, 'j, 's, T> {
    engine: &'e Engine<T>,
    compiled: &'e Compiled,
    state: StateFile,
    journal: OperationJournal<'j, 's>,
    selectors: SelectorIndex,
    organization: Option<String>,
    applied: usize,
}

impl<'e, T: Transport> Run<'e, '_, '_, T> {
    async fn execute(&mut self, change: &PlannedChange) -> Result<(), ApplyError> {
        match change.kind() {
            ChangeKind::Create => self.create(change).await,
            ChangeKind::Adopt => self.adopt_existing(change).await,
            ChangeKind::Update => self.update(change).await,
            ChangeKind::NoOp => self.adopt(change),
            ChangeKind::Delete | ChangeKind::Forget => self.remove(change).await,
            ChangeKind::Replace => {
                self.remove(change).await?;
                self.create(change).await
            }
            ChangeKind::Move => {
                if change.move_action() == Some(dokploy_core::MoveAction::Update) {
                    self.rename_then_write(change).await
                } else {
                    self.rename(change)
                }
            }
        }
    }

    fn inputs(&self) -> request::Inputs<'_> {
        request::Inputs {
            compiled: self.compiled,
            selectors: Some(&self.selectors),
            organization: self.organization.as_deref(),
        }
    }

    fn spec(&self, address: &ResourceAddress) -> Result<&'e KindSpec, ApplyError> {
        let engine: &'e Engine<T> = self.engine;
        engine.specs.get(address.kind().as_str()).ok_or_else(|| {
            crate::EngineError::UnknownKind {
                kind: address.kind().as_str().to_owned(),
            }
            .into()
        })
    }

    fn parent_id(&self, checkpoint: &ResourceCheckpoint) -> Option<String> {
        checkpoint
            .containment()
            .and_then(|parent| self.state.resource(parent))
            .map(|parent| parent.remote_id().as_str().to_owned())
    }

    async fn create(&mut self, change: &PlannedChange) -> Result<(), ApplyError> {
        let address = change.address();
        let spec = self.spec(address)?;
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let checkpoint = change.checkpoint().present().ok_or_else(invalid)?;
        let parent_id = self.parent_id(checkpoint);
        let parent: Option<(String, String)> = checkpoint
            .containment()
            .map(|parent| parent.kind().as_str().to_owned())
            .zip(parent_id.clone());
        let created = request::create_request(
            spec,
            address,
            checkpoint,
            parent_id.as_deref(),
            self.inputs(),
        )?;
        let request = created.request;
        // Dokploy made this one with its parent: take it, and write the rest with an update.
        if let Some(id) = self
            .made_with_parent(spec, parent.as_ref(), checkpoint)
            .await
            .map_err(|code| ApplyError::Preparation {
                address: address.clone(),
                code,
            })?
        {
            return self.take_made_with_parent(change, spec, id).await;
        }
        // What the create operation cannot carry is written by an update right after, as a
        // step of its own: the create step expects only what the create writes.
        let deferred = created.deferred;
        let at_creation = checkpoint.without(&deferred);

        // Learning the identity by diffing needs the collection as it was before.
        let before = match &spec.api.create_identity {
            Some(CreateIdentity::DiffCollection { .. }) => Some(
                self.listing(
                    spec,
                    parent.as_ref().map(|(k, id)| (k.as_str(), id.as_str())),
                )
                .await
                .map_err(|code| ApplyError::Preparation {
                    address: address.clone(),
                    code,
                })?,
            ),
            _ => None,
        };

        let placeholder = RemoteId::new("recovery-pending").expect("a constant identity");
        let token = self.journal.start_recoverable_step(
            address.clone(),
            JournalAction::Create,
            ExpectedCheckpoint::create(
                at_creation
                    .materialize(address, placeholder)
                    .map_err(|_| invalid())?,
            )
            .map_err(|_| invalid())?,
        )?;
        let response = match self.engine.transport.call(request).await {
            Ok(response) => response,
            Err(error) => return self.failed(token, address, &error),
        };

        let identity = match &spec.api.create_identity {
            Some(CreateIdentity::FromResponse(pointer)) => response
                .pointer(pointer)
                .and_then(Json::as_str)
                .map(str::to_owned),
            Some(CreateIdentity::DiffCollection { key }) => {
                self.diff_identity(
                    spec,
                    parent.as_ref().map(|(k, id)| (k.as_str(), id.as_str())),
                    &at_creation,
                    key,
                    before.unwrap_or_default(),
                )
                .await
            }
            Some(CreateIdentity::None) | None => None,
        };
        // Without an identity the create may have happened: leave the step open for recovery.
        let Some(remote_id) = identity.and_then(|id| RemoteId::new(id).ok()) else {
            return Err(ApplyError::OutcomeUnknown {
                address: address.clone(),
            });
        };

        let resource = at_creation
            .materialize(address, remote_id.clone())
            .map_err(|_| invalid())?;
        self.state.upsert_resource(address.clone(), resource)?;
        self.journal
            .succeed(token, Some(remote_id.clone()), &self.state)?;
        self.record_selector_target(spec, address, checkpoint, &remote_id);

        let paths: Vec<&PropertyPath> = at_creation
            .property_paths()
            .into_iter()
            .filter(|path| !path.info().is_sensitive())
            .collect();
        self.verify(spec, address, remote_id.as_str(), &at_creation, &paths)
            .await?;

        if deferred.is_empty() {
            return Ok(());
        }
        let deferred: Vec<&PropertyPath> = deferred.iter().collect();
        self.write(address, checkpoint, &deferred).await
    }

    /// The row Dokploy made with the parent when it is the one this create names: the key fields of
    /// the checkpoint match a row the kind's marker flags.
    async fn made_with_parent(
        &self,
        spec: &KindSpec,
        parent: Option<&(String, String)>,
        checkpoint: &ResourceCheckpoint,
    ) -> Result<Option<String>, FailureCode> {
        if spec.made_with_parent.is_none() {
            return Ok(None);
        }
        let wanted: Option<Vec<(String, Json)>> = spec
            .identity
            .collision
            .iter()
            .map(|name| {
                let path = PropertyPath::from_spec(spec, name).ok()?;
                match checkpoint.property(&path)? {
                    dokploy_core::CheckpointValueRef::NonSensitive(value) => {
                        Some((name.clone(), value.clone()))
                    }
                    _ => None,
                }
            })
            .collect();
        let Some(wanted) = wanted else {
            return Ok(None);
        };
        let items = self
            .listing(spec, parent.map(|(k, id)| (k.as_str(), id.as_str())))
            .await?;
        let mut found = items.iter().filter(|item| {
            crate::discover::is_made_with_parent(spec, item)
                && wanted
                    .iter()
                    .all(|(name, value)| list_field_value(spec, name, item).as_ref() == Some(value))
        });

        Ok(match (found.next(), found.next()) {
            (Some(item), None) => list_item_id(spec, item).map(str::to_owned),
            _ => None,
        })
    }

    /// A create that takes the row Dokploy made instead of making one: the step is journaled as a
    /// create of what names it, and everything else is written by an update right after.
    async fn take_made_with_parent(
        &mut self,
        change: &PlannedChange,
        spec: &KindSpec,
        id: String,
    ) -> Result<(), ApplyError> {
        let address = change.address();
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let checkpoint = change.checkpoint().present().ok_or_else(invalid)?;
        let remote_id = RemoteId::new(id).map_err(|_| invalid())?;
        let deferred: Vec<PropertyPath> = checkpoint
            .property_paths()
            .into_iter()
            .filter(|path| {
                !spec
                    .identity
                    .collision
                    .iter()
                    .any(|name| name == path.info().field_name())
            })
            .cloned()
            .collect();
        let at_creation = checkpoint.without(&deferred);
        let placeholder = RemoteId::new("recovery-pending").expect("a constant identity");
        let token = self.journal.start_recoverable_step(
            address.clone(),
            JournalAction::Create,
            ExpectedCheckpoint::create(
                at_creation
                    .materialize(address, placeholder)
                    .map_err(|_| invalid())?,
            )
            .map_err(|_| invalid())?,
        )?;
        let resource = at_creation
            .materialize(address, remote_id.clone())
            .map_err(|_| invalid())?;
        self.state.upsert_resource(address.clone(), resource)?;
        self.journal
            .succeed(token, Some(remote_id.clone()), &self.state)?;
        self.record_selector_target(spec, address, checkpoint, &remote_id);
        if deferred.is_empty() {
            return Ok(());
        }
        let deferred: Vec<&PropertyPath> = deferred.iter().collect();
        self.write(address, checkpoint, &deferred).await
    }

    /// A resource that was just made is what a selector later in the apply may name: the index
    /// read before the first write does not have it.
    fn record_selector_target(
        &mut self,
        spec: &KindSpec,
        address: &ResourceAddress,
        checkpoint: &ResourceCheckpoint,
        remote_id: &RemoteId,
    ) {
        let Some(key) = spec.identity.key.as_deref() else {
            return;
        };
        let Ok(path) = PropertyPath::from_spec(spec, key) else {
            return;
        };
        if let Some(dokploy_core::CheckpointValueRef::NonSensitive(Json::String(name))) =
            checkpoint.property(&path)
        {
            self.selectors
                .record(address.kind().as_str(), remote_id.as_str(), name);
        }
    }

    async fn update(&mut self, change: &PlannedChange) -> Result<(), ApplyError> {
        let address = change.address();
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let checkpoint = change.checkpoint().present().ok_or_else(invalid)?;
        let written = written(change);
        if written.is_empty() {
            return self.adopt(change);
        }
        self.write(address, checkpoint, &written).await
    }

    /// Writes properties of an existing resource with the spec's write groups, as one
    /// journaled step that moves the state to `checkpoint`.
    async fn write(
        &mut self,
        address: &ResourceAddress,
        checkpoint: &ResourceCheckpoint,
        written: &[&PropertyPath],
    ) -> Result<(), ApplyError> {
        let spec = self.spec(address)?;
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let before = self.state.resource(address).ok_or_else(invalid)?.clone();
        let remote_id = before.remote_id().clone();
        let bodies = self
            .update_requests(spec, address, checkpoint, written, remote_id.as_str())
            .await?;

        let resource = checkpoint
            .materialize(address, remote_id.clone())
            .map_err(|_| invalid())?;
        let token = self.journal.start_recoverable_step(
            address.clone(),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, resource.clone()).map_err(|_| invalid())?,
        )?;
        for request in bodies {
            if let Err(error) = self.engine.transport.call(request).await {
                return self.failed(token, address, &error);
            }
        }
        self.state.upsert_resource(address.clone(), resource)?;
        self.journal
            .succeed(token, Some(remote_id.clone()), &self.state)?;

        let paths: Vec<&PropertyPath> = written
            .iter()
            .copied()
            .filter(|path| !path.info().is_sensitive())
            .collect();
        self.verify(spec, address, remote_id.as_str(), checkpoint, &paths)
            .await
    }

    async fn update_requests(
        &self,
        spec: &KindSpec,
        address: &ResourceAddress,
        checkpoint: &ResourceCheckpoint,
        written: &[&PropertyPath],
        remote_id: &str,
    ) -> Result<Vec<OperationRequest>, ApplyError> {
        let groups = request::groups(spec, address, written)?;

        // A full group re-sends what is there, so it starts from a fresh read.
        let mut bodies = Vec::new();
        for group in &groups {
            let fresh = if group.reads_fresh {
                Some(self.direct_read(spec, remote_id).await.map_err(|code| {
                    ApplyError::Preparation {
                        address: address.clone(),
                        code,
                    }
                })?)
            } else {
                None
            };
            let mut body = request::group_body(
                spec,
                address,
                checkpoint,
                group,
                remote_id,
                fresh.as_ref(),
                self.inputs(),
            )?;
            let operation = request::group_operation(spec, address, checkpoint, group)?;
            request::fix_update_body(spec, checkpoint, &operation, &mut body);
            bodies.push(OperationRequest::new(&operation).body(body));
        }

        // The members of a relation are added and removed one request each, in the same step.
        let held = if request::relation_needs_read(spec, written, checkpoint) {
            Some(self.direct_read(spec, remote_id).await.map_err(|code| {
                ApplyError::Preparation {
                    address: address.clone(),
                    code,
                }
            })?)
        } else {
            None
        };
        bodies.extend(request::membership_requests(
            spec,
            address,
            checkpoint,
            written,
            remote_id,
            held.as_ref(),
            self.inputs(),
        )?);

        Ok(bodies)
    }

    /// Enrolls a remote resource that the tool may update but never create or destroy.
    async fn adopt_existing(&mut self, change: &PlannedChange) -> Result<(), ApplyError> {
        let address = change.address();
        let spec = self.spec(address)?;
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let checkpoint = change.checkpoint().present().ok_or_else(invalid)?;
        let remote_id = change.adopted_remote_id().ok_or_else(invalid)?.clone();
        let written = written(change);
        let bodies = self
            .update_requests(spec, address, checkpoint, &written, remote_id.as_str())
            .await?;
        let resource = checkpoint
            .materialize(address, remote_id.clone())
            .map_err(|_| invalid())?;
        let token = self.journal.start_recoverable_step(
            address.clone(),
            JournalAction::Create,
            ExpectedCheckpoint::create(resource.clone()).map_err(|_| invalid())?,
        )?;
        for request in bodies {
            if let Err(error) = self.engine.transport.call(request).await {
                return self.failed(token, address, &error);
            }
        }
        self.state.upsert_resource(address.clone(), resource)?;
        self.journal
            .succeed(token, Some(remote_id.clone()), &self.state)?;
        self.record_selector_target(spec, address, checkpoint, &remote_id);

        let paths: Vec<&PropertyPath> = written
            .iter()
            .copied()
            .filter(|path| !path.info().is_sensitive())
            .collect();
        self.verify(spec, address, remote_id.as_str(), checkpoint, &paths)
            .await
    }

    /// A state-only checkpoint: the remote already says what the document says.
    fn adopt(&mut self, change: &PlannedChange) -> Result<(), ApplyError> {
        let address = change.address();
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let checkpoint = change.checkpoint().present().ok_or_else(invalid)?;
        let before = self.state.resource(address).ok_or_else(invalid)?.clone();
        let remote_id = before.remote_id().clone();
        let resource = checkpoint
            .materialize(address, remote_id.clone())
            .map_err(|_| invalid())?;
        let token = self.journal.start_recoverable_step(
            address.clone(),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, resource.clone()).map_err(|_| invalid())?,
        )?;
        self.state.upsert_resource(address.clone(), resource)?;
        self.journal.succeed(token, Some(remote_id), &self.state)?;

        Ok(())
    }

    async fn remove(&mut self, change: &PlannedChange) -> Result<(), ApplyError> {
        let address = change.address();
        let spec = self.spec(address)?;
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let before = self.state.resource(address).ok_or_else(invalid)?.clone();
        // Dokploy refuses to delete what it made with the parent (a project's default
        // environment) and removes it with the parent, so the document stops managing it.
        let made_with_parent = change.kind() != ChangeKind::Forget
            && spec.made_with_parent.is_some()
            && self
                .direct_read(spec, before.remote_id().as_str())
                .await
                .is_ok_and(|item| crate::discover::is_made_with_parent(spec, &item));
        let action = if change.kind() == ChangeKind::Forget || made_with_parent {
            JournalAction::Forget
        } else {
            JournalAction::Delete
        };
        let request = if action == JournalAction::Delete {
            Some(request::remove_request(
                spec,
                address,
                before.remote_id().as_str(),
            )?)
        } else {
            None
        };
        let token = self.journal.start_recoverable_step(
            address.clone(),
            action,
            ExpectedCheckpoint::remove(before.clone()),
        )?;
        if let Some(request) = request
            && let Err(error) = self.engine.transport.call(request).await
            && !matches!(&error, SdkError::Api(detail) if detail.status() == 404)
        {
            return self.failed(token, address, &error);
        }
        // An acknowledged removal that leaves the resource readable did not happen: the step
        // fails and the resource stays tracked, so the next plan removes it again.
        if action == JournalAction::Delete
            && self.still_there(spec, before.remote_id().as_str()).await
        {
            self.journal.fail(token, FailureCode::RemoteRejected)?;
            return Err(ApplyError::NotRemoved {
                address: address.clone(),
            });
        }
        self.state.remove_resource(address)?;
        self.journal.succeed(token, None, &self.state)?;

        Ok(())
    }

    /// Whether the resource can still be read. An error other than a success proves nothing
    /// either way: the remove was acknowledged, so it is taken as gone.
    async fn still_there(&self, spec: &KindSpec, remote_id: &str) -> bool {
        self.direct_read(spec, remote_id).await.is_ok()
    }

    /// A rename that also changes properties: the address moves first, with the resource as state
    /// has it, and then the properties are written as an update of the resource at its new address.
    async fn rename_then_write(&mut self, change: &PlannedChange) -> Result<(), ApplyError> {
        let address = change.address();
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let source = change.previous_address().ok_or_else(invalid)?;
        let target = change.checkpoint().move_target().ok_or_else(invalid)?;
        let before = self.state.resource(source).ok_or_else(invalid)?.clone();
        if self.state.resource(address).is_some() {
            return Err(invalid());
        }
        let moved = ResourceState::try_new(
            before.kind(),
            before.remote_id().clone(),
            before.is_protected(),
            before.last_applied().clone(),
            before.sensitive_inputs().clone(),
            target.containment().cloned(),
            before.dependencies().to_vec(),
        )
        .map_err(|_| invalid())?;
        let token = self.journal.start_recoverable_step(
            address.clone(),
            JournalAction::Move,
            ExpectedCheckpoint::move_resource(source.clone(), before, moved)
                .map_err(|_| invalid())?,
        )?;
        self.state.move_resource(source, address.clone())?;
        self.journal.succeed(token, None, &self.state)?;

        let written = written(change);
        if written.is_empty() {
            return self.adopt_target(address, target);
        }
        self.write(address, target, &written).await
    }

    /// The target of a rename that writes nothing, recorded as it is.
    fn adopt_target(
        &mut self,
        address: &ResourceAddress,
        target: &ResourceCheckpoint,
    ) -> Result<(), ApplyError> {
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let before = self.state.resource(address).ok_or_else(invalid)?.clone();
        let remote_id = before.remote_id().clone();
        let resource = target
            .materialize(address, remote_id.clone())
            .map_err(|_| invalid())?;
        if resource == before {
            return Ok(());
        }
        let token = self.journal.start_recoverable_step(
            address.clone(),
            JournalAction::Update,
            ExpectedCheckpoint::update(before, resource.clone()).map_err(|_| invalid())?,
        )?;
        self.state.upsert_resource(address.clone(), resource)?;
        self.journal.succeed(token, Some(remote_id), &self.state)?;

        Ok(())
    }

    /// A rename: only the logical address changes.
    fn rename(&mut self, change: &PlannedChange) -> Result<(), ApplyError> {
        let address = change.address();
        let invalid = || ApplyError::InvalidCheckpoint {
            address: address.clone(),
        };
        let source = change.previous_address().ok_or_else(invalid)?;
        let target = change.checkpoint().move_target().ok_or_else(invalid)?;
        let before = self.state.resource(source).ok_or_else(invalid)?.clone();
        if self.state.resource(address).is_some() {
            return Err(invalid());
        }
        let remote_id = before.remote_id().clone();
        let source_target = target
            .materialize(source, remote_id.clone())
            .map_err(|_| invalid())?;
        if before != source_target {
            let token = self.journal.start_recoverable_step(
                source.clone(),
                JournalAction::Update,
                ExpectedCheckpoint::update(before, source_target.clone()).map_err(|_| invalid())?,
            )?;
            self.state.upsert_resource(source.clone(), source_target)?;
            self.journal
                .succeed(token, Some(remote_id.clone()), &self.state)?;
        }
        let exact = target
            .materialize(address, remote_id)
            .map_err(|_| invalid())?;
        let move_before = self.state.resource(source).ok_or_else(invalid)?.clone();
        let token = self.journal.start_recoverable_step(
            address.clone(),
            JournalAction::Move,
            ExpectedCheckpoint::move_resource(source.clone(), move_before, exact)
                .map_err(|_| invalid())?,
        )?;
        self.state.move_resource(source, address.clone())?;
        self.journal.succeed(token, None, &self.state)?;

        Ok(())
    }

    /// A transport error on a mutation: unknown outcomes keep the step open for recovery;
    /// anything else is a definitive rejection.
    fn failed(
        &mut self,
        token: StepToken,
        address: &ResourceAddress,
        error: &SdkError,
    ) -> Result<(), ApplyError> {
        let code = failure_code(error);
        if code == FailureCode::TransportOutcomeUnknown {
            return Err(ApplyError::OutcomeUnknown {
                address: address.clone(),
            });
        }
        self.journal.fail(token, code)?;

        Err(ApplyError::Rejected {
            address: address.clone(),
            code,
        })
    }

    /// The collection the kind's resources are found in, read fresh.
    async fn listing(
        &self,
        spec: &KindSpec,
        parent: Option<(&str, &str)>,
    ) -> Result<Vec<Json>, FailureCode> {
        crate::discover::read_collection(&self.engine.transport, &self.engine.specs, spec, parent)
            .await
            .map_err(|kind| match kind {
                dokploy_core::RemoteFailureKind::Unauthorized => FailureCode::Unauthorized,
                dokploy_core::RemoteFailureKind::InvalidResponse => FailureCode::Validation,
                _ => FailureCode::Internal,
            })
    }

    /// The one new item whose key fields match what was created.
    async fn diff_identity(
        &self,
        spec: &KindSpec,
        parent: Option<(&str, &str)>,
        checkpoint: &ResourceCheckpoint,
        key: &[String],
        before: Vec<Json>,
    ) -> Option<String> {
        let known: Vec<&str> = before
            .iter()
            .filter_map(|item| list_item_id(spec, item))
            .collect();
        let after = self.listing(spec, parent).await.ok()?;
        let wanted: Vec<(String, Json)> = key
            .iter()
            .map(|name| {
                let path = PropertyPath::from_spec(spec, name).ok()?;
                match checkpoint.property(&path)? {
                    dokploy_core::CheckpointValueRef::NonSensitive(value) => {
                        Some((name.clone(), value.clone()))
                    }
                    _ => None,
                }
            })
            .collect::<Option<_>>()?;
        let mut matches = after.iter().filter(|item| {
            list_item_id(spec, item).is_some_and(|id| !known.contains(&id))
                && wanted
                    .iter()
                    .all(|(name, value)| list_field_value(spec, name, item).as_ref() == Some(value))
        });
        match (matches.next(), matches.next()) {
            (Some(item), None) => list_item_id(spec, item).map(str::to_owned),
            _ => None,
        }
    }

    async fn direct_read(&self, spec: &KindSpec, remote_id: &str) -> Result<Json, FailureCode> {
        let one = spec.api.read.one.as_ref().ok_or(FailureCode::Validation)?;
        let query_id = if let Some(pointer) = one.id_from.as_deref() {
            let items = self.listing(spec, None).await?;
            let mut matches = items
                .iter()
                .filter(|item| list_item_id(spec, item) == Some(remote_id));
            let Some(item) = matches.next().filter(|_| matches.next().is_none()) else {
                return Err(FailureCode::Validation);
            };
            crate::project::lookup(item, pointer)
                .and_then(Json::as_str)
                .map(str::to_owned)
                .ok_or(FailureCode::Validation)?
        } else {
            remote_id.to_owned()
        };
        match self
            .engine
            .transport
            .call(OperationRequest::new(&one.op).query(one.id_param.clone(), query_id))
            .await
        {
            Ok(value) if value.is_object() => Ok(value),
            Ok(_) => Err(FailureCode::TransportOutcomeUnknown),
            Err(error) => Err(failure_code(&error)),
        }
    }

    /// Re-reads the resource and checks that every public property just written reads back
    /// as written (invariant 22 as a per-step check). A mismatch is reported, never retried.
    async fn verify(
        &self,
        spec: &KindSpec,
        address: &ResourceAddress,
        remote_id: &str,
        checkpoint: &ResourceCheckpoint,
        paths: &[&PropertyPath],
    ) -> Result<(), ApplyError> {
        if spec.api.read.one.is_none() || paths.is_empty() {
            return Ok(());
        }
        let direct =
            self.direct_read(spec, remote_id)
                .await
                .map_err(|code| ApplyError::Preparation {
                    address: address.clone(),
                    code,
                })?;
        // The members of a set of selectors are named by the document, so they are observed by
        // name: the ones it keeps, and the ones it dropped.
        let members: Vec<PropertyPath> = paths
            .iter()
            .filter(|path| path.info().root.is_some() && path.info().selector.is_some())
            .map(|path| (*path).clone())
            .collect();
        let observed = project(spec, &direct, &self.selectors, &members);
        for path in paths {
            let matches = match (checkpoint.property(path), observed.get(*path)) {
                (
                    Some(dokploy_core::CheckpointValueRef::NonSensitive(value)),
                    Some(PropertyObservation::Known(seen)),
                ) => ComparableValue::try_from_json(value.clone()).is_ok_and(|want| &want == seen),
                (
                    Some(dokploy_core::CheckpointValueRef::Null),
                    Some(PropertyObservation::KnownAbsent),
                ) => true,
                (
                    Some(dokploy_core::CheckpointValueRef::EmptyCollection),
                    Some(PropertyObservation::Known(seen)),
                ) => ComparableValue::try_from_json(serde_json::json!({}))
                    .is_ok_and(|empty| &empty == seen),
                _ => false,
            };
            if !matches {
                return Err(ApplyError::Verification {
                    address: address.clone(),
                    property: path.to_string(),
                });
            }
        }

        Ok(())
    }
}

fn failure_code(error: &SdkError) -> FailureCode {
    match error {
        SdkError::Api(detail) if matches!(detail.status(), 401 | 403) => FailureCode::Unauthorized,
        SdkError::Api(detail) if matches!(detail.status(), 400 | 422) => FailureCode::Validation,
        SdkError::Api(_) => FailureCode::RemoteRejected,
        SdkError::OutcomeUnknown { .. }
        | SdkError::Decode { .. }
        | SdkError::UnexpectedResponse { .. } => FailureCode::TransportOutcomeUnknown,
        SdkError::InvalidRequest { .. } => FailureCode::Validation,
        SdkError::Request { .. } => FailureCode::Internal,
    }
}
