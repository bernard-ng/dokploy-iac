//! Authoritative bounded discovery for target-scoped database Backups.
//!
//! A Backup is contained by its environment but identified by its typed
//! database target and, for adoption, the SDK collision tuple of destination,
//! runtime-normalized prefix, and database. Every read is target scoped: the
//! exact target's complete `backups` relation must agree with the direct
//! `backup.one` read, and absence is proven only by an authoritative
//! collection. The attached destination is observed through the external
//! selector vocabulary and never as a physical identity.

use dokploy_sdk::{
    BackupCollection, BackupDetails, BackupId, BackupTarget, LibSqlId, MariaDbId, MongoId, MySqlId,
    PostgresId,
};

use super::*;

type BackupCollections = BTreeMap<(ResourceKind, String), Result<BackupCollection, SdkError>>;

/// Maps one logical target kind and trusted physical ID to the SDK's typed union.
///
/// Compose, web-server, Redis, and application targets are not Backup targets.
pub(crate) fn backup_target(kind: ResourceKind, id: &str) -> Option<BackupTarget> {
    Some(match kind {
        ResourceKind::Postgres => BackupTarget::Postgres(PostgresId::new(id)),
        ResourceKind::MySql => BackupTarget::MySql(MySqlId::new(id)),
        ResourceKind::MariaDb => BackupTarget::MariaDb(MariaDbId::new(id)),
        ResourceKind::Mongo => BackupTarget::Mongo(MongoId::new(id)),
        ResourceKind::LibSql => BackupTarget::LibSql(LibSqlId::new(id)),
        ResourceKind::Project
        | ResourceKind::Environment
        | ResourceKind::Application
        | ResourceKind::Compose
        | ResourceKind::Redis
        | ResourceKind::Domain
        | ResourceKind::Port
        | ResourceKind::Redirect
        | ResourceKind::Security
        | ResourceKind::Mount
        | ResourceKind::Tag
        | ResourceKind::Schedule
        | ResourceKind::Backup => return None,
        _ => return None,
    })
}

/// Mirrors Dokploy's collision normalization used by the SDK collision tuple.
fn normalize_backup_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

/// Returns whether a remote Backup occupies the desired collision key.
fn occupies_key(
    backup: &BackupDetails,
    destination_id: &str,
    prefix: &str,
    database: &str,
) -> bool {
    backup.destination_id.as_str() == destination_id
        && normalize_backup_prefix(&backup.prefix) == normalize_backup_prefix(prefix)
        && backup.database == database
}

