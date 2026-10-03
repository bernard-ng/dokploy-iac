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
use crate::selectors::SelectorIndex;

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
    /// The identity of the resource this address is being moved from. It is not a collision:
    /// the same resource found under the new address is the one that is moving.
    moved_from: Option<RemoteId>,
    /// The entries of keyed collections the document or state owns, which are observed one by
    /// one because the spec cannot name them.
    entries: Vec<PropertyPath>,
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
                    moved_from: None,
                    entries: owned_entries(compiled, address),
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
                            moved_from: None,
                            entries: Vec::new(),
                        },
                    )
                })
                .1
                .stored = Some(resource.remote_id().clone());
            if let Some((_, subject)) = subjects.get_mut(address) {
                // A collection the document owns as a whole is observed as a whole.
                let owned_roots: Vec<&str> = compiled
                    .desired
                    .resources()
                    .get(address)
                    .into_iter()
                    .flat_map(|resource| resource.properties().keys())
                    .filter(|path| path.is_collection_root())
                    .map(|path| path.info().path.as_str())
                    .collect();
                for path in stored_entries(specs, resource) {
                    let whole = path
                        .info()
                        .root
                        .as_deref()
                        .is_some_and(|root| owned_roots.contains(&root));
                    if !whole && !subject.entries.contains(&path) {
                        subject.entries.push(path);
                    }
                }
            }
        }
    }

    if let Some(state) = state {
        for directive in compiled.desired_for(Some(state))?.moves() {
            let source = state
                .resource(directive.from())
                .map(|resource| resource.remote_id().clone());
            if let Some((_, subject)) = subjects.get_mut(directive.to()) {
                subject.moved_from = source;
            }
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

    // The collections of the kinds the subjects select from serve both to turn the ids Dokploy
    // returns into names and to resolve the names the document uses.
    let targets = SelectorIndex::targets(specs, by_kind.keys().map(String::as_str));
    let selectors = SelectorIndex::load(transport, specs, &targets).await;

    let mut outcomes: BTreeMap<ResourceAddress, (String, Outcome)> = BTreeMap::new();
    for kind in kinds {
        let group = &by_kind[kind];
        let spec = specs
            .get(kind)
            .ok_or_else(|| EngineError::UnknownKind { kind: kind.clone() })?;
        let read = if !spec.parents.is_empty() {
            read_nested(transport, specs, spec, group, &outcomes, &selectors).await?
        } else {
            read_top_level(transport, spec, group, &selectors).await?
        };
        for (address, outcome) in read {
            outcomes.insert(address, (kind.clone(), outcome));
        }
    }

    let remote = build(specs, instance, outcomes)?;

    resolve_selectors(compiled, &selectors, remote)
}

fn depth(specs: &SpecRegistry, kind: &str) -> usize {
    fn deepest(specs: &SpecRegistry, kind: &str, budget: usize) -> usize {
        let Some(spec) = specs.get(kind) else {
            return 0;
        };
        if budget == 0 {
            return 0;
        }
        spec.parents
            .iter()
            .map(|parent| 1 + deepest(specs, parent, budget - 1))
            .max()
            .unwrap_or(0)
    }

    deepest(specs, kind, 32)
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
    selectors: &SelectorIndex,
) -> Result<Vec<(ResourceAddress, Outcome)>, EngineError> {
    let Some(list) = spec.api.read.list.as_ref() else {
        return Ok(direct_only(transport, spec, group, selectors).await);
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

    Ok(settle(transport, spec, list, group, listing, None, selectors).await)
}

/// A child kind: its collection is read once per parent, from what was learned about it.
async fn read_nested<T: Transport>(
    transport: &T,
    specs: &SpecRegistry,
    spec: &KindSpec,
    group: &[Subject<'_>],
    outcomes: &BTreeMap<ResourceAddress, (String, Outcome)>,
    selectors: &SelectorIndex,
) -> Result<Vec<(ResourceAddress, Outcome)>, EngineError> {
    let Some(list) = spec.api.read.list.as_ref() else {
        return Ok(direct_only(transport, spec, group, selectors).await);
    };
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
                let parent_kind = parent.map_or("", |parent| parent.kind().as_str());
                let parent_spec = specs
                    .get(parent_kind)
                    .ok_or_else(|| unsupported(spec, "the parent kind has no spec"))?;
                let listing = if let Some(embedded) = &list.embedded_in {
                    let one = parent_spec.api.read.one.as_ref();
                    if one.is_none_or(|one| {
                        embedded
                            .parent_op
                            .as_deref()
                            .is_some_and(|parent_op| one.op != parent_op)
                    }) {
                        return Err(unsupported(
                            spec,
                            "an embedded collection must come from the parent's direct read",
                        ));
                    }
                    match raw.pointer(&embedded.pointer) {
                        Some(Json::Array(items)) => bounded(items.clone()),
                        _ => Err(RemoteFailureKind::InvalidResponse),
                    }
                } else if let (Some(operation), Some(scope)) =
                    (list.op.as_deref(), list.scope.as_ref())
                {
                    let mut request = OperationRequest::new(operation);
                    for (name, value) in scope.query_for(Some(parent_kind), id.as_str()) {
                        request = request.query(name, value);
                    }
                    fetch_list(transport, request).await
                } else {
                    return Err(unsupported(
                        spec,
                        "a nested collection must be embedded in its parent or scoped by it",
                    ));
                };
                let attach = spec.parent_column(parent_kind).map(|field| Attachment {
                    field,
                    parent_id: id.as_str(),
                });
                results.extend(
                    settle(
                        transport,
                        spec,
                        list,
                        &owned,
                        listing,
                        attach.as_ref(),
                        selectors,
                    )
                    .await,
                );
            }
        }
    }

    Ok(results)
}

