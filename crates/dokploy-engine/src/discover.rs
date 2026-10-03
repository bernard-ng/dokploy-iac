//! Discovery: the minimum reads that say what the remote holds (ADR 0007).
//!
//! One list per kind serves every address of that kind, and a direct read is made only for
//! a resource that matched. A kind that neither the document nor the state mentions is not
//! read. A failed read makes exactly the addresses it covers unavailable and blocks their
//! planning, nothing else.

use std::collections::BTreeMap;

use dokploy_core::{
    MutationContract, PropertyObservation, PropertyPath, RemoteFailureKind, RemoteObservation,
    RemoteResource, RemoteState, RemoteStateError,
};
use dokploy_sdk::{Error as SdkError, OperationRequest, Transport};
use dokploy_spec::{Authority, KindSpec, ListRead, SpecRegistry};
use dokploy_state::{InstanceIdentity, RemoteId, ResourceAddress, StateFile};
use serde_json::Value as Json;

use crate::EngineError;
use crate::compile::Compiled;
use crate::project::{field_value, item_id, project};

/// What was learned about one address; rebuilt into core types on demand.
#[derive(Clone)]
enum Outcome {
    Missing,
    Present {
        id: RemoteId,
        properties: BTreeMap<PropertyPath, PropertyObservation>,
        /// The response the properties came from. A child kind whose collection is embedded
        /// in it reads that collection from here, without another request.
        raw: Json,
    },
    Unavailable(RemoteFailureKind),
}

#[derive(Clone)]
struct Subject<'a> {
    address: ResourceAddress,
    parent: Option<ResourceAddress>,
    collision: Option<&'a BTreeMap<String, Json>>,
    stored: Option<RemoteId>,
}

