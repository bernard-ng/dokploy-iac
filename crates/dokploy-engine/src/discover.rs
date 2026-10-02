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
use dokploy_spec::{Authority, KindSpec, SpecRegistry};
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
    },
    Unavailable(RemoteFailureKind),
}

struct Subject<'a> {
    address: ResourceAddress,
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

    let mut outcomes: BTreeMap<ResourceAddress, (String, Outcome)> = BTreeMap::new();
    for (kind, group) in &by_kind {
        let spec = specs
            .get(kind)
            .ok_or_else(|| EngineError::UnknownKind { kind: kind.clone() })?;
        for (address, outcome) in read_kind(transport, spec, group).await? {
            outcomes.insert(address, (kind.clone(), outcome));
        }
    }

    build(specs, instance, outcomes)
}

async fn read_kind<T: Transport>(
    transport: &T,
    spec: &KindSpec,
    group: &[Subject<'_>],
) -> Result<Vec<(ResourceAddress, Outcome)>, EngineError> {
    let unsupported = |reason| EngineError::UnsupportedDiscovery {
        kind: spec.kind.clone(),
        reason,
    };
    if spec.parent.is_some() {
        return Err(unsupported(
            "nested kinds are discovered through their parent, which is not implemented yet",
        ));
    }
    let list = spec
        .api
        .read
        .list
        .as_ref()
        .ok_or_else(|| unsupported("the spec declares no collection read"))?;
    if list.embedded_in.is_some() {
        return Err(unsupported("embedded collections are not implemented yet"));
    }
    let list_op = list
        .op
        .as_deref()
        .ok_or_else(|| unsupported("the spec declares no collection operation"))?;

    let listing = match transport.call(OperationRequest::new(list_op)).await {
        Ok(Json::Array(items)) => items,
        Ok(_) => {
            return Ok(all(
                group,
                Outcome::Unavailable(RemoteFailureKind::InvalidResponse),
            ));
        }
        Err(error) => return Ok(all(group, Outcome::Unavailable(failure(&error)))),
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

    Ok(results)
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
        Outcome::Present { id, properties } => {
            RemoteObservation::Present(RemoteResource::new(id.clone(), properties.clone()))
        }
    }
}
