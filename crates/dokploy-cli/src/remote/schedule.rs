//! Authoritative bounded discovery for target-scoped Schedules.
//!
//! A Schedule is contained by its environment but identified by its typed
//! target (an application, or one Compose service) and its name. Every read is
//! target scoped: the exact target's complete `schedule.list` collection must
//! agree with the direct `schedule.one` read, and absence is proven only by an
//! authoritative collection. Command and script text never reaches this module:
//! the SDK reduces both to presence flags.

use dokploy_sdk::{
    ApplicationId, ComposeId, ScheduleCollection, ScheduleDetails, ScheduleId, ScheduleTarget,
    ShellType,
};

use super::*;

type ScheduleCollections =
    BTreeMap<(ResourceKind, String, Option<String>), Result<ScheduleCollection, SdkError>>;

/// A logical Schedule target: the target address and its optional Compose service.
type LogicalTarget = (ResourceAddress, Option<String>);

/// Maps one logical target kind, trusted physical ID, and service name to the SDK's closed union.
pub(crate) fn schedule_sdk_target(
    kind: ResourceKind,
    id: &str,
    service_name: Option<&str>,
) -> Option<ScheduleTarget> {
    match (kind, service_name) {
        (ResourceKind::Application, None) => {
            Some(ScheduleTarget::Application(ApplicationId::new(id)))
        }
        (ResourceKind::Compose, Some(service_name)) if !service_name.is_empty() => {
            Some(ScheduleTarget::Compose {
                compose_id: ComposeId::new(id),
                service_name: service_name.to_owned(),
            })
        }
        _ => None,
    }
}

/// Returns the stable shell label used in properties.
pub(crate) const fn shell_label(shell: ShellType) -> &'static str {
    match shell {
        ShellType::Bash => "bash",
        ShellType::Sh => "sh",
    }
}

