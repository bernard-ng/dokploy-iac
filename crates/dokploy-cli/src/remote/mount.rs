//! Authoritative bounded discovery for target-scoped Mounts.
//!
//! A Mount is contained by its environment but identified by its typed target
//! service and mount path. Every read is target scoped: the exact target's
//! complete Mount collection must agree with the direct `mounts.one` read, and
//! absence is proven only by an authoritative collection.

use dokploy_sdk::{
    ApplicationId, ComposeId, LibSqlId, MariaDbId, MongoId, MountDetails, MountId, MountType,
    MySqlId, PostgresId, RedisId, ServiceTarget,
};

use super::*;

type MountCollections =
    BTreeMap<(ResourceKind, String), Result<dokploy_sdk::MountCollection, SdkError>>;

/// Maps one logical target kind and trusted physical ID to the SDK's typed union.
pub(crate) fn mount_service_target(kind: ResourceKind, id: &str) -> Option<ServiceTarget> {
    Some(match kind {
        ResourceKind::Application => ServiceTarget::Application(ApplicationId::new(id)),
        ResourceKind::Compose => ServiceTarget::Compose(ComposeId::new(id)),
        ResourceKind::LibSql => ServiceTarget::LibSql(LibSqlId::new(id)),
        ResourceKind::MariaDb => ServiceTarget::MariaDb(MariaDbId::new(id)),
        ResourceKind::Mongo => ServiceTarget::Mongo(MongoId::new(id)),
        ResourceKind::MySql => ServiceTarget::MySql(MySqlId::new(id)),
        ResourceKind::Postgres => ServiceTarget::Postgres(PostgresId::new(id)),
        ResourceKind::Redis => ServiceTarget::Redis(RedisId::new(id)),
        ResourceKind::Project
        | ResourceKind::Environment
        | ResourceKind::Domain
        | ResourceKind::Port
        | ResourceKind::Redirect
        | ResourceKind::Security
        | ResourceKind::Tag
        | ResourceKind::Schedule
        | ResourceKind::Mount
        | ResourceKind::Backup => return None,
    })
}