pub(super) async fn discover_backup_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    authority: BackupTopologyAuthority,
    externals: &ExternalDirectory,
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
        .filter(|address| address.kind() == ResourceKind::Backup)
        .cloned()
        .collect();

    let mut collections = BackupCollections::new();
    for address in &addresses {
        for target in backup_targets(address, compiled, state)? {
            let Some(id) = trusted_application_id(&target, compiled, state, topology) else {
                continue;
            };
            let key = (target.kind(), id.clone());
            if collections.contains_key(&key) {
                continue;
            }
            let service =
                backup_target(target.kind(), &id).ok_or(DiscoverRemoteError::BackupTarget)?;
            collections.insert(key, client.backups().by_target(service).await);
        }
    }
    validate_backup_collections(&collections)?;

    let mut seen_ids = BTreeSet::new();
    let mut observations = Vec::new();
    for address in addresses {
        let desired_binding = compiled.bindings().backup(&address);
        let observation = if let Some(stored) = state.resource(&address) {
            let stored_target = stored_backup_target(&address, state)?;
            let Some(stored_target_id) =
                trusted_application_id(&stored_target, compiled, state, topology)
            else {
                observations.push((
                    address,
                    inherited_target_observation(&stored_target, topology),
                ));
                continue;
            };
            let expected = backup_target(stored_target.kind(), &stored_target_id)
                .ok_or(DiscoverRemoteError::BackupTarget)?;
            let key = (stored_target.kind(), stored_target_id.clone());
            match client
                .backups()
                .get(BackupId::new(stored.remote_id().as_str()))
                .await
            {
                Ok(backup) => {
                    let remote_id = RemoteId::new(backup.backup_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidBackupId)?;
                    if remote_id != *stored.remote_id() || backup.target != expected {
                        return Err(DiscoverRemoteError::BackupTopologyConflict);
                    }
                    match collections.get(&key) {
                        Some(Ok(collection)) => {
                            let matching = collection
                                .backups()
                                .iter()
                                .filter(|candidate| candidate.backup_id == backup.backup_id)
                                .collect::<Vec<_>>();
                            if matching.as_slice() != [&backup] {
                                return Err(DiscoverRemoteError::BackupTopologyConflict);
                            }
                        }
                        Some(Err(error)) => {
                            observations.push((
                                address,
                                RemoteObservation::Unavailable(classify_sdk_error(error)),
                            ));
                            continue;
                        }
                        None => return Err(DiscoverRemoteError::BackupTarget),
                    }
                    if !seen_ids.insert(remote_id.clone()) {
                        return Err(DiscoverRemoteError::DuplicateBackupId);
                    }

                    // A key change must not land on another Backup. An unresolved
                    // destination blocks planning through its own diagnostic.
                    let mut blocked = None;
                    if let Some((desired_target, selector, prefix, database)) = desired_binding
                        && let Some(destination_id) = resolved_destination(externals, selector)
                    {
                        let desired_target_id =
                            trusted_application_id(desired_target, compiled, state, topology);
                        if desired_target_id.as_deref() == Some(stored_target_id.as_str()) {
                            if let Some(Ok(collection)) = collections.get(&key)
                                && collection.backups().iter().any(|other| {
                                    other.backup_id != backup.backup_id
                                        && occupies_key(other, &destination_id, prefix, database)
                                })
                            {
                                return Err(DiscoverRemoteError::DuplicateBackupCollision);
                            }
                        } else {
                            match observe_backup_under_target(
                                &address,
                                desired_target,
                                &destination_id,
                                prefix,
                                database,
                                compiled,
                                state,
                                topology,
                                &collections,
                                authority,
                                externals,
                            )? {
                                RemoteObservation::Present(_) => {
                                    return Err(DiscoverRemoteError::DuplicateBackupCollision);
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
                            desired_binding.map(|(target, ..)| target),
                            &stored_target,
                            &stored_target_id,
                            compiled,
                            state,
                            topology,
                        );
                        RemoteObservation::Present(RemoteResource::new(
                            remote_id,
                            backup_properties(
                                &address,
                                compiled,
                                &backup,
                                &logical_target,
                                externals,
                            ),
                        ))
                    }
                }
                Err(SdkError::Api(error)) if error.status() == 404 => match collections.get(&key) {
                    Some(Ok(collection))
                        if collection.backups().iter().all(|backup| {
                            backup.backup_id.as_str() != stored.remote_id().as_str()
                        }) =>
                    {
                        if authority == BackupTopologyAuthority::Authoritative {
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
            let Some((target, selector, prefix, database)) = desired_binding else {
                return Err(DiscoverRemoteError::BackupTarget);
            };
            match observation(topology, target) {
                Some(RemoteObservation::Missing) => RemoteObservation::Missing,
                _ => match resolved_destination(externals, selector) {
                    Some(destination_id) => observe_backup_under_target(
                        &address,
                        target,
                        &destination_id,
                        prefix,
                        database,
                        compiled,
                        state,
                        topology,
                        &collections,
                        authority,
                        externals,
                    )?,
                    // The destination selector blocks planning; the identity stays unknown.
                    None => RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse),
                },
            }
        };
        observations.push((address, observation));
    }

    Ok(observations)
}

/// Returns the physical identity only for a destination that resolved uniquely.
fn resolved_destination(
    externals: &ExternalDirectory,
    selector: &dokploy_config::ExternalSelector,
) -> Option<String> {
    match externals.resolve(SelectorKind::Destination, selector) {
        ExternalResolution::Resolved(remote_id) => Some(remote_id.as_str().to_owned()),
        ExternalResolution::Local
        | ExternalResolution::Unmatched
        | ExternalResolution::Ambiguous
        | ExternalResolution::Unavailable(_) => None,
    }
}

/// Returns the desired and stored target addresses relevant to one Backup.
fn backup_targets(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<Vec<ResourceAddress>, DiscoverRemoteError> {
    let mut targets = Vec::new();
    if let Some((target, ..)) = compiled.bindings().backup(address) {
        targets.push(target.clone());
    }
    if state.resource(address).is_some() {
        targets.push(stored_backup_target(address, state)?);
    }
    targets.sort();
    targets.dedup();

    Ok(targets)
}

fn stored_backup_target(
    address: &ResourceAddress,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    let target = state
        .resource(address)
        .and_then(|resource| resource.last_applied().as_json().get("target"))
        .and_then(serde_json::Value::as_str)
        .ok_or(DiscoverRemoteError::BackupTarget)?;
    let target: ResourceAddress = target
        .parse()
        .map_err(|_| DiscoverRemoteError::BackupTarget)?;
    if !target.kind().is_backup_target() {
        return Err(DiscoverRemoteError::BackupTarget);
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
/// When the desired target resolves to the same physical database as the stored
/// one (for example after a declarative target move), the remote already
/// satisfies the desired target. Otherwise the stored logical target is
/// reported, so a changed target plans replacement.
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

fn validate_backup_collections(collections: &BackupCollections) -> Result<(), DiscoverRemoteError> {
    let mut global_ids = BTreeSet::new();
    for collection in collections
        .values()
        .filter_map(|result| result.as_ref().ok())
    {
        for backup in collection.backups() {
            let remote_id = RemoteId::new(backup.backup_id.as_str())
                .map_err(|_| DiscoverRemoteError::InvalidBackupId)?;
            if !global_ids.insert(remote_id) {
                return Err(DiscoverRemoteError::DuplicateBackupId);
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_backup_under_target(
    address: &ResourceAddress,
    target: &ResourceAddress,
    destination_id: &str,
    prefix: &str,
    database: &str,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &BackupCollections,
    authority: BackupTopologyAuthority,
    externals: &ExternalDirectory,
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
                .backups()
                .iter()
                .filter(|backup| occupies_key(backup, destination_id, prefix, database))
                .collect::<Vec<_>>();
            match matching.as_slice() {
                [] if authority == BackupTopologyAuthority::Authoritative => {
                    Ok(RemoteObservation::Missing)
                }
                [] => Ok(RemoteObservation::Unavailable(
                    RemoteFailureKind::InvalidResponse,
                )),
                [backup] => {
                    let remote_id = RemoteId::new(backup.backup_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidBackupId)?;
                    Ok(RemoteObservation::Present(RemoteResource::new(
                        remote_id,
                        backup_properties(address, compiled, backup, target, externals),
                    )))
                }
                _ => Err(DiscoverRemoteError::DuplicateBackupCollision),
            }
        }
        Some(Err(error)) => Ok(RemoteObservation::Unavailable(classify_sdk_error(error))),
        None => Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        )),
    }
}

fn backup_properties(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    backup: &BackupDetails,
    logical_target: &ResourceAddress,
    externals: &ExternalDirectory,
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
            PropertyPath::Destination => externals.observe_association(
                SelectorKind::Destination,
                false,
                ResponseField::Value(backup.destination_id.as_str()),
            ),
            PropertyPath::Schedule => known(serde_json::json!(backup.schedule)),
            PropertyPath::Prefix => known(serde_json::json!(backup.prefix)),
            PropertyPath::Database => known(serde_json::json!(backup.database)),
            PropertyPath::Enabled => match backup.enabled {
                Some(enabled) => known(serde_json::json!(enabled)),
                None => PropertyObservation::KnownAbsent,
            },
            PropertyPath::KeepLatest => match backup.keep_latest_count {
                Some(count) => known(serde_json::json!(count.get())),
                None => PropertyObservation::KnownAbsent,
            },
            PropertyPath::IncludeEncryptionKey => {
                known(serde_json::json!(backup.include_encryption_key))
            }
            _ => continue,
        };
        properties.insert(path.clone(), observed);
    }

    properties
}

fn known(value: serde_json::Value) -> PropertyObservation {
    PropertyObservation::Known(
        ComparableValue::try_from_json(value).expect("Backup response values are non-null"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_five_database_kinds_map_to_backup_targets() {
        for (kind, expected) in [
            (ResourceKind::Postgres, "postgres"),
            (ResourceKind::MySql, "mysql"),
            (ResourceKind::MariaDb, "mariadb"),
            (ResourceKind::Mongo, "mongo"),
            (ResourceKind::LibSql, "libsql"),
        ] {
            let target = backup_target(kind, "id-1").expect("supported database kind");
            let rendered = format!("{target:?}");
            assert!(rendered.to_lowercase().starts_with(expected), "{rendered}");
        }
        for kind in [
            ResourceKind::Application,
            ResourceKind::Compose,
            ResourceKind::Redis,
            ResourceKind::Project,
            ResourceKind::Environment,
            ResourceKind::Domain,
            ResourceKind::Port,
            ResourceKind::Redirect,
            ResourceKind::Security,
            ResourceKind::Mount,
            ResourceKind::Schedule,
            ResourceKind::Backup,
        ] {
            assert!(backup_target(kind, "id-1").is_none(), "{kind}");
        }
    }

    #[test]
    fn prefix_normalization_mirrors_the_runtime_collision_rule() {
        for (prefix, normalized) in [
            ("nightly", "nightly/"),
            ("/nightly/", "nightly/"),
            (" /a/b/ ", "a/b/"),
            ("/", ""),
            ("  ", ""),
        ] {
            assert_eq!(normalize_backup_prefix(prefix), normalized, "{prefix:?}");
        }
    }
}
