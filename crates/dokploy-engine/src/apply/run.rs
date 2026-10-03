//! The steps of an apply.

use dokploy_core::{
    ChangeKind, ComparableValue, PlannedChange, PropertyObservation, PropertyPath,
    ResourceCheckpoint, ValueState,
};
use dokploy_sdk::{Error as SdkError, OperationRequest, Transport};
use dokploy_spec::{CreateIdentity, KindSpec};
use dokploy_state::{
    ExpectedCheckpoint, FailureCode, JournalAction, OperationJournal, RemoteId, ResourceAddress,
    StateFile, StepToken,
};
use serde_json::Value as Json;

use super::{ApplyError, request};
use crate::Engine;
use crate::compile::Compiled;
use crate::project::{item_id, project};
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
    let mut run = Run {
        engine,
        compiled,
        state,
        journal,
        selectors,
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
    applied: usize,
}

impl<'e, T: Transport> Run<'e, '_, '_, T> {
    async fn execute(&mut self, change: &PlannedChange) -> Result<(), ApplyError> {
        match change.kind() {
            ChangeKind::Create => self.create(change).await,
            ChangeKind::Update => self.update(change).await,
            ChangeKind::NoOp => self.adopt(change),
            ChangeKind::Delete | ChangeKind::Forget => self.remove(change).await,
            ChangeKind::Replace => {
                self.remove(change).await?;
                self.create(change).await
            }
            ChangeKind::Move => self.rename(change),
        }
    }

    fn inputs(&self) -> request::Inputs<'_> {
        request::Inputs {
            compiled: self.compiled,
            selectors: Some(&self.selectors),
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
        let created = request::create_request(
            spec,
            address,
            checkpoint,
            parent_id.as_deref(),
            self.inputs(),
        )?;
        let request = created.request;
        // What the create operation cannot carry is written by an update right after, as a
        // step of its own: the create step expects only what the create writes.
        let deferred = created.deferred;
        let at_creation = checkpoint.without(&deferred);

        // Learning the identity by diffing needs the collection as it was before.
        let before = match &spec.api.create_identity {
            Some(CreateIdentity::DiffCollection { .. }) => Some(
                self.listing(spec, parent_id.as_deref())
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
                    parent_id.as_deref(),
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
        let groups = request::groups(spec, address, written)?;

        // A full group re-sends what is there, so it starts from a fresh read.
        let mut bodies = Vec::new();
        for group in &groups {
            let fresh = if group.shape == dokploy_spec::Shape::Full {
                Some(
                    self.direct_read(spec, remote_id.as_str())
                        .await
                        .map_err(|code| ApplyError::Preparation {
                            address: address.clone(),
                            code,
                        })?,
                )
            } else {
                None
            };
            let body = request::group_body(
                spec,
                address,
                checkpoint,
                group,
                remote_id.as_str(),
                fresh.as_ref(),
                self.inputs(),
            )?;
            bodies.push(OperationRequest::new(&group.operation).body(body));
        }

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
        let action = if change.kind() == ChangeKind::Forget {
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
        let Some(one) = spec.api.read.one.as_ref() else {
            return false;
        };
        let request = OperationRequest::new(&one.op).query(one.id_param.clone(), remote_id);
        matches!(self.engine.transport.call(request).await, Ok(response) if response.is_object())
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
        parent_id: Option<&str>,
    ) -> Result<Vec<Json>, FailureCode> {
        crate::discover::read_collection(
            &self.engine.transport,
            &self.engine.specs,
            spec,
            parent_id,
        )
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
        parent_id: Option<&str>,
        checkpoint: &ResourceCheckpoint,
        key: &[String],
        before: Vec<Json>,
    ) -> Option<String> {
        let known: Vec<&str> = before
            .iter()
            .filter_map(|item| item_id(spec, item))
            .collect();
        let after = self.listing(spec, parent_id).await.ok()?;
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
            item_id(spec, item).is_some_and(|id| !known.contains(&id))
                && wanted.iter().all(|(name, value)| {
                    crate::project::field_value(spec, name, item).as_ref() == Some(value)
                })
        });
        match (matches.next(), matches.next()) {
            (Some(item), None) => item_id(spec, item).map(str::to_owned),
            _ => None,
        }
    }

    async fn direct_read(&self, spec: &KindSpec, remote_id: &str) -> Result<Json, FailureCode> {
        let one = spec.api.read.one.as_ref().ok_or(FailureCode::Validation)?;
        match self
            .engine
            .transport
            .call(OperationRequest::new(&one.op).query(one.id_param.clone(), remote_id))
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
        let observed = project(spec, &direct, &self.selectors);
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