pub(super) async fn discover_mount_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    authority: MountTopologyAuthority,
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
        .filter(|address| address.kind() == ResourceKind::Mount)
        .cloned()
        .collect();

    let mut collections = MountCollections::new();
    for address in &addresses {
        for target in mount_targets(address, compiled, state)? {
            let Some(id) = trusted_application_id(&target, compiled, state, topology) else {
                continue;
            };
            let key = (target.kind(), id.clone());
            if collections.contains_key(&key) {
                continue;
            }
            let service =
                mount_service_target(target.kind(), &id).ok_or(DiscoverRemoteError::MountTarget)?;
            collections.insert(key, client.mounts().by_target(service).await);
        }
    }
    validate_mount_collections(&collections)?;

    let mut seen_ids = BTreeSet::new();
    let mut observations = Vec::new();
    for address in addresses {
        let desired_binding = compiled.bindings().mount(&address);
        let observation = if let Some(stored) = state.resource(&address) {
            let stored_target = stored_mount_target(&address, state)?;
            let Some(stored_target_id) =
                trusted_application_id(&stored_target, compiled, state, topology)
            else {
                observations.push((
                    address,
                    inherited_target_observation(&stored_target, topology),
                ));
                continue;
            };
            let expected = mount_service_target(stored_target.kind(), &stored_target_id)
                .ok_or(DiscoverRemoteError::MountTarget)?;
            let key = (stored_target.kind(), stored_target_id.clone());
            match client
                .mounts()
                .get(MountId::new(stored.remote_id().as_str()))
                .await
            {
                Ok(mount) => {
                    let remote_id = RemoteId::new(mount.mount_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidMountId)?;
                    if remote_id != *stored.remote_id() || mount.target != expected {
                        return Err(DiscoverRemoteError::MountTopologyConflict);
                    }
                    match collections.get(&key) {
                        Some(Ok(collection)) => {
                            let matching = collection
                                .mounts()
                                .iter()
                                .filter(|candidate| candidate.mount_id == mount.mount_id)
                                .collect::<Vec<_>>();
                            if matching.as_slice() != [&mount] {
                                return Err(DiscoverRemoteError::MountTopologyConflict);
                            }
                        }
                        Some(Err(error)) => {
                            observations.push((
                                address,
                                RemoteObservation::Unavailable(classify_sdk_error(error)),
                            ));
                            continue;
                        }
                        None => return Err(DiscoverRemoteError::MountTarget),
                    }
                    if !seen_ids.insert(remote_id.clone()) {
                        return Err(DiscoverRemoteError::DuplicateMountId);
                    }

                    // Collision-key changes must not land on another Mount.
                    let mut blocked = None;
                    if let Some((desired_target, _, desired_path)) = desired_binding {
                        let desired_target_id =
                            trusted_application_id(desired_target, compiled, state, topology);
                        let same_physical_target =
                            desired_target_id.as_deref() == Some(stored_target_id.as_str());
                        if same_physical_target {
                            if desired_path != mount.mount_path
                                && let Some(Ok(collection)) = collections.get(&key)
                                && collection.mounts().iter().any(|other| {
                                    other.mount_id != mount.mount_id
                                        && other.mount_path == desired_path
                                })
                            {
                                return Err(DiscoverRemoteError::DuplicateMountCollision);
                            }
                        } else {
                            match observe_mount_under_target(
                                &address,
                                desired_target,
                                desired_path,
                                compiled,
                                state,
                                topology,
                                &collections,
                                authority,
                            )? {
                                RemoteObservation::Present(_) => {
                                    return Err(DiscoverRemoteError::DuplicateMountCollision);
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
                            desired_binding.map(|(target, _, _)| target),
                            &stored_target,
                            &stored_target_id,
                            compiled,
                            state,
                            topology,
                        );
                        RemoteObservation::Present(RemoteResource::new(
                            remote_id,
                            mount_properties(&address, compiled, &mount, &logical_target),
                        ))
                    }
                }
                Err(SdkError::Api(error)) if error.status() == 404 => match collections.get(&key) {
                    Some(Ok(collection))
                        if collection.mounts().iter().all(|mount| {
                            mount.mount_id.as_str() != stored.remote_id().as_str()
                        }) =>
                    {
                        if authority == MountTopologyAuthority::Authoritative {
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
            let Some((target, _, mount_path)) = desired_binding else {
                return Err(DiscoverRemoteError::MountTarget);
            };
            observe_mount_under_target(
                &address,
                target,
                mount_path,
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

/// Returns the desired and stored target addresses relevant to one Mount.
fn mount_targets(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<Vec<ResourceAddress>, DiscoverRemoteError> {
    let mut targets = Vec::new();
    if let Some((target, _, _)) = compiled.bindings().mount(address) {
        targets.push(target.clone());
    }
    if state.resource(address).is_some() {
        targets.push(stored_mount_target(address, state)?);
    }
    targets.sort();
    targets.dedup();

    Ok(targets)
}

fn stored_mount_target(
    address: &ResourceAddress,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    let target = state
        .resource(address)
        .and_then(|resource| resource.last_applied().as_json().get("target"))
        .and_then(serde_json::Value::as_str)
        .ok_or(DiscoverRemoteError::MountTarget)?;
    let target: ResourceAddress = target
        .parse()
        .map_err(|_| DiscoverRemoteError::MountTarget)?;
    if !target.kind().is_mount_target() {
        return Err(DiscoverRemoteError::MountTarget);
    }

    Ok(target)
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
/// When the desired target resolves to the same physical service as the stored
/// one (for example after a declarative target move), the remote already
/// satisfies the desired target and no replacement is required. Otherwise the
/// stored logical target is reported, so a changed target plans replacement.
fn logical_observed_target(
    desired_target: Option<&ResourceAddress>,
    stored_target: &ResourceAddress,
    stored_target_id: &str,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
) -> ResourceAddress {
    if let Some(desired_target) = desired_target
        && desired_target.kind() == stored_target.kind()
        && trusted_application_id(desired_target, compiled, state, topology).as_deref()
            == Some(stored_target_id)
    {
        return desired_target.clone();
    }

    stored_target.clone()
}

fn validate_mount_collections(collections: &MountCollections) -> Result<(), DiscoverRemoteError> {
    let mut global_ids = BTreeSet::new();
    for collection in collections
        .values()
        .filter_map(|result| result.as_ref().ok())
    {
        let mut paths = BTreeSet::new();
        for mount in collection.mounts() {
            let remote_id = RemoteId::new(mount.mount_id.as_str())
                .map_err(|_| DiscoverRemoteError::InvalidMountId)?;
            if !global_ids.insert(remote_id) {
                return Err(DiscoverRemoteError::DuplicateMountId);
            }
            if !paths.insert(mount.mount_path.as_str()) {
                return Err(DiscoverRemoteError::DuplicateMountCollision);
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_mount_under_target(
    address: &ResourceAddress,
    target: &ResourceAddress,
    mount_path: &str,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &MountCollections,
    authority: MountTopologyAuthority,
) -> Result<RemoteObservation, DiscoverRemoteError> {
    match observation(topology, target) {
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
    let Some(target_id) = trusted_application_id(target, compiled, state, topology) else {
        return Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        ));
    };
    match collections.get(&(target.kind(), target_id)) {
        Some(Ok(collection)) => {
            let matching = collection
                .mounts()
                .iter()
                .filter(|mount| mount.mount_path == mount_path)
                .collect::<Vec<_>>();
            match matching.as_slice() {
                [] if authority == MountTopologyAuthority::Authoritative => {
                    Ok(RemoteObservation::Missing)
                }
                [] => Ok(RemoteObservation::Unavailable(
                    RemoteFailureKind::InvalidResponse,
                )),
                [mount] => {
                    let remote_id = RemoteId::new(mount.mount_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidMountId)?;
                    Ok(RemoteObservation::Present(RemoteResource::new(
                        remote_id,
                        mount_properties(address, compiled, mount, target),
                    )))
                }
                _ => Err(DiscoverRemoteError::DuplicateMountCollision),
            }
        }
        Some(Err(error)) => Ok(RemoteObservation::Unavailable(classify_sdk_error(error))),
        None => Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        )),
    }
}

fn mount_properties(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    mount: &MountDetails,
    logical_target: &ResourceAddress,
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
            PropertyPath::Target => known(serde_json::json!(logical_target.to_string())),
            PropertyPath::MountType => known(serde_json::json!(mount_type_label(mount.mount_type))),
            PropertyPath::MountPath => known(serde_json::json!(mount.mount_path)),
            PropertyPath::HostPath => observe_string_field(&mount.host_path),
            PropertyPath::VolumeName => observe_string_field(&mount.volume_name),
            PropertyPath::FilePath => observe_string_field(&mount.file_path),
            PropertyPath::FileContent => {
                PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)
            }
            _ => continue,
        };
        properties.insert(path.clone(), observed);
    }

    properties
}

fn known(value: serde_json::Value) -> PropertyObservation {
    PropertyObservation::Known(
        ComparableValue::try_from_json(value).expect("Mount response values are non-null"),
    )
}

pub(crate) const fn mount_type_label(mount_type: MountType) -> &'static str {
    match mount_type {
        MountType::Bind => "bind",
        MountType::Volume => "volume",
        MountType::File => "file",
    }
}
