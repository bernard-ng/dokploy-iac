//! Authoritative discovery for application-contained leaf resources that
//! Dokploy lists only through `application.one`: Redirects and Security entries.
//!
//! Both kinds share one contract. The parent collection is read once per
//! application. Every stored identity is also read directly, and the direct and
//! collection records must agree exactly. Unmanaged resources are matched by a
//! kind-specific application-scoped collision key. Absence is proven only when
//! the parent collection is authoritative and does not contain the identity.

use std::collections::{BTreeMap, BTreeSet};

use dokploy_core::{
    ComparableValue, PropertyObservation, PropertyPath, PropertyUnknownReason, RemoteFailureKind,
    RemoteObservation, RemoteResource,
};
use dokploy_sdk::{ApplicationId, Dokploy, Error as SdkError, RedirectId, SecurityId};
use dokploy_state::{RemoteId, ResourceAddress, ResourceKind, StateFile};

use super::{
    DiscoverRemoteError, classify_sdk_error, desired_resource_for_observation, observation,
    trusted_application_id,
};
use crate::desired::CompiledDesired;

/// One kind-neutral, secret-free view of a Redirect or Security record.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LeafRecord {
    id: String,
    application_id: String,
    key: String,
    properties: BTreeMap<PropertyPath, PropertyObservation>,
}

struct LeafCollection {
    application_id: String,
    records: Vec<LeafRecord>,
}

enum LeafFailure {
    Containment,
    InvalidId,
    DuplicateCollision,
    DuplicateId,
    Conflict,
}

fn leaf_error(kind: ResourceKind, failure: LeafFailure) -> DiscoverRemoteError {
    match (kind, failure) {
        (ResourceKind::Redirect, LeafFailure::Containment) => {
            DiscoverRemoteError::RedirectContainment
        }
        (ResourceKind::Redirect, LeafFailure::InvalidId) => DiscoverRemoteError::InvalidRedirectId,
        (ResourceKind::Redirect, LeafFailure::DuplicateCollision) => {
            DiscoverRemoteError::DuplicateRedirectCollision
        }
        (ResourceKind::Redirect, LeafFailure::DuplicateId) => {
            DiscoverRemoteError::DuplicateRedirectId
        }
        (ResourceKind::Redirect, LeafFailure::Conflict) => {
            DiscoverRemoteError::RedirectTopologyConflict
        }
        (ResourceKind::Security, LeafFailure::Containment) => {
            DiscoverRemoteError::SecurityContainment
        }
        (ResourceKind::Security, LeafFailure::InvalidId) => DiscoverRemoteError::InvalidSecurityId,
        (ResourceKind::Security, LeafFailure::DuplicateCollision) => {
            DiscoverRemoteError::DuplicateSecurityCollision
        }
        (ResourceKind::Security, LeafFailure::DuplicateId) => {
            DiscoverRemoteError::DuplicateSecurityId
        }
        (ResourceKind::Security, LeafFailure::Conflict) => {
            DiscoverRemoteError::SecurityTopologyConflict
        }
        _ => unreachable!("only Redirect and Security use leaf discovery"),
    }
}

fn known(value: serde_json::Value) -> PropertyObservation {
    PropertyObservation::Known(
        ComparableValue::try_from_json(value).expect("typed leaf response values are non-null"),
    )
}

fn redirect_record(details: &dokploy_sdk::RedirectDetails) -> LeafRecord {
    LeafRecord {
        id: details.redirect_id.as_str().to_owned(),
        application_id: details.application_id.as_str().to_owned(),
        key: details.regex.clone(),
        properties: BTreeMap::from([
            (PropertyPath::Regex, known(serde_json::json!(details.regex))),
            (
                PropertyPath::Replacement,
                known(serde_json::json!(details.replacement)),
            ),
            (
                PropertyPath::Permanent,
                known(serde_json::json!(details.permanent)),
            ),
        ]),
    }
}

fn security_record(details: &dokploy_sdk::SecurityDetails) -> LeafRecord {
    LeafRecord {
        id: details.security_id.as_str().to_owned(),
        application_id: details.application_id.as_str().to_owned(),
        key: details.username.clone(),
        properties: BTreeMap::from([
            (
                PropertyPath::Username,
                known(serde_json::json!(details.username)),
            ),
            (
                PropertyPath::Password,
                if details.password_present {
                    PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)
                } else {
                    PropertyObservation::KnownAbsent
                },
            ),
        ]),
    }
}

async fn read_collection(
    client: &Dokploy,
    kind: ResourceKind,
    application_id: &str,
) -> Result<LeafCollection, SdkError> {
    let application = ApplicationId::new(application_id);
    match kind {
        ResourceKind::Redirect => {
            let collection = client.redirects().by_application(application).await?;
            Ok(LeafCollection {
                application_id: collection.application_id().as_str().to_owned(),
                records: collection.redirects().iter().map(redirect_record).collect(),
            })
        }
        ResourceKind::Security => {
            let collection = client.security().by_application(application).await?;
            Ok(LeafCollection {
                application_id: collection.application_id().as_str().to_owned(),
                records: collection.entries().iter().map(security_record).collect(),
            })
        }
        _ => unreachable!("only Redirect and Security use leaf discovery"),
    }
}

