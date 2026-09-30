use std::collections::{BTreeMap, BTreeSet};

use dokploy_state::ResourceAddress;

use crate::dependency::{DependencyGraphKind, DependencyOrdering};
use crate::{
    ChangeKind, ChangeOrigin, CheckpointTarget, DesiredResource, DesiredState, DriftChange,
    DriftKind, FieldChange, MetadataChangeKind, MoveAction, OwnedValue, Plan, PlanDiagnostic,
    PlanDiagnosticCode, PlannedChange, PropertyObservation, PropertyPath, PropertyUnknownReason,
    ProtectionIntent, RemoteObservation, RemoteResource, RemoteState, ResourceCheckpoint,
    StoredState, UnsupportedDirectiveKind, ValueState,
    plan::PLAN_FORMAT_VERSION,
    snapshot::{StoredResource, owned_source_shape_valid},
};

/// Computes a plan without I/O or mutation.
#[must_use]
pub fn plan(desired: &DesiredState, stored: &StoredState, remote: &RemoteState) -> Plan {
    if stored.instance != remote.instance {
        return finish_plan(
            desired,
            stored,
            Vec::new(),
            Vec::new(),
            vec![diagnostic(PlanDiagnosticCode::InstanceMismatch, None)],
        );
    }

    let directives = match ValidatedDirectives::validate(desired, stored) {
        Ok(directives) => directives,
        Err(directive_diagnostics) => {
            return finish_plan(
                desired,
                stored,
                Vec::new(),
                Vec::new(),
                directive_diagnostics,
            );
        }
    };

    let ordering = match DependencyOrdering::analyze(desired, stored) {
        Ok(ordering) => ordering,
        Err(cycles) => {
            let diagnostics = cycles
                .into_iter()
                .map(|cycle| {
                    diagnostic(
                        match cycle.graph {
                            DependencyGraphKind::Desired => {
                                PlanDiagnosticCode::DesiredDependencyCycle
                            }
                            DependencyGraphKind::StoredRemoval => {
                                PlanDiagnosticCode::StoredDependencyCycle
                            }
                        },
                        Some(cycle.address),
                    )
                })
                .collect();
            return finish_plan(desired, stored, Vec::new(), Vec::new(), diagnostics);
        }
    };

    let mut changes = Vec::new();
    let mut drift = Vec::new();
    let mut diagnostics = Vec::new();
    for (source, target) in &directives.moves {
        if !directives.pending_moves.contains(source) {
            continue;
        }
        plan_move(
            source,
            target,
            desired,
            stored,
            remote,
            &mut changes,
            &mut drift,
            &mut diagnostics,
        );
    }
    for (address, destroy) in &directives.removals {
        if !directives.pending_removals.contains(address) {
            continue;
        }
        plan_explicit_removal(
            address,
            *destroy,
            stored,
            remote,
            &mut changes,
            &mut drift,
            &mut diagnostics,
        );
    }
    let relevant_addresses: BTreeSet<_> = desired
        .resources
        .keys()
        .chain(stored.resources.keys())
        .cloned()
        .collect();

    for address in relevant_addresses {
        if directives.handled.contains(&address) {
            continue;
        }
        let desired_resource = desired.resources.get(&address);
        let stored_resource = stored.resources.get(&address);
        let Some(observation) = remote.observation(&address) else {
            diagnostics.push(diagnostic(
                PlanDiagnosticCode::MissingObservation,
                Some(address),
            ));
            continue;
        };

        match observation {
            RemoteObservation::Unavailable(failure) => {
                let mut issue = diagnostic(PlanDiagnosticCode::RemoteUnavailable, Some(address));
                issue.remote_failure = Some(*failure);
                diagnostics.push(issue);
            }
            RemoteObservation::Missing => plan_missing_resource(
                address,
                desired_resource,
                stored_resource,
                &mut changes,
                &mut drift,
                &mut diagnostics,
            ),
            RemoteObservation::Present(remote_resource) => plan_present_resource(
                address,
                desired_resource,
                stored_resource,
                remote_resource,
                &mut changes,
                &mut drift,
                &mut diagnostics,
            ),
        }
    }

    ordering.order_changes(&mut changes);
    finish_plan(desired, stored, changes, drift, diagnostics)
}