pub(super) async fn discover_schedule_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    authority: ScheduleTopologyAuthority,
) -> Result<Vec<(ResourceAddress, RemoteObservation)>, DiscoverRemoteError> {
    let desired = compiled.desired_state();
    let addresses: BTreeSet<_> = desired
        .resources()
        .keys()
        .chain(state.resources().keys())
        .chain(
            desired
                .removals()
                .iter()
                .filter(|directive| state.resource(directive.address()).is_some())
                .map(|directive| directive.address()),
        )
        .filter(|address| address.kind() == ResourceKind::Schedule)
        .cloned()
        .collect();

    let mut collections = ScheduleCollections::new();
    for address in &addresses {
        for (target, service) in schedule_targets(address, compiled, state)? {
            let Some(id) = trusted_application_id(&target, compiled, state, topology) else {
                continue;
            };
            let key = (target.kind(), id.clone(), service.clone());
            if collections.contains_key(&key) {
                continue;
            }
            let sdk_target = schedule_sdk_target(target.kind(), &id, service.as_deref())
                .ok_or(DiscoverRemoteError::ScheduleTarget)?;
            collections.insert(key, client.schedules().by_target(sdk_target).await);
        }
    }
    validate_schedule_collections(&collections)?;

    let mut seen_ids = BTreeSet::new();
    let mut observations = Vec::new();
    for address in addresses {
        let desired_binding = compiled.bindings().schedule(&address);
        let observation = if let Some(stored) = state.resource(&address) {
            let (stored_target, stored_service) = stored_schedule_target(&address, state)?;
            let Some(stored_target_id) =
                trusted_application_id(&stored_target, compiled, state, topology)
            else {
                observations.push((
                    address,
                    inherited_target_observation(&stored_target, topology),
                ));
                continue;
            };
            let expected = schedule_sdk_target(
                stored_target.kind(),
                &stored_target_id,
                stored_service.as_deref(),
            )
            .ok_or(DiscoverRemoteError::ScheduleTarget)?;
            let key = (
                stored_target.kind(),
                stored_target_id.clone(),
                stored_service.clone(),
            );
            match client
                .schedules()
                .get(ScheduleId::new(stored.remote_id().as_str()))
                .await
            {
                Ok(schedule) => {
                    let remote_id = RemoteId::new(schedule.schedule_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidScheduleId)?;
                    if remote_id != *stored.remote_id() || schedule.target != expected {
                        return Err(DiscoverRemoteError::ScheduleTopologyConflict);
                    }
                    match collections.get(&key) {
                        Some(Ok(collection)) => {
                            let matching = collection
                                .schedules()
                                .iter()
                                .filter(|candidate| candidate.schedule_id == schedule.schedule_id)
                                .collect::<Vec<_>>();
                            if matching.as_slice() != [&schedule] {
                                return Err(DiscoverRemoteError::ScheduleTopologyConflict);
                            }
                        }
                        Some(Err(error)) => {
                            observations.push((
                                address,
                                RemoteObservation::Unavailable(classify_sdk_error(error)),
                            ));
                            continue;
                        }
                        None => return Err(DiscoverRemoteError::ScheduleTarget),
                    }
                    if !seen_ids.insert(remote_id.clone()) {
                        return Err(DiscoverRemoteError::DuplicateScheduleId);
                    }

                    // A name or target change must not land on another Schedule.
                    let mut blocked = None;
                    if let Some((desired_target, desired_service, desired_name)) = desired_binding {
                        let desired_target_id =
                            trusted_application_id(desired_target, compiled, state, topology);
                        let same_physical_target = desired_target_id.as_deref()
                            == Some(stored_target_id.as_str())
                            && desired_service == stored_service.as_deref();
                        if same_physical_target {
                            if desired_name != schedule.name
                                && let Some(Ok(collection)) = collections.get(&key)
                                && collection.schedules().iter().any(|other| {
                                    other.schedule_id != schedule.schedule_id
                                        && other.name == desired_name
                                })
                            {
                                return Err(DiscoverRemoteError::DuplicateScheduleCollision);
                            }
                        } else {
                            match observe_schedule_under_target(
                                &address,
                                (desired_target, desired_service),
                                desired_name,
                                compiled,
                                state,
                                topology,
                                &collections,
                                authority,
                            )? {
                                RemoteObservation::Present(_) => {
                                    return Err(DiscoverRemoteError::DuplicateScheduleCollision);
                                }
                                RemoteObservation::Unavailable(failure) => {
                                    blocked = Some(RemoteObservation::Unavailable(failure));
                                }
                                RemoteObservation::Missing => {}
                            }
                        }
                    }
                    if let Some(blocked) = blocked {
                        blocked
                    } else {
                        let logical_target = logical_observed_target(
                            desired_binding.map(|(target, service, _)| (target, service)),
                            (&stored_target, stored_service.as_deref()),
                            &stored_target_id,
                            compiled,
                            state,
                            topology,
                        );
                        RemoteObservation::Present(RemoteResource::new(
                            remote_id,
                            schedule_properties(&address, compiled, &schedule, &logical_target),
                        ))
                    }
                }
                Err(SdkError::Api(error)) if error.status() == 404 => match collections.get(&key) {
                    Some(Ok(collection))
                        if collection.schedules().iter().all(|schedule| {
                            schedule.schedule_id.as_str() != stored.remote_id().as_str()
                        }) =>
                    {
                        if authority == ScheduleTopologyAuthority::Authoritative {
                            RemoteObservation::Missing
                        } else {
                            RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
                        }
                    }
                    Some(Ok(_)) => {
                        RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
                    }
                    Some(Err(error)) => RemoteObservation::Unavailable(classify_sdk_error(error)),
                    None => RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse),
                },
                Err(error) => RemoteObservation::Unavailable(classify_sdk_error(&error)),
            }
        } else {
            let Some((target, service, name)) = desired_binding else {
                return Err(DiscoverRemoteError::ScheduleTarget);
            };
            observe_schedule_under_target(
                &address,
                (target, service),
                name,
                compiled,
                state,
                topology,
                &collections,
                authority,
            )?
        };
        observations.push((address, observation));
    }

    Ok(observations)
}