/// The most items one collection may hold. A response past it is not a collection this tool
/// can reason about, so it is refused instead of read.
pub(crate) const MAX_COLLECTION_ITEMS: usize = 10_000;

async fn fetch_list<T: Transport>(
    transport: &T,
    request: OperationRequest,
) -> Result<Vec<Json>, RemoteFailureKind> {
    match transport.call(request).await {
        Ok(Json::Array(items)) => bounded(items),
        Ok(_) => Err(RemoteFailureKind::InvalidResponse),
        Err(error) => Err(failure(&error)),
    }
}

fn bounded(items: Vec<Json>) -> Result<Vec<Json>, RemoteFailureKind> {
    if items.len() > MAX_COLLECTION_ITEMS {
        return Err(RemoteFailureKind::InvalidResponse);
    }

    Ok(items)
}

/// A kind with no collection read can only be found by the identity state recorded: absence
/// cannot be proven, so a resource without one is unavailable.
async fn direct_only<T: Transport>(
    transport: &T,
    spec: &KindSpec,
    group: &[Subject<'_>],
    selectors: &SelectorIndex,
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
                            properties: project(spec, &direct, selectors, &subject.entries),
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
    attachment: Option<&Attachment<'_>>,
    selectors: &SelectorIndex,
) -> Vec<(ResourceAddress, Outcome)> {
    let listing = match listing {
        Ok(items) => items,
        Err(kind) => return all(group, Outcome::Unavailable(kind)),
    };
    let by_id: BTreeMap<&str, &Json> = listing
        .iter()
        .filter_map(|item| item_id(spec, item).map(|id| (id, item)))
        .collect();
    // A collection that lists one identity twice, or names another parent's items, contradicts
    // itself: nothing can be concluded from it.
    let identified = listing
        .iter()
        .filter(|item| item_id(spec, item).is_some())
        .count();
    let foreign =
        attachment.is_some_and(|attachment| listing.iter().any(|item| !attachment.owns(item)));
    if by_id.len() != identified || foreign {
        return all(
            group,
            Outcome::Unavailable(RemoteFailureKind::InvalidResponse),
        );
    }

    let mut results = Vec::new();
    for subject in group {
        let outcome = if list.authority == Authority::Partial {
            // Absence from a partial collection proves nothing.
            match find(spec, subject, &by_id, &listing) {
                Found::Item(item) => {
                    resolve(
                        transport,
                        spec,
                        item,
                        attachment,
                        selectors,
                        &subject.entries,
                    )
                    .await
                }
                Found::Nothing | Found::Ambiguous => {
                    Outcome::Unavailable(RemoteFailureKind::Unavailable)
                }
            }
        } else {
            match find(spec, subject, &by_id, &listing) {
                Found::Item(item) => {
                    resolve(
                        transport,
                        spec,
                        item,
                        attachment,
                        selectors,
                        &subject.entries,
                    )
                    .await
                }
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
        subject
            .moved_from
            .as_ref()
            .is_none_or(|source| item_id(spec, item) != Some(source.as_str()))
            && wanted
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
async fn resolve<T: Transport>(
    transport: &T,
    spec: &KindSpec,
    listed: &Json,
    attachment: Option<&Attachment<'_>>,
    selectors: &SelectorIndex,
    entries: &[PropertyPath],
) -> Outcome {
    let Some(id) = item_id(spec, listed).and_then(|id| RemoteId::new(id).ok()) else {
        return Outcome::Unavailable(RemoteFailureKind::InvalidResponse);
    };
    let Some(one) = spec.api.read.one.as_ref() else {
        return Outcome::Present {
            properties: project(spec, listed, selectors, entries),
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
    if item_id(spec, &direct) != Some(id.as_str())
        || (one.agree && !agree(spec, listed, &direct))
        || attachment.is_some_and(|attachment| !attachment.owns(&direct))
    {
        return Outcome::Unavailable(RemoteFailureKind::InvalidResponse);
    }

    Outcome::Present {
        properties: project(spec, &direct, selectors, entries),
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

/// The entries of keyed collections the document owns.
fn owned_entries(compiled: &Compiled, address: &ResourceAddress) -> Vec<PropertyPath> {
    compiled
        .desired
        .resources()
        .get(address)
        .map(|resource| {
            resource
                .properties()
                .keys()
                .filter(|path| path.info().root.is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The entries of keyed collections state recorded: their values are receipts, under the
/// dotted path of the entry.
fn stored_entries(
    specs: &SpecRegistry,
    resource: &dokploy_state::ResourceState,
) -> Vec<PropertyPath> {
    let Some(spec) = specs.get(resource.kind().as_str()) else {
        return Vec::new();
    };

    let mut entries: Vec<PropertyPath> = resource
        .sensitive_inputs()
        .paths()
        .filter_map(|path| PropertyPath::from_spec(spec, path.as_str()).ok())
        .filter(|path| path.info().root.is_some())
        .collect();
    // The members of a set of selectors are not secret: they are in the managed inputs, under the
    // name of the field.
    if let Some(applied) = resource.last_applied().as_json().as_object() {
        for (field, value) in applied {
            let Some(members) = value.as_object() else {
                continue;
            };
            for member in members.keys() {
                if let Ok(path) = PropertyPath::from_spec(spec, &format!("{field}.{member}"))
                    && path.info().root.is_some()
                    && path.info().selector.is_some()
                {
                    entries.push(path);
                }
            }
        }
    }

    entries
}

/// Tells the planner what each selector the document sets means right now.
fn resolve_selectors(
    compiled: &Compiled,
    selectors: &SelectorIndex,
    remote: RemoteState,
) -> Result<RemoteState, EngineError> {
    let mut resolutions = Vec::new();
    for (address, resource) in &compiled.resources {
        for (path, value) in &resource.selectors {
            let Some(kind) = path.info().selector.as_deref() else {
                continue;
            };
            if let Some(resolution) = selectors.resolve(kind, value) {
                resolutions.push(((address.clone(), path.clone()), resolution));
            }
        }
    }

    remote
        .with_external_resolutions(resolutions)
        .map_err(EngineError::Remote)
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
                    .map(|spec| (address.clone(), crate::contract::mutation_contract(spec)))
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

/// How a child is tied to its parent in a response: the request field that holds the
/// parent's id. A response that names another parent is not this parent's child.
struct Attachment<'a> {
    field: &'a str,
    parent_id: &'a str,
}

impl Attachment<'_> {
    /// Whether `item` belongs to the parent. An item that does not say is taken to.
    fn owns(&self, item: &Json) -> bool {
        item.get(self.field)
            .is_none_or(|value| value.as_str() == Some(self.parent_id))
    }
}

/// The collection a kind's resources are found in, read fresh: the top-level list, the list
/// scoped by `parent_id`, or the collection embedded in the parent's direct read. Used before a
/// create whose identity is learned by diffing the collection.
pub(crate) async fn read_collection<T: Transport>(
    transport: &T,
    specs: &SpecRegistry,
    spec: &KindSpec,
    parent: Option<(&str, &str)>,
) -> Result<Vec<Json>, RemoteFailureKind> {
    let (parent_kind, parent_id) = parent.unzip();
    let list = spec
        .api
        .read
        .list
        .as_ref()
        .ok_or(RemoteFailureKind::Unavailable)?;
    if let Some(embedded) = &list.embedded_in {
        let parent_spec = parent_kind
            .and_then(|parent| specs.get(parent))
            .ok_or(RemoteFailureKind::Unavailable)?;
        let one = parent_spec
            .api
            .read
            .one
            .as_ref()
            .filter(|one| {
                embedded
                    .parent_op
                    .as_deref()
                    .is_none_or(|parent_op| one.op == parent_op)
            })
            .ok_or(RemoteFailureKind::Unavailable)?;
        let parent_id = parent_id.ok_or(RemoteFailureKind::Unavailable)?;
        let request = OperationRequest::new(&one.op).query(one.id_param.clone(), parent_id);
        let parent = transport
            .call(request)
            .await
            .map_err(|error| failure(&error))?;
        return match parent.pointer(&embedded.pointer) {
            Some(Json::Array(items)) => bounded(items.clone()),
            _ => Err(RemoteFailureKind::InvalidResponse),
        };
    }
    let operation = list.op.as_deref().ok_or(RemoteFailureKind::Unavailable)?;
    let mut request = OperationRequest::new(operation);
    if let Some(scope) = &list.scope {
        let parent_id = parent_id.ok_or(RemoteFailureKind::Unavailable)?;
        for (name, value) in scope.query_for(parent_kind, parent_id) {
            request = request.query(name, value);
        }
    }

    fetch_list(transport, request).await
}