async fn read_direct(
    client: &Dokploy,
    kind: ResourceKind,
    remote_id: &str,
) -> Result<LeafRecord, SdkError> {
    match kind {
        ResourceKind::Redirect => client
            .redirects()
            .get(RedirectId::new(remote_id))
            .await
            .map(|details| redirect_record(&details)),
        ResourceKind::Security => client
            .security()
            .get(SecurityId::new(remote_id))
            .await
            .map(|details| security_record(&details)),
        _ => unreachable!("only Redirect and Security use leaf discovery"),
    }
}

fn desired_key<'a>(
    kind: ResourceKind,
    address: &ResourceAddress,
    compiled: &'a CompiledDesired,
) -> Option<&'a str> {
    match kind {
        ResourceKind::Redirect => compiled.bindings().redirect_regex(address),
        ResourceKind::Security => compiled.bindings().security_username(address),
        _ => None,
    }
}

type Collections = BTreeMap<String, Result<LeafCollection, SdkError>>;

/// Discovers every desired, stored, or removed leaf of one kind.
///
/// `authoritative` states whether absence from `application.one` proves
/// nonexistence for the caller's role.
pub(super) async fn discover_leaf_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    kind: ResourceKind,
    authoritative: bool,
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
        .filter(|address| address.kind() == kind)
        .cloned()
        .collect();
    let mut collections: Collections = BTreeMap::new();

    for address in &addresses {
        let mut parents = Vec::new();
        if desired.resources().contains_key(address) {
            parents.push(parent_from_desired(kind, address, compiled, state)?);
        }
        if state.resource(address).is_some() {
            parents.push(parent_from_state(kind, address, state)?);
        }
        parents.sort();
        parents.dedup();
        for parent in parents {
            let Some(application_id) = trusted_application_id(&parent, compiled, state, topology)
            else {
                continue;
            };
            if collections.contains_key(&application_id) {
                continue;
            }
            let collection = read_collection(client, kind, &application_id).await;
            collections.insert(application_id, collection);
        }
    }
    validate_collections(kind, &collections)?;

    let mut seen_direct_ids = BTreeSet::new();
    let mut observations = Vec::new();
    for address in addresses {
        let observed = if let Some(stored) = state.resource(&address) {
            let current_parent = parent_from_state(kind, &address, state)?;
            let Some(current_application_id) =
                trusted_application_id(&current_parent, compiled, state, topology)
            else {
                observations.push((
                    address,
                    inherited_parent_observation(&current_parent, topology),
                ));
                continue;
            };
            match read_direct(client, kind, stored.remote_id().as_str()).await {
                Ok(record) => {
                    let remote_id = RemoteId::new(&record.id)
                        .map_err(|_| leaf_error(kind, LeafFailure::InvalidId))?;
                    if remote_id != *stored.remote_id()
                        || record.application_id != current_application_id
                    {
                        return Err(leaf_error(kind, LeafFailure::Conflict));
                    }
                    match collections.get(&current_application_id) {
                        Some(Ok(collection)) => {
                            let matching = collection
                                .records
                                .iter()
                                .filter(|candidate| candidate.id == record.id)
                                .collect::<Vec<_>>();
                            if matching.as_slice() != [&record] {
                                return Err(leaf_error(kind, LeafFailure::Conflict));
                            }
                        }
                        Some(Err(error)) => {
                            observations.push((
                                address,
                                RemoteObservation::Unavailable(classify_sdk_error(error)),
                            ));
                            continue;
                        }
                        None => return Err(leaf_error(kind, LeafFailure::Containment)),
                    }
                    if !seen_direct_ids.insert(remote_id.clone()) {
                        return Err(leaf_error(kind, LeafFailure::DuplicateId));
                    }
                    if desired.resources().contains_key(&address) {
                        let desired_parent = parent_from_desired(kind, &address, compiled, state)?;
                        if desired_parent != current_parent {
                            match observe_under_parent(
                                kind,
                                &address,
                                &desired_parent,
                                compiled,
                                state,
                                topology,
                                &collections,
                                authoritative,
                            )? {
                                RemoteObservation::Present(_) => {
                                    return Err(leaf_error(kind, LeafFailure::DuplicateCollision));
                                }
                                RemoteObservation::Unavailable(failure) => {
                                    observations
                                        .push((address, RemoteObservation::Unavailable(failure)));
                                    continue;
                                }
                                RemoteObservation::Missing => {}
                            }
                        }
                    }
                    RemoteObservation::Present(RemoteResource::new(
                        remote_id,
                        leaf_properties(&address, compiled, &record),
                    ))
                }
                Err(SdkError::Api(error)) if error.status() == 404 => {
                    match collections.get(&current_application_id) {
                        Some(Ok(collection))
                            if collection
                                .records
                                .iter()
                                .all(|record| record.id != stored.remote_id().as_str()) =>
                        {
                            if authoritative {
                                RemoteObservation::Missing
                            } else {
                                RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
                            }
                        }
                        Some(Ok(_)) => {
                            RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
                        }
                        Some(Err(error)) => {
                            RemoteObservation::Unavailable(classify_sdk_error(error))
                        }
                        None => RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse),
                    }
                }
                Err(error) => RemoteObservation::Unavailable(classify_sdk_error(&error)),
            }
        } else {
            let parent = parent_from_desired(kind, &address, compiled, state)?;
            observe_under_parent(
                kind,
                &address,
                &parent,
                compiled,
                state,
                topology,
                &collections,
                authoritative,
            )?
        };
        observations.push((address, observed));
    }

    Ok(observations)
}

