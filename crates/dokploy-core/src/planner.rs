use std::collections::BTreeSet;

use dokploy_state::ResourceAddress;

use crate::{
    ChangeKind, ChangeOrigin, CheckpointTarget, DesiredResource, DesiredState, DriftChange,
    DriftKind, FieldChange, MetadataChangeKind, OwnedValue, Plan, PlanDiagnostic,
    PlanDiagnosticCode, PlannedChange, PropertyObservation, PropertyPath, ProtectionIntent,
    RemoteObservation, RemoteResource, RemoteState, ResourceCheckpoint, StoredState,
    UnsupportedDirectiveKind, ValueState, plan::PLAN_FORMAT_VERSION, snapshot::StoredResource,
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

    let directive_diagnostics = unsupported_directive_diagnostics(desired);
    if !directive_diagnostics.is_empty() {
        return finish_plan(
            desired,
            stored,
            Vec::new(),
            Vec::new(),
            directive_diagnostics,
        );
    }

    let mut changes = Vec::new();
    let mut drift = Vec::new();
    let mut diagnostics = Vec::new();
    let relevant_addresses: BTreeSet<_> = desired
        .resources
        .keys()
        .chain(stored.resources.keys())
        .cloned()
        .collect();

    for address in relevant_addresses {
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

    finish_plan(desired, stored, changes, drift, diagnostics)
}

fn unsupported_directive_diagnostics(desired: &DesiredState) -> Vec<PlanDiagnostic> {
    let mut diagnostics = Vec::new();
    for directive in &desired.moves {
        let mut issue = diagnostic(
            PlanDiagnosticCode::UnsupportedDirective,
            Some(directive.from().clone()),
        );
        issue.unsupported = Some(UnsupportedDirectiveKind::Move);
        diagnostics.push(issue);
    }
    for directive in &desired.removals {
        let mut issue = diagnostic(
            PlanDiagnosticCode::UnsupportedDirective,
            Some(directive.address().clone()),
        );
        issue.unsupported = Some(UnsupportedDirectiveKind::Removal);
        diagnostics.push(issue);
    }
    diagnostics
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

            changes.push(PlannedChange {
                address,
                kind: ChangeKind::Create,
                origin: ChangeOrigin::Config,
                fields: create_field_changes(desired),
                metadata: desired_metadata_for_create(desired),
                checkpoint: checkpoint_for_desired(desired, None),
            });
        }
        (Some(desired), Some(stored)) => {
            if add_unsupported_resource_diagnostics(&address, desired, diagnostics) {
                return;
            }

            let (fields, config_changed) = missing_resource_field_changes(desired, stored);
            let (metadata, metadata_changed) = metadata_changes(desired, stored);
            drift.push(DriftChange {
                address: address.clone(),
                kind: DriftKind::Deleted,
                properties: Vec::new(),
            });
            changes.push(PlannedChange {
                address,
                kind: ChangeKind::Create,
                origin: if config_changed || metadata_changed {
                    ChangeOrigin::ConfigAndDrift
                } else {
                    ChangeOrigin::Drift
                },
                fields,
                metadata,
                checkpoint: checkpoint_for_desired(desired, Some(stored)),
            });
        }
        (None, Some(_)) => {
            drift.push(DriftChange {
                address: address.clone(),
                kind: DriftKind::Deleted,
                properties: Vec::new(),
            });
            changes.push(PlannedChange {
                address,
                kind: ChangeKind::Forget,
                origin: ChangeOrigin::ConfigAndDrift,
                fields: Vec::new(),
                metadata: Vec::new(),
                checkpoint: CheckpointTarget::Absent,
            });
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

    changes.push(PlannedChange {
        address,
        kind: if property_plan.convergence_required {
            ChangeKind::Update
        } else {
            ChangeKind::NoOp
        },
        origin: change_origin(config_changed, remote_changed),
        fields: property_plan.fields,
        metadata,
        checkpoint: checkpoint_for_desired(desired, Some(stored)),
    });
}

fn plan_present_delete(
    address: ResourceAddress,
    stored: &StoredResource,
    remote: &RemoteResource,
    changes: &mut Vec<PlannedChange>,
    drift: &mut Vec<DriftChange>,
    diagnostics: &mut Vec<PlanDiagnostic>,
) {
    let mut property_diagnostics = stored_property_diagnostics(&address, stored, remote);
    if !property_diagnostics.is_empty() {
        diagnostics.append(&mut property_diagnostics);
        return;
    }

    let drifted = stored
        .properties
        .iter()
        .filter_map(|(key, stored_value)| {
            let remote_value = observed_value(
                key,
                remote
                    .property(key)
                    .expect("stored property observations were validated"),
            )
            .expect("stored property observations were validated as known");
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

    changes.push(PlannedChange {
        address,
        kind: ChangeKind::Delete,
        origin: if remote_changed {
            ChangeOrigin::ConfigAndDrift
        } else {
            ChangeOrigin::Config
        },
        fields: Vec::new(),
        metadata: Vec::new(),
        checkpoint: CheckpointTarget::Absent,
    });
}

fn add_unsupported_resource_diagnostics(
    address: &ResourceAddress,
    desired: &DesiredResource,
    diagnostics: &mut Vec<PlanDiagnostic>,
) -> bool {
    let before = diagnostics.len();
    if !desired.ignore_changes.is_empty() {
        let mut issue = diagnostic(
            PlanDiagnosticCode::UnsupportedDirective,
            Some(address.clone()),
        );
        issue.property = desired.ignore_changes.first().cloned();
        issue.unsupported = Some(UnsupportedDirectiveKind::IgnoreChanges);
        diagnostics.push(issue);
    }
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
        .filter_map(|key| property_diagnostic(address, key.clone(), remote))
        .collect()
}

fn stored_property_diagnostics(
    address: &ResourceAddress,
    stored: &StoredResource,
    remote: &RemoteResource,
) -> Vec<PlanDiagnostic> {
    stored
        .properties
        .keys()
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

    for key in keys {
        let desired_value = desired.properties.get(&key);
        let stored_value = stored.properties.get(&key);

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

        let remote_value = observed_value(
            &key,
            remote
                .property(&key)
                .expect("desired property observations were validated"),
        )
        .expect("desired property observations were validated as known");
        let config_changed = stored_value != Some(desired_value);
        let remote_changed = stored_value.is_some() && remote_value.as_ref() != stored_value;
        let convergence = remote_value.as_ref() != Some(desired_value);

        if remote_changed {
            drifted.push(key.clone());
        }
        if config_changed || remote_changed {
            fields.push(FieldChange {
                key: key.clone(),
                stored: value_state(stored_value),
                desired: value_state(Some(desired_value)),
                remote: value_state(remote_value.as_ref()),
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
    }
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
) -> (Vec<FieldChange>, bool) {
    let keys: BTreeSet<_> = desired
        .properties
        .keys()
        .chain(stored.properties.keys())
        .cloned()
        .collect();
    let mut config_changed = false;
    let fields = keys
        .into_iter()
        .filter_map(|key| {
            let desired_value = desired.properties.get(&key);
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
    (fields, config_changed)
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
        Some(OwnedValue::Sensitive) => ValueState::Sensitive,
    }
}

fn checkpoint_for_desired(
    desired: &DesiredResource,
    stored: Option<&StoredResource>,
) -> CheckpointTarget {
    let protected = match desired.protection {
        ProtectionIntent::Unmanaged => stored.is_some_and(|resource| resource.protected),
        ProtectionIntent::Set(value) => value,
    };
    CheckpointTarget::Present(ResourceCheckpoint {
        protected,
        dependencies: desired.dependencies.clone(),
        properties: desired.properties.clone(),
    })
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
        property: None,
        remote_failure: None,
        property_unknown: None,
        unsupported: None,
    }
}

fn finish_plan(
    desired: &DesiredState,
    stored: &StoredState,
    mut changes: Vec<PlannedChange>,
    mut drift: Vec<DriftChange>,
    mut diagnostics: Vec<PlanDiagnostic>,
) -> Plan {
    changes.sort_by(|left, right| left.address.cmp(&right.address));
    drift.sort_by(|left, right| left.address.cmp(&right.address));
    diagnostics.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then_with(|| left.address.cmp(&right.address))
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