/// Returns the desired and stored typed targets relevant to one Schedule.
fn schedule_targets(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<Vec<LogicalTarget>, DiscoverRemoteError> {
    let mut targets = Vec::new();
    if let Some((target, service, _)) = compiled.bindings().schedule(address) {
        targets.push((target.clone(), service.map(str::to_owned)));
    }
    if state.resource(address).is_some() {
        targets.push(stored_schedule_target(address, state)?);
    }
    targets.sort();
    targets.dedup();

    Ok(targets)
}

pub(crate) fn stored_schedule_target(
    address: &ResourceAddress,
    state: &StateFile,
) -> Result<LogicalTarget, DiscoverRemoteError> {
    let managed = state
        .resource(address)
        .map(|resource| resource.last_applied().as_json())
        .ok_or(DiscoverRemoteError::ScheduleTarget)?;
    let target: ResourceAddress = managed
        .get("target")
        .and_then(serde_json::Value::as_str)
        .ok_or(DiscoverRemoteError::ScheduleTarget)?
        .parse()
        .map_err(|_| DiscoverRemoteError::ScheduleTarget)?;
    let service = match managed.get("service_name") {
        None => None,
        Some(serde_json::Value::String(service)) if !service.is_empty() => Some(service.clone()),
        Some(_) => return Err(DiscoverRemoteError::ScheduleTarget),
    };
    let shape_valid = match target.kind() {
        ResourceKind::Application => service.is_none(),
        ResourceKind::Compose => service.is_some(),
        _ => false,
    };
    if !shape_valid {
        return Err(DiscoverRemoteError::ScheduleTarget);
    }

    Ok((target, service))
}

fn inherited_target_observation(
    target: &ResourceAddress,
    topology: &[(ResourceAddress, RemoteObservation)],
) -> RemoteObservation {
    match observation(topology, target) {
        Some(RemoteObservation::Missing) => RemoteObservation::Missing,
        Some(RemoteObservation::Unavailable(failure)) => RemoteObservation::Unavailable(*failure),
        Some(RemoteObservation::Present(_)) | None => {
            RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
        }
    }
}

/// Resolves the physical target back to the logical address the plan compares.
///
/// When the desired target resolves to the same physical service (for example
/// after a declarative target move), the remote already satisfies the desired
/// target and no replacement is required. Otherwise the stored logical target
/// is reported, so a changed target plans replacement.
fn logical_observed_target(
    desired_target: Option<(&ResourceAddress, Option<&str>)>,
    stored_target: (&ResourceAddress, Option<&str>),
    stored_target_id: &str,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
) -> LogicalTarget {
    if let Some((desired_target, desired_service)) = desired_target
        && desired_target.kind() == stored_target.0.kind()
        && desired_service == stored_target.1
        && trusted_application_id(desired_target, compiled, state, topology).as_deref()
            == Some(stored_target_id)
    {
        return (desired_target.clone(), desired_service.map(str::to_owned));
    }

    (stored_target.0.clone(), stored_target.1.map(str::to_owned))
}

fn validate_schedule_collections(
    collections: &ScheduleCollections,
) -> Result<(), DiscoverRemoteError> {
    let mut global_ids = BTreeSet::new();
    for collection in collections
        .values()
        .filter_map(|result| result.as_ref().ok())
    {
        let mut names = BTreeSet::new();
        for schedule in collection.schedules() {
            let remote_id = RemoteId::new(schedule.schedule_id.as_str())
                .map_err(|_| DiscoverRemoteError::InvalidScheduleId)?;
            if !global_ids.insert(remote_id) {
                return Err(DiscoverRemoteError::DuplicateScheduleId);
            }
            if !names.insert(schedule.name.as_str()) {
                return Err(DiscoverRemoteError::DuplicateScheduleCollision);
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_schedule_under_target(
    address: &ResourceAddress,
    target: (&ResourceAddress, Option<&str>),
    name: &str,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &ScheduleCollections,
    authority: ScheduleTopologyAuthority,
) -> Result<RemoteObservation, DiscoverRemoteError> {
    let (target_address, service) = target;
    match observation(topology, target_address) {
        Some(RemoteObservation::Missing) => return Ok(RemoteObservation::Missing),
        Some(RemoteObservation::Unavailable(failure)) => {
            return Ok(RemoteObservation::Unavailable(*failure));
        }
        Some(RemoteObservation::Present(_)) => {}
        None => {
            return Ok(RemoteObservation::Unavailable(
                RemoteFailureKind::InvalidResponse,
            ));
        }
    }
    let Some(target_id) = trusted_application_id(target_address, compiled, state, topology) else {
        return Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        ));
    };
    match collections.get(&(target_address.kind(), target_id, service.map(str::to_owned))) {
        Some(Ok(collection)) => {
            let matching = collection
                .schedules()
                .iter()
                .filter(|schedule| schedule.name == name)
                .collect::<Vec<_>>();
            match matching.as_slice() {
                [] if authority == ScheduleTopologyAuthority::Authoritative => {
                    Ok(RemoteObservation::Missing)
                }
                [] => Ok(RemoteObservation::Unavailable(
                    RemoteFailureKind::InvalidResponse,
                )),
                [schedule] => {
                    let remote_id = RemoteId::new(schedule.schedule_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidScheduleId)?;
                    Ok(RemoteObservation::Present(RemoteResource::new(
                        remote_id,
                        schedule_properties(
                            address,
                            compiled,
                            schedule,
                            &(target_address.clone(), service.map(str::to_owned)),
                        ),
                    )))
                }
                _ => Err(DiscoverRemoteError::DuplicateScheduleCollision),
            }
        }
        Some(Err(error)) => Ok(RemoteObservation::Unavailable(classify_sdk_error(error))),
        None => Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        )),
    }
}

fn schedule_properties(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    schedule: &ScheduleDetails,
    logical_target: &LogicalTarget,
) -> BTreeMap<PropertyPath, PropertyObservation> {
    let mut properties = BTreeMap::new();
    let Some(desired) = desired_resource_for_observation(address, compiled) else {
        return properties;
    };

    for path in desired.properties().keys() {
        if desired.ignored_changes().contains(path) {
            continue;
        }
        let observed = match path {
            PropertyPath::Target => known(serde_json::json!(logical_target.0.to_string())),
            PropertyPath::ServiceName => match &logical_target.1 {
                Some(service) => known(serde_json::json!(service)),
                None => PropertyObservation::KnownAbsent,
            },
            PropertyPath::ScheduleName => known(serde_json::json!(schedule.name)),
            PropertyPath::CronExpression => known(serde_json::json!(schedule.cron_expression)),
            PropertyPath::ShellType => known(serde_json::json!(shell_label(schedule.shell_type))),
            PropertyPath::Enabled => known(serde_json::json!(schedule.enabled)),
            PropertyPath::Description => optional_text(schedule.description.as_deref()),
            PropertyPath::Timezone => optional_text(schedule.timezone.as_deref()),
            // The command is always present in a valid record; the script only sometimes.
            PropertyPath::Command => PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
            PropertyPath::Script if schedule.script_present => {
                PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)
            }
            PropertyPath::Script => PropertyObservation::KnownAbsent,
            _ => continue,
        };
        properties.insert(path.clone(), observed);
    }

    properties
}

fn optional_text(value: Option<&str>) -> PropertyObservation {
    match value {
        Some(value) => known(serde_json::json!(value)),
        None => PropertyObservation::KnownAbsent,
    }
}

fn known(value: serde_json::Value) -> PropertyObservation {
    PropertyObservation::Known(
        ComparableValue::try_from_json(value).expect("Schedule response values are non-null"),
    )
}