fn validate_collections(
    kind: ResourceKind,
    collections: &Collections,
) -> Result<(), DiscoverRemoteError> {
    let mut global_ids = BTreeSet::new();
    for (application_id, collection) in collections
        .iter()
        .filter_map(|(id, result)| result.as_ref().ok().map(|collection| (id, collection)))
    {
        if &collection.application_id != application_id {
            return Err(leaf_error(kind, LeafFailure::Containment));
        }
        let mut collisions = BTreeSet::new();
        for record in &collection.records {
            let remote_id =
                RemoteId::new(&record.id).map_err(|_| leaf_error(kind, LeafFailure::InvalidId))?;
            if &record.application_id != application_id {
                return Err(leaf_error(kind, LeafFailure::Containment));
            }
            if !global_ids.insert(remote_id) {
                return Err(leaf_error(kind, LeafFailure::DuplicateId));
            }
            if !collisions.insert(record.key.as_str()) {
                return Err(leaf_error(kind, LeafFailure::DuplicateCollision));
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_under_parent(
    kind: ResourceKind,
    address: &ResourceAddress,
    parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &Collections,
    authoritative: bool,
) -> Result<RemoteObservation, DiscoverRemoteError> {
    match observation(topology, parent) {
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
    let Some(application_id) = trusted_application_id(parent, compiled, state, topology) else {
        return Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        ));
    };
    let Some(key) = desired_key(kind, address, compiled) else {
        return Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        ));
    };
    match collections.get(&application_id) {
        Some(Ok(collection)) => {
            let matching = collection
                .records
                .iter()
                .filter(|record| record.key == key)
                .collect::<Vec<_>>();
            let record = match matching.as_slice() {
                [] if authoritative => return Ok(RemoteObservation::Missing),
                [] => {
                    return Ok(RemoteObservation::Unavailable(
                        RemoteFailureKind::InvalidResponse,
                    ));
                }
                [record] => *record,
                _ => return Err(leaf_error(kind, LeafFailure::DuplicateCollision)),
            };
            let remote_id =
                RemoteId::new(&record.id).map_err(|_| leaf_error(kind, LeafFailure::InvalidId))?;
            Ok(RemoteObservation::Present(RemoteResource::new(
                remote_id,
                leaf_properties(address, compiled, record),
            )))
        }
        Some(Err(error)) => Ok(RemoteObservation::Unavailable(classify_sdk_error(error))),
        None => Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        )),
    }
}

fn inherited_parent_observation(
    parent: &ResourceAddress,
    topology: &[(ResourceAddress, RemoteObservation)],
) -> RemoteObservation {
    match observation(topology, parent) {
        Some(RemoteObservation::Missing) => RemoteObservation::Missing,
        Some(RemoteObservation::Unavailable(failure)) => RemoteObservation::Unavailable(*failure),
        Some(RemoteObservation::Present(_)) | None => {
            RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
        }
    }
}

fn parent_from_desired(
    kind: ResourceKind,
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    compiled
        .desired_state()
        .resources()
        .get(address)
        .and_then(dokploy_core::DesiredResource::containment)
        .cloned()
        .or_else(|| {
            state
                .resource(address)
                .and_then(dokploy_state::ResourceState::containment)
                .cloned()
        })
        .filter(|parent| parent.kind() == ResourceKind::Application)
        .ok_or_else(|| leaf_error(kind, LeafFailure::Containment))
}

fn parent_from_state(
    kind: ResourceKind,
    address: &ResourceAddress,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    state
        .resource(address)
        .and_then(dokploy_state::ResourceState::containment)
        .cloned()
        .filter(|parent| parent.kind() == ResourceKind::Application)
        .ok_or_else(|| leaf_error(kind, LeafFailure::Containment))
}

/// Projects only the desired, non-ignored paths so unowned fields never enter a plan.
fn leaf_properties(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    record: &LeafRecord,
) -> BTreeMap<PropertyPath, PropertyObservation> {
    let mut properties = BTreeMap::new();
    let Some(desired) = desired_resource_for_observation(address, compiled) else {
        return properties;
    };

    for path in desired.properties().keys() {
        if desired.ignored_changes().contains(path) {
            continue;
        }
        if let Some(observed) = record.properties.get(path) {
            properties.insert(path.clone(), observed.clone());
        }
    }

    properties
}