pub(crate) async fn discover<T: Transport>(
    transport: &T,
    specs: &SpecRegistry,
    instance: &InstanceIdentity,
    compiled: &Compiled,
    state: Option<&StateFile>,
) -> Result<RemoteState, EngineError> {
    let mut subjects: BTreeMap<ResourceAddress, (String, Subject<'_>)> = BTreeMap::new();
    for (address, resource) in &compiled.resources {
        subjects.insert(
            address.clone(),
            (
                resource.spec_kind.clone(),
                Subject {
                    address: address.clone(),
                    parent: address.parent(),
                    collision: Some(&resource.collision),
                    stored: None,
                },
            ),
        );
    }
    if let Some(state) = state {
        for (address, resource) in state.resources() {
            subjects
                .entry(address.clone())
                .or_insert_with(|| {
                    (
                        resource.kind().as_str().to_owned(),
                        Subject {
                            address: address.clone(),
                            parent: address.parent(),
                            collision: None,
                            stored: None,
                        },
                    )
                })
                .1
                .stored = Some(resource.remote_id().clone());
        }
    }

    let mut by_kind: BTreeMap<String, Vec<Subject<'_>>> = BTreeMap::new();
    for (_, (kind, subject)) in subjects {
        by_kind.entry(kind).or_default().push(subject);
    }

    // Parents are read before their children: a child collection is scoped by, or embedded
    // in, what was just learned about its parent.
    let mut kinds: Vec<&String> = by_kind.keys().collect();
    kinds.sort_by_key(|kind| (depth(specs, kind), (*kind).clone()));

    let mut outcomes: BTreeMap<ResourceAddress, (String, Outcome)> = BTreeMap::new();
    for kind in kinds {
        let group = &by_kind[kind];
        let spec = specs
            .get(kind)
            .ok_or_else(|| EngineError::UnknownKind { kind: kind.clone() })?;
        let read = if spec.parent.is_some() {
            read_nested(transport, specs, spec, group, &outcomes).await?
        } else {
            read_top_level(transport, spec, group).await?
        };
        for (address, outcome) in read {
            outcomes.insert(address, (kind.clone(), outcome));
        }
    }

    build(specs, instance, outcomes)
}

fn depth(specs: &SpecRegistry, kind: &str) -> usize {
    let mut depth = 0;
    let mut current = specs.get(kind);
    while let Some(spec) = current {
        let Some(parent) = &spec.parent else { break };
        depth += 1;
        current = specs.get(parent);
        if depth > 32 {
            break;
        }
    }

    depth
}

fn unsupported(spec: &KindSpec, reason: &'static str) -> EngineError {
    EngineError::UnsupportedDiscovery {
        kind: spec.kind.clone(),
        reason,
    }
}

async fn read_top_level<T: Transport>(
    transport: &T,
    spec: &KindSpec,
    group: &[Subject<'_>],
) -> Result<Vec<(ResourceAddress, Outcome)>, EngineError> {
    let Some(list) = spec.api.read.list.as_ref() else {
        return Ok(direct_only(transport, spec, group).await);
    };
    if list.embedded_in.is_some() || list.scope.is_some() {
        return Err(unsupported(
            spec,
            "a top-level kind cannot have an embedded or scoped collection",
        ));
    }
    let operation = list
        .op
        .as_deref()
        .ok_or_else(|| unsupported(spec, "the spec declares no collection operation"))?;
    let listing = fetch_list(transport, OperationRequest::new(operation)).await;

    Ok(settle(transport, spec, list, group, listing).await)
}

/// A child kind: its collection is read once per parent, from what was learned about it.
async fn read_nested<T: Transport>(
    transport: &T,
    specs: &SpecRegistry,
    spec: &KindSpec,
    group: &[Subject<'_>],
    outcomes: &BTreeMap<ResourceAddress, (String, Outcome)>,
) -> Result<Vec<(ResourceAddress, Outcome)>, EngineError> {
    let Some(list) = spec.api.read.list.as_ref() else {
        return Ok(direct_only(transport, spec, group).await);
    };
    let parent_spec = spec
        .parent
        .as_deref()
        .and_then(|parent| specs.get(parent))
        .ok_or_else(|| unsupported(spec, "the parent kind has no spec"))?;

    let mut by_parent: BTreeMap<Option<&ResourceAddress>, Vec<&Subject<'_>>> = BTreeMap::new();
    for subject in group {
        by_parent
            .entry(subject.parent.as_ref())
            .or_default()
            .push(subject);
    }

    let mut results = Vec::new();
    for (parent, subjects) in by_parent {
        let owned: Vec<Subject<'_>> = subjects.iter().map(|s| (*s).clone()).collect();
        let observed = parent
            .and_then(|parent| outcomes.get(parent))
            .map(|(_, outcome)| outcome);
        match observed {
            None => results.extend(all(
                &owned,
                Outcome::Unavailable(RemoteFailureKind::InvalidResponse),
            )),
            Some(Outcome::Missing) => results.extend(all(&owned, Outcome::Missing)),
            Some(Outcome::Unavailable(kind)) => {
                results.extend(all(&owned, Outcome::Unavailable(*kind)));
            }
            Some(Outcome::Present { id, raw, .. }) => {
                let listing = if let Some(embedded) = &list.embedded_in {
                    let one = parent_spec.api.read.one.as_ref();
                    if one.is_none_or(|one| one.op != embedded.parent_op) {
                        return Err(unsupported(
                            spec,
                            "an embedded collection must come from the parent's direct read",
                        ));
                    }
                    match raw.pointer(&embedded.pointer) {
                        Some(Json::Array(items)) => Ok(items.clone()),
                        _ => Err(RemoteFailureKind::InvalidResponse),
                    }
                } else if let (Some(operation), Some(scope)) =
                    (list.op.as_deref(), list.scope.as_ref())
                {
                    fetch_list(
                        transport,
                        OperationRequest::new(operation).query(scope.param.clone(), id.as_str()),
                    )
                    .await
                } else {
                    return Err(unsupported(
                        spec,
                        "a nested collection must be embedded in its parent or scoped by it",
                    ));
                };
                results.extend(settle(transport, spec, list, &owned, listing).await);
            }
        }
    }

    Ok(results)
}

async fn fetch_list<T: Transport>(
    transport: &T,
    request: OperationRequest,
) -> Result<Vec<Json>, RemoteFailureKind> {
    match transport.call(request).await {
        Ok(Json::Array(items)) => Ok(items),
        Ok(_) => Err(RemoteFailureKind::InvalidResponse),
        Err(error) => Err(failure(&error)),
    }
}

/// A kind with no collection read can only be found by the identity state recorded: absence
/// cannot be proven, so a resource without one is unavailable.
async fn direct_only<T: Transport>(
    transport: &T,
    spec: &KindSpec,
    group: &[Subject<'_>],
) -> Vec<(ResourceAddress, Outcome)> {
    let mut results = Vec::new();
    for subject in group {
        let outcome = match (&subject.stored, spec.api.read.one.as_ref()) {
            (Some(id), Some(one)) => {
                let request =
                    OperationRequest::new(&one.op).query(one.id_param.clone(), id.as_str());
                match transport.call(request).await {
                    Ok(direct)
                        if direct.is_object() && item_id(spec, &direct) == Some(id.as_str()) =>
                    {
                        Outcome::Present {
                            properties: project(spec, &direct),
                            id: id.clone(),
                            raw: direct,
                        }
                    }
                    Ok(_) => Outcome::Unavailable(RemoteFailureKind::InvalidResponse),
                    Err(SdkError::Api(detail)) if detail.status() == 404 => Outcome::Missing,
                    Err(error) => Outcome::Unavailable(failure(&error)),
                }
            }
            _ => Outcome::Unavailable(RemoteFailureKind::Unavailable),
        };
        results.push((subject.address.clone(), outcome));
    }

    results
}

/// Matches each subject against one collection and reads the matches directly.
async fn settle<T: Transport>(
    transport: &T,
    spec: &KindSpec,
    list: &ListRead,
    group: &[Subject<'_>],
    listing: Result<Vec<Json>, RemoteFailureKind>,
) -> Vec<(ResourceAddress, Outcome)> {
    let listing = match listing {
        Ok(items) => items,
        Err(kind) => return all(group, Outcome::Unavailable(kind)),
    };
    let by_id: BTreeMap<&str, &Json> = listing
        .iter()
        .filter_map(|item| item_id(spec, item).map(|id| (id, item)))
        .collect();

    let mut results = Vec::new();
    for subject in group {
        let outcome = if list.authority == Authority::Partial {
            // Absence from a partial collection proves nothing.
            match find(spec, subject, &by_id, &listing) {
                Found::Item(item) => resolve(transport, spec, item).await,
                Found::Nothing | Found::Ambiguous => {
                    Outcome::Unavailable(RemoteFailureKind::Unavailable)
                }
            }
        } else {
            match find(spec, subject, &by_id, &listing) {
                Found::Item(item) => resolve(transport, spec, item).await,
                Found::Nothing => Outcome::Missing,
                Found::Ambiguous => Outcome::Unavailable(RemoteFailureKind::InvalidResponse),
            }
        };
        results.push((subject.address.clone(), outcome));
    }

    results
}

fn all(group: &[Subject<'_>], outcome: Outcome) -> Vec<(ResourceAddress, Outcome)> {
    group
        .iter()
        .map(|subject| (subject.address.clone(), outcome.clone()))
        .collect()
}

enum Found<'a> {
    Item(&'a Json),
    Nothing,
    Ambiguous,
}

/// Finds the item for a subject: by the id state recorded, otherwise by the kind's
/// collision fields. An address with neither matches nothing.
fn find<'a>(
    spec: &KindSpec,
    subject: &Subject<'_>,
    by_id: &BTreeMap<&str, &'a Json>,
    listing: &'a [Json],
) -> Found<'a> {
    if let Some(id) = &subject.stored {
        return by_id
            .get(id.as_str())
            .map_or(Found::Nothing, |item| Found::Item(item));
    }
    let Some(wanted) = subject.collision.filter(|wanted| !wanted.is_empty()) else {
        return Found::Nothing;
    };
    let mut matches = listing.iter().filter(|item| {
        wanted
            .iter()
            .all(|(name, value)| field_value(spec, name, item).as_ref() == Some(value))
    });
    match (matches.next(), matches.next()) {
        (None, _) => Found::Nothing,
        (Some(item), None) => Found::Item(item),
        (Some(_), Some(_)) => Found::Ambiguous,
    }
}

/// Turns a matched list item into an outcome, reading and cross-checking the resource
/// directly when the spec asks for agreement.
async fn resolve<T: Transport>(transport: &T, spec: &KindSpec, listed: &Json) -> Outcome {
    let Some(id) = item_id(spec, listed).and_then(|id| RemoteId::new(id).ok()) else {
        return Outcome::Unavailable(RemoteFailureKind::InvalidResponse);
    };
    let Some(one) = spec.api.read.one.as_ref() else {
        return Outcome::Present {
            properties: project(spec, listed),
            id,
            raw: listed.clone(),
        };
    };
    let request = OperationRequest::new(&one.op).query(one.id_param.clone(), id.as_str());
    let direct = match transport.call(request).await {
        Ok(direct) if direct.is_object() => direct,
        Ok(_) => return Outcome::Unavailable(RemoteFailureKind::InvalidResponse),
        Err(error) => return Outcome::Unavailable(failure(&error)),
    };
    if item_id(spec, &direct) != Some(id.as_str()) || (one.agree && !agree(spec, listed, &direct)) {
        return Outcome::Unavailable(RemoteFailureKind::InvalidResponse);
    }

    Outcome::Present {
        properties: project(spec, &direct),
        id,
        raw: direct,
    }
}

/// The collection item and the direct read must agree on every field both return.
fn agree(spec: &KindSpec, listed: &Json, direct: &Json) -> bool {
    spec.fields
        .iter()
        .filter(|(_, field)| field.class == dokploy_spec::ValueClass::Public)
        .all(|(name, _)| {
            match (
                field_value(spec, name, listed),
                field_value(spec, name, direct),
            ) {
                (Some(left), Some(right)) => left == right,
                _ => true,
            }
        })
}

fn failure(error: &SdkError) -> RemoteFailureKind {
    match error {
        SdkError::Api(detail) if matches!(detail.status(), 401 | 403) => {
            RemoteFailureKind::Unauthorized
        }
        SdkError::Decode { .. } | SdkError::UnexpectedResponse { .. } => {
            RemoteFailureKind::InvalidResponse
        }
        _ => RemoteFailureKind::Unavailable,
    }
}

/// Builds the planner's remote snapshot. A resource whose observation the planner rejects
/// becomes unavailable, so one bad response never fails the rest.
fn build(
    specs: &SpecRegistry,
    instance: &InstanceIdentity,
    mut outcomes: BTreeMap<ResourceAddress, (String, Outcome)>,
) -> Result<RemoteState, EngineError> {
    for _ in 0..=outcomes.len() {
        let observations: Vec<(ResourceAddress, RemoteObservation)> = outcomes
            .iter()
            .map(|(address, (_, outcome))| (address.clone(), observation(outcome)))
            .collect();
        let contracts: Vec<(ResourceAddress, MutationContract)> = outcomes
            .iter()
            .filter_map(|(address, (kind, _))| {
                specs
                    .get(kind)
                    .map(|spec| (address.clone(), MutationContract::from_spec(spec)))
            })
            .collect();
        match RemoteState::try_new_with_contracts(instance.clone(), observations, contracts) {
            Ok(state) => return Ok(state),
            Err(
                RemoteStateError::InvalidPropertyObservation { address }
                | RemoteStateError::InvalidPropertyPath { address }
                | RemoteStateError::ConflictingPropertyPaths { address }
                | RemoteStateError::DuplicateRemoteIdentity {
                    second: address, ..
                },
            ) => {
                let Some(entry) = outcomes.get_mut(&address) else {
                    return Err(EngineError::Remote(RemoteStateError::DuplicateAddress {
                        address,
                    }));
                };
                entry.1 = Outcome::Unavailable(RemoteFailureKind::InvalidResponse);
            }
            Err(other) => return Err(EngineError::Remote(other)),
        }
    }

    Err(EngineError::Remote(
        RemoteStateError::MutationContractMismatch,
    ))
}

fn observation(outcome: &Outcome) -> RemoteObservation {
    match outcome {
        Outcome::Missing => RemoteObservation::Missing,
        Outcome::Unavailable(kind) => RemoteObservation::Unavailable(*kind),
        Outcome::Present { id, properties, .. } => {
            RemoteObservation::Present(RemoteResource::new(id.clone(), properties.clone()))
        }
    }
}

/// The collection a kind's resources are found in, read fresh: the top-level list, the list
/// scoped by `parent_id`, or the collection embedded in the parent's direct read. Used before a
/// create whose identity is learned by diffing the collection.
pub(crate) async fn read_collection<T: Transport>(
    transport: &T,
    specs: &SpecRegistry,
    spec: &KindSpec,
    parent_id: Option<&str>,
) -> Result<Vec<Json>, RemoteFailureKind> {
    let list = spec
        .api
        .read
        .list
        .as_ref()
        .ok_or(RemoteFailureKind::Unavailable)?;
    if let Some(embedded) = &list.embedded_in {
        let parent_spec = spec
            .parent
            .as_deref()
            .and_then(|parent| specs.get(parent))
            .ok_or(RemoteFailureKind::Unavailable)?;
        let one = parent_spec
            .api
            .read
            .one
            .as_ref()
            .filter(|one| one.op == embedded.parent_op)
            .ok_or(RemoteFailureKind::Unavailable)?;
        let parent_id = parent_id.ok_or(RemoteFailureKind::Unavailable)?;
        let request = OperationRequest::new(&one.op).query(one.id_param.clone(), parent_id);
        let parent = transport
            .call(request)
            .await
            .map_err(|error| failure(&error))?;
        return match parent.pointer(&embedded.pointer) {
            Some(Json::Array(items)) => Ok(items.clone()),
            _ => Err(RemoteFailureKind::InvalidResponse),
        };
    }
    let operation = list.op.as_deref().ok_or(RemoteFailureKind::Unavailable)?;
    let mut request = OperationRequest::new(operation);
    if let Some(scope) = &list.scope {
        let parent_id = parent_id.ok_or(RemoteFailureKind::Unavailable)?;
        request = request.query(scope.param.clone(), parent_id);
    }

    fetch_list(transport, request).await
}