struct ValidatedDirectives {
    moves: BTreeMap<ResourceAddress, ResourceAddress>,
    pending_moves: BTreeSet<ResourceAddress>,
    removals: BTreeMap<ResourceAddress, bool>,
    pending_removals: BTreeSet<ResourceAddress>,
    handled: BTreeSet<ResourceAddress>,
}

impl ValidatedDirectives {
    fn validate(desired: &DesiredState, stored: &StoredState) -> Result<Self, Vec<PlanDiagnostic>> {
        let mut diagnostics = Vec::new();
        let mut moves = BTreeMap::new();
        let mut pending_moves = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for directive in &desired.moves {
            let from = directive.from();
            let to = directive.to();
            let invalid = from == to
                || from.kind() != to.kind()
                || desired.resources.contains_key(from)
                || !desired.resources.contains_key(to)
                || moves.contains_key(from)
                || !targets.insert(to.clone());
            if invalid {
                diagnostics.push(move_diagnostic(
                    PlanDiagnosticCode::InvalidMoveDirective,
                    from,
                    to,
                ));
                continue;
            }
            match (
                stored.resources.contains_key(from),
                stored.resources.contains_key(to),
            ) {
                (true, false) => {
                    pending_moves.insert(from.clone());
                }
                (false, true) => {}
                (false, false) => diagnostics.push(move_diagnostic(
                    PlanDiagnosticCode::MoveSourceMissing,
                    from,
                    to,
                )),
                (true, true) => diagnostics.push(move_diagnostic(
                    PlanDiagnosticCode::MoveTargetCollision,
                    from,
                    to,
                )),
            }
            moves.insert(from.clone(), to.clone());
        }

        if let Some((source, target)) = moves.iter().find(|(source, _)| targets.contains(*source)) {
            diagnostics.push(move_diagnostic(
                PlanDiagnosticCode::InvalidMoveDirective,
                source,
                target,
            ));
        }

        let mut removals = BTreeMap::new();
        let mut pending_removals = BTreeSet::new();
        for directive in &desired.removals {
            let address = directive.address();
            if let Some((source, target)) = moves
                .iter()
                .find(|(source, target)| *source == address || *target == address)
            {
                diagnostics.push(move_diagnostic(
                    PlanDiagnosticCode::InvalidMoveDirective,
                    source,
                    target,
                ));
                continue;
            }
            if desired.resources.contains_key(address) || removals.contains_key(address) {
                diagnostics.push(diagnostic(
                    PlanDiagnosticCode::InvalidRemovalDirective,
                    Some(address.clone()),
                ));
                continue;
            }
            if stored.resources.contains_key(address) {
                pending_removals.insert(address.clone());
            }
            removals.insert(address.clone(), directive.destroy());
        }

        if !diagnostics.is_empty() {
            return Err(diagnostics);
        }
        let handled = moves
            .iter()
            .flat_map(|(from, to)| {
                if pending_moves.contains(from) {
                    vec![from.clone(), to.clone()]
                } else {
                    vec![from.clone()]
                }
            })
            .chain(pending_removals.iter().cloned())
            .collect();
        Ok(Self {
            moves,
            pending_moves,
            removals,
            pending_removals,
            handled,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_move(
    source: &ResourceAddress,
    target: &ResourceAddress,
    desired: &DesiredState,
    stored: &StoredState,
    remote: &RemoteState,
    changes: &mut Vec<PlannedChange>,
    drift: &mut Vec<DriftChange>,
    diagnostics: &mut Vec<PlanDiagnostic>,
) {
    let Some(source_observation) = remote.observation(source) else {
        diagnostics.push(move_diagnostic(
            PlanDiagnosticCode::MissingObservation,
            source,
            target,
        ));
        return;
    };
    let Some(target_observation) = remote.observation(target) else {
        diagnostics.push(move_diagnostic(
            PlanDiagnosticCode::MissingObservation,
            source,
            target,
        ));
        return;
    };
    if let RemoteObservation::Unavailable(failure) = source_observation {
        let mut issue = move_diagnostic(PlanDiagnosticCode::RemoteUnavailable, source, target);
        issue.remote_failure = Some(*failure);
        diagnostics.push(issue);
        return;
    }
    if let RemoteObservation::Unavailable(failure) = target_observation {
        let mut issue = move_diagnostic(PlanDiagnosticCode::RemoteUnavailable, source, target);
        issue.remote_failure = Some(*failure);
        diagnostics.push(issue);
        return;
    }
    let RemoteObservation::Present(remote_resource) = source_observation else {
        diagnostics.push(move_diagnostic(
            PlanDiagnosticCode::MoveSourceMissing,
            source,
            target,
        ));
        return;
    };
    if !matches!(target_observation, RemoteObservation::Missing) {
        diagnostics.push(move_diagnostic(
            PlanDiagnosticCode::MoveTargetCollision,
            source,
            target,
        ));
        return;
    }

    let stored_resource = &stored.resources[source];
    if stored_resource.remote_id != *remote_resource.remote_id() {
        diagnostics.push(move_diagnostic(
            PlanDiagnosticCode::RemoteIdentityMismatch,
            source,
            target,
        ));
        return;
    }
    let desired_resource = &desired.resources[target];
    let diagnostic_start = diagnostics.len();
    if add_unsupported_resource_diagnostics(target, desired_resource, diagnostics) {
        for issue in &mut diagnostics[diagnostic_start..] {
            issue.address = Some(source.clone());
            issue.related_address = Some(target.clone());
        }
        return;
    }
    let mut property_diagnostics =
        required_property_diagnostics(target, desired_resource, remote_resource);
    if !property_diagnostics.is_empty() {
        for issue in &mut property_diagnostics {
            issue.address = Some(source.clone());
            issue.related_address = Some(target.clone());
        }
        diagnostics.append(&mut property_diagnostics);
        return;
    }

    let property_plan = compare_properties(desired_resource, stored_resource, remote_resource);
    if let Some(property) = invalid_ignored_checkpoint(desired_resource, &property_plan) {
        let mut issue =
            move_diagnostic(PlanDiagnosticCode::InvalidIgnoredCheckpoint, source, target);
        issue.property = Some(property);
        diagnostics.push(issue);
        return;
    }
    let (metadata, _) = metadata_changes(desired_resource, stored_resource);
    if !property_plan.drifted.is_empty() {
        drift.push(DriftChange {
            address: target.clone(),
            kind: DriftKind::Updated,
            properties: property_plan.drifted.clone(),
        });
    }
    changes.push(
        PlannedChange::move_change(
            source.clone(),
            target.clone(),
            if property_plan.convergence_required {
                MoveAction::Update
            } else {
                MoveAction::StateOnly
            },
            if property_plan.remote_changed {
                ChangeOrigin::ConfigAndDrift
            } else {
                ChangeOrigin::Config
            },
            property_plan.fields,
            metadata,
            resource_checkpoint_for_properties(
                desired_resource,
                Some(stored_resource),
                property_plan.checkpoint_properties,
            ),
        )
        .preserving(desired_resource.ignore_changes.clone()),
    );
}

#[allow(clippy::too_many_arguments)]
fn plan_explicit_removal(
    address: &ResourceAddress,
    destroy: bool,
    stored: &StoredState,
    remote: &RemoteState,
    changes: &mut Vec<PlannedChange>,
    drift: &mut Vec<DriftChange>,
    diagnostics: &mut Vec<PlanDiagnostic>,
) {
    let Some(observation) = remote.observation(address) else {
        diagnostics.push(diagnostic(
            PlanDiagnosticCode::MissingObservation,
            Some(address.clone()),
        ));
        return;
    };
    match observation {
        RemoteObservation::Unavailable(failure) => {
            let mut issue =
                diagnostic(PlanDiagnosticCode::RemoteUnavailable, Some(address.clone()));
            issue.remote_failure = Some(*failure);
            diagnostics.push(issue);
        }
        RemoteObservation::Missing => {
            drift.push(DriftChange {
                address: address.clone(),
                kind: DriftKind::Deleted,
                properties: Vec::new(),
            });
            changes.push(PlannedChange::resource(
                address.clone(),
                ChangeKind::Forget,
                ChangeOrigin::ConfigAndDrift,
                Vec::new(),
                Vec::new(),
                CheckpointTarget::Absent,
            ));
        }
        RemoteObservation::Present(remote_resource) => {
            let stored_resource = &stored.resources[address];
            if stored_resource.remote_id != *remote_resource.remote_id() {
                diagnostics.push(diagnostic(
                    PlanDiagnosticCode::RemoteIdentityMismatch,
                    Some(address.clone()),
                ));
                return;
            }
            if destroy {
                plan_present_delete(
                    address.clone(),
                    stored_resource,
                    remote_resource,
                    changes,
                    drift,
                    diagnostics,
                );
            } else {
                changes.push(PlannedChange::resource(
                    address.clone(),
                    ChangeKind::Forget,
                    ChangeOrigin::Config,
                    Vec::new(),
                    Vec::new(),
                    CheckpointTarget::Absent,
                ));
            }
        }
    }
}

fn plan_missing_resource(
    address: ResourceAddress,
    desired: Option<&DesiredResource>,
    stored: Option<&StoredResource>,
    changes: &mut Vec<PlannedChange>,
    drift: &mut Vec<DriftChange>,
    diagnostics: &mut Vec<PlanDiagnostic>,
) {
    match (desired, stored) {
        (Some(desired), None) => {
            if add_unsupported_resource_diagnostics(&address, desired, diagnostics) {
                return;
            }

            changes.push(PlannedChange::resource(
                address,
                ChangeKind::Create,
                ChangeOrigin::Config,
                create_field_changes(desired),
                desired_metadata_for_create(desired),
                checkpoint_for_desired(desired, None),
            ));
        }
        (Some(desired), Some(stored)) => {
            if add_unsupported_resource_diagnostics(&address, desired, diagnostics) {
                return;
            }

            let (fields, config_changed, checkpoint_properties) =
                missing_resource_field_changes(desired, stored);
            if let Some(property) = invalid_ignored_properties(desired, &checkpoint_properties) {
                let mut issue =
                    diagnostic(PlanDiagnosticCode::InvalidIgnoredCheckpoint, Some(address));
                issue.property = Some(property);
                diagnostics.push(issue);
                return;
            }
            let (metadata, metadata_changed) = metadata_changes(desired, stored);
            drift.push(DriftChange {
                address: address.clone(),
                kind: DriftKind::Deleted,
                properties: Vec::new(),
            });
            changes.push(PlannedChange::resource(
                address,
                ChangeKind::Create,
                if config_changed || metadata_changed {
                    ChangeOrigin::ConfigAndDrift
                } else {
                    ChangeOrigin::Drift
                },
                fields,
                metadata,
                CheckpointTarget::Present(resource_checkpoint_for_properties(
                    desired,
                    Some(stored),
                    checkpoint_properties,
                )),
            ));
        }
        (None, Some(_)) => {
            drift.push(DriftChange {
                address: address.clone(),
                kind: DriftKind::Deleted,
                properties: Vec::new(),
            });
            changes.push(PlannedChange::resource(
                address,
                ChangeKind::Forget,
                ChangeOrigin::ConfigAndDrift,
                Vec::new(),
                Vec::new(),
                CheckpointTarget::Absent,
            ));
        }
        (None, None) => {}
    }
}

fn plan_present_resource(
    address: ResourceAddress,
    desired: Option<&DesiredResource>,
    stored: Option<&StoredResource>,
    remote: &RemoteResource,
    changes: &mut Vec<PlannedChange>,
    drift: &mut Vec<DriftChange>,
    diagnostics: &mut Vec<PlanDiagnostic>,
) {
    let Some(stored) = stored else {
        if desired.is_some() {
            diagnostics.push(diagnostic(
                PlanDiagnosticCode::UnmanagedAddressCollision,
                Some(address),
            ));
        }
        return;
    };

    if stored.remote_id != *remote.remote_id() {
        diagnostics.push(diagnostic(
            PlanDiagnosticCode::RemoteIdentityMismatch,
            Some(address),
        ));
        return;
    }

    let Some(desired) = desired else {
        plan_present_delete(address, stored, remote, changes, drift, diagnostics);
        return;
    };

    if add_unsupported_resource_diagnostics(&address, desired, diagnostics) {
        return;
    }

    let mut property_diagnostics = required_property_diagnostics(&address, desired, remote);
    if !property_diagnostics.is_empty() {
        diagnostics.append(&mut property_diagnostics);
        return;
    }

    let property_plan = compare_properties(desired, stored, remote);
    if let Some(property) = invalid_ignored_checkpoint(desired, &property_plan) {
        let mut issue = diagnostic(PlanDiagnosticCode::InvalidIgnoredCheckpoint, Some(address));
        issue.property = Some(property);
        diagnostics.push(issue);
        return;
    }
    let (metadata, metadata_changed) = metadata_changes(desired, stored);
    if !property_plan.drifted.is_empty() {
        drift.push(DriftChange {
            address: address.clone(),
            kind: DriftKind::Updated,
            properties: property_plan.drifted.clone(),
        });
    }

    let config_changed = property_plan.config_changed || metadata_changed;
    let remote_changed = property_plan.remote_changed;
    if !config_changed && !remote_changed {
        return;
    }

    changes.push(
        PlannedChange::resource(
            address,
            if property_plan.convergence_required {
                ChangeKind::Update
            } else {
                ChangeKind::NoOp
            },
            change_origin(config_changed, remote_changed),
            property_plan.fields,
            metadata,
            CheckpointTarget::Present(resource_checkpoint_for_properties(
                desired,
                Some(stored),
                property_plan.checkpoint_properties,
            )),
        )
        .preserving(desired.ignore_changes.clone()),
    );
}

fn plan_present_delete(
    address: ResourceAddress,
    stored: &StoredResource,
    remote: &RemoteResource,
    changes: &mut Vec<PlannedChange>,
    drift: &mut Vec<DriftChange>,
    diagnostics: &mut Vec<PlanDiagnostic>,
) {
    let drifted = stored
        .properties
        .iter()
        .filter_map(|(key, stored_value)| {
            let remote_value = remote
                .property(key)
                .and_then(|observation| observed_value(key, observation))?;
            (remote_value.as_ref() != Some(stored_value)).then_some(key.clone())
        })
        .collect::<Vec<_>>();
    let remote_changed = !drifted.is_empty();
    if remote_changed {
        drift.push(DriftChange {
            address: address.clone(),
            kind: DriftKind::Updated,
            properties: drifted,
        });
    }

    if stored.protected {
        diagnostics.push(diagnostic(
            PlanDiagnosticCode::ProtectedDelete,
            Some(address),
        ));
        return;
    }

    changes.push(PlannedChange::resource(
        address,
        ChangeKind::Delete,
        if remote_changed {
            ChangeOrigin::ConfigAndDrift
        } else {
            ChangeOrigin::Config
        },
        Vec::new(),
        Vec::new(),
        CheckpointTarget::Absent,
    ));
}

fn add_unsupported_resource_diagnostics(
    address: &ResourceAddress,
    desired: &DesiredResource,
    diagnostics: &mut Vec<PlanDiagnostic>,
) -> bool {
    let before = diagnostics.len();
    if !desired.replace_on_changes.is_empty() {
        let mut issue = diagnostic(
            PlanDiagnosticCode::UnsupportedDirective,
            Some(address.clone()),
        );
        issue.property = desired.replace_on_changes.first().cloned();
        issue.unsupported = Some(UnsupportedDirectiveKind::Replacement);
        diagnostics.push(issue);
    }
    diagnostics.len() != before
}

fn required_property_diagnostics(
    address: &ResourceAddress,
    desired: &DesiredResource,
    remote: &RemoteResource,
) -> Vec<PlanDiagnostic> {
    desired
        .properties
        .keys()
        .filter(|key| !desired.ignore_changes.contains(key))
        .filter_map(|key| property_diagnostic(address, key.clone(), remote))
        .collect()
}

fn property_diagnostic(
    address: &ResourceAddress,
    key: PropertyPath,
    remote: &RemoteResource,
) -> Option<PlanDiagnostic> {
    match remote.property(&key) {
        None => {
            let mut issue = diagnostic(
                PlanDiagnosticCode::MissingPropertyObservation,
                Some(address.clone()),
            );
            issue.property = Some(key);
            Some(issue)
        }
        Some(PropertyObservation::Unknown(PropertyUnknownReason::Sensitive))
            if key.is_sensitive() =>
        {
            None
        }
        Some(PropertyObservation::Unknown(reason)) => {
            let mut issue = diagnostic(
                PlanDiagnosticCode::UnknownPropertyObservation,
                Some(address.clone()),
            );
            issue.property = Some(key);
            issue.property_unknown = Some(*reason);
            Some(issue)
        }
        Some(PropertyObservation::Known(_) | PropertyObservation::KnownAbsent) => None,
    }
}

struct PropertyPlan {
    fields: Vec<FieldChange>,
    drifted: Vec<PropertyPath>,
    config_changed: bool,
    remote_changed: bool,
    convergence_required: bool,
    checkpoint_properties: BTreeMap<PropertyPath, OwnedValue>,
}

fn compare_properties(
    desired: &DesiredResource,
    stored: &StoredResource,
    remote: &RemoteResource,
) -> PropertyPlan {
    let keys: BTreeSet<_> = desired
        .properties
        .keys()
        .chain(stored.properties.keys())
        .cloned()
        .collect();
    let mut fields = Vec::new();
    let mut drifted = Vec::new();
    let mut any_config = false;
    let mut any_remote = false;
    let mut convergence_required = false;
    let checkpoint_properties = effective_existing_properties(desired, stored);

    for key in keys {
        let desired_value = desired.properties.get(&key);
        let stored_value = stored.properties.get(&key);

        if desired.ignore_changes.contains(&key) {
            continue;
        }

        let Some(desired_value) = desired_value else {
            if let Some(stored_value) = stored_value {
                any_config = true;
                fields.push(FieldChange {
                    key: key.clone(),
                    stored: value_state(Some(stored_value)),
                    desired: ValueState::Unmanaged,
                    remote: observation_state(remote.property(&key)),
                    origin: ChangeOrigin::Config,
                });
            }
            continue;
        };

        let observation = remote
            .property(&key)
            .expect("desired property observations were validated");
        let config_changed = stored_value != Some(desired_value);
        let remote_value = observed_value(&key, observation);
        let (remote_changed, convergence) = match remote_value.as_ref() {
            Some(remote_value) => (
                stored_value.is_some() && remote_value.as_ref() != stored_value,
                remote_value.as_ref() != Some(desired_value),
            ),
            None => (false, config_changed),
        };

        if remote_changed {
            drifted.push(key.clone());
        }
        if config_changed || remote_changed {
            fields.push(FieldChange {
                key: key.clone(),
                stored: value_state(stored_value),
                desired: value_state(Some(desired_value)),
                remote: remote_value.as_ref().map_or_else(
                    || observation_state(Some(observation)),
                    |value| value_state(value.as_ref()),
                ),
                origin: change_origin(config_changed, remote_changed),
            });
        }
        any_config |= config_changed;
        any_remote |= remote_changed;
        convergence_required |= convergence;
    }

    PropertyPlan {
        fields,
        drifted,
        config_changed: any_config,
        remote_changed: any_remote,
        convergence_required,
        checkpoint_properties,
    }
}

fn invalid_ignored_checkpoint(
    desired: &DesiredResource,
    property_plan: &PropertyPlan,
) -> Option<PropertyPath> {
    invalid_ignored_properties(desired, &property_plan.checkpoint_properties)
}

fn invalid_ignored_properties(
    desired: &DesiredResource,
    properties: &BTreeMap<PropertyPath, OwnedValue>,
) -> Option<PropertyPath> {
    (!owned_source_shape_valid(properties)).then(|| {
        desired
            .ignore_changes
            .iter()
            .find(|path| path.is_source_child())
            .cloned()
            .unwrap_or(PropertyPath::SourceRepository)
    })
}

fn create_field_changes(desired: &DesiredResource) -> Vec<FieldChange> {
    desired
        .properties
        .iter()
        .map(|(key, value)| FieldChange {
            key: key.clone(),
            stored: ValueState::Unmanaged,
            desired: value_state(Some(value)),
            remote: ValueState::Null,
            origin: ChangeOrigin::Config,
        })
        .collect()
}

fn missing_resource_field_changes(
    desired: &DesiredResource,
    stored: &StoredResource,
) -> (Vec<FieldChange>, bool, BTreeMap<PropertyPath, OwnedValue>) {
    let checkpoint_properties = effective_existing_properties(desired, stored);
    let keys: BTreeSet<_> = checkpoint_properties
        .keys()
        .chain(stored.properties.keys())
        .cloned()
        .collect();
    let mut config_changed = false;
    let fields = keys
        .into_iter()
        .filter_map(|key| {
            let desired_value = checkpoint_properties.get(&key);
            let stored_value = stored.properties.get(&key);
            if desired_value == stored_value {
                return None;
            }
            config_changed = true;
            Some(FieldChange {
                key,
                stored: value_state(stored_value),
                desired: value_state(desired_value),
                remote: ValueState::Null,
                origin: ChangeOrigin::ConfigAndDrift,
            })
        })
        .collect();
    (fields, config_changed, checkpoint_properties)
}

fn effective_existing_properties(
    desired: &DesiredResource,
    stored: &StoredResource,
) -> BTreeMap<PropertyPath, OwnedValue> {
    let mut properties = desired.properties.clone();
    for path in &desired.ignore_changes {
        if path.is_lifecycle_only() {
            continue;
        }
        if let Some(value) = stored.properties.get(path) {
            properties.insert(path.clone(), value.clone());
        } else {
            properties.remove(path);
        }
    }
    properties
}

fn metadata_changes(
    desired: &DesiredResource,
    stored: &StoredResource,
) -> (Vec<MetadataChangeKind>, bool) {
    let mut metadata = Vec::new();
    if let ProtectionIntent::Set(protected) = desired.protection
        && protected != stored.protected
    {
        metadata.push(MetadataChangeKind::Protection);
    }
    if desired.containment != stored.containment {
        metadata.push(MetadataChangeKind::Containment);
    }
    if desired.dependencies != stored.dependencies {
        metadata.push(MetadataChangeKind::Dependencies);
    }
    let changed = !metadata.is_empty();
    (metadata, changed)
}

fn desired_metadata_for_create(desired: &DesiredResource) -> Vec<MetadataChangeKind> {
    let mut metadata = Vec::new();
    if matches!(desired.protection, ProtectionIntent::Set(_)) {
        metadata.push(MetadataChangeKind::Protection);
    }
    if desired.containment.is_some() {
        metadata.push(MetadataChangeKind::Containment);
    }
    if !desired.dependencies.is_empty() {
        metadata.push(MetadataChangeKind::Dependencies);
    }
    metadata
}

fn observed_value(
    path: &PropertyPath,
    observation: &PropertyObservation,
) -> Option<Option<OwnedValue>> {
    match observation {
        PropertyObservation::Known(value) => Some(Some(
            if path == &PropertyPath::Environment
                && value
                    .as_json()
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty)
            {
                OwnedValue::EmptyCollection
            } else {
                OwnedValue::Value(value.clone())
            },
        )),
        PropertyObservation::KnownAbsent => Some(Some(OwnedValue::Null)),
        PropertyObservation::Unknown(_) => None,
    }
}

fn observation_state(observation: Option<&PropertyObservation>) -> ValueState {
    match observation {
        Some(PropertyObservation::Known(_)) => ValueState::Present,
        Some(PropertyObservation::KnownAbsent) => ValueState::Null,
        Some(PropertyObservation::Unknown(_)) | None => ValueState::Unknown,
    }
}

fn value_state(value: Option<&OwnedValue>) -> ValueState {
    match value {
        None => ValueState::Unmanaged,
        Some(OwnedValue::Null) => ValueState::Null,
        Some(OwnedValue::EmptyCollection | OwnedValue::Value(_)) => ValueState::Present,
        Some(OwnedValue::Sensitive(_)) => ValueState::Sensitive,
    }
}

fn checkpoint_for_desired(
    desired: &DesiredResource,
    stored: Option<&StoredResource>,
) -> CheckpointTarget {
    CheckpointTarget::Present(resource_checkpoint_for_desired(desired, stored))
}

fn resource_checkpoint_for_desired(
    desired: &DesiredResource,
    stored: Option<&StoredResource>,
) -> ResourceCheckpoint {
    resource_checkpoint_for_properties(desired, stored, desired.properties.clone())
}

fn resource_checkpoint_for_properties(
    desired: &DesiredResource,
    stored: Option<&StoredResource>,
    properties: BTreeMap<PropertyPath, OwnedValue>,
) -> ResourceCheckpoint {
    let protected = match desired.protection {
        ProtectionIntent::Unmanaged => stored.is_some_and(|resource| resource.protected),
        ProtectionIntent::Set(value) => value,
    };
    ResourceCheckpoint {
        protected,
        containment: desired.containment.clone(),
        dependencies: desired.dependencies.clone(),
        properties,
    }
}

fn change_origin(config_changed: bool, remote_changed: bool) -> ChangeOrigin {
    match (config_changed, remote_changed) {
        (true, false) => ChangeOrigin::Config,
        (false, true) => ChangeOrigin::Drift,
        (true, true) => ChangeOrigin::ConfigAndDrift,
        (false, false) => unreachable!("unchanged resources do not produce plan entries"),
    }
}

fn diagnostic(code: PlanDiagnosticCode, address: Option<ResourceAddress>) -> PlanDiagnostic {
    PlanDiagnostic {
        code,
        address,
        related_address: None,
        property: None,
        remote_failure: None,
        property_unknown: None,
        unsupported: None,
    }
}

fn move_diagnostic(
    code: PlanDiagnosticCode,
    source: &ResourceAddress,
    target: &ResourceAddress,
) -> PlanDiagnostic {
    let mut issue = diagnostic(code, Some(source.clone()));
    issue.related_address = Some(target.clone());
    issue
}

fn finish_plan(
    desired: &DesiredState,
    stored: &StoredState,
    changes: Vec<PlannedChange>,
    mut drift: Vec<DriftChange>,
    mut diagnostics: Vec<PlanDiagnostic>,
) -> Plan {
    drift.sort_by(|left, right| left.address.cmp(&right.address));
    diagnostics.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then_with(|| left.address.cmp(&right.address))
            .then_with(|| left.related_address.cmp(&right.related_address))
            .then_with(|| left.property.cmp(&right.property))
            .then_with(|| left.unsupported.cmp(&right.unsupported))
    });
    let complete = !diagnostics.iter().any(|issue| {
        matches!(
            issue.code,
            PlanDiagnosticCode::InstanceMismatch
                | PlanDiagnosticCode::MissingObservation
                | PlanDiagnosticCode::RemoteUnavailable
                | PlanDiagnosticCode::MissingPropertyObservation
                | PlanDiagnosticCode::UnknownPropertyObservation
        )
    });

    Plan {
        format_version: PLAN_FORMAT_VERSION,
        lineage: stored.lineage,
        state_serial: stored.serial,
        config_digest: desired.digest.clone(),
        changes,
        drift,
        complete,
        applyable: diagnostics.is_empty(),
        diagnostics,
    }
}
