//! Fresh discovery of settings-scope resources.
//!
//! Settings resources live in their own document and state, so they have their
//! own discovery entry instead of riding the project chain. The authoritative
//! `tag.all` collection is the single source: a tag's identity, name, and color
//! come from it, and its absence is the proof of deletion. Nothing is read when
//! the document and state mention no tag.

use super::*;

/// Whether the tag collection is known to list every tag of the organization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagTopologyAuthority {
    /// Absence from `tag.all` proves nonexistence.
    Authoritative,
    /// Absence may be caused by role-dependent filtering.
    Partial,
}

/// Discovers every settings-scope resource the desired state or durable state names.
pub async fn discover_settings_remote(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    authority: TagTopologyAuthority,
) -> Result<RemoteState, DiscoverRemoteError> {
    if !instances_match(client, state) {
        return Err(DiscoverRemoteError::InstanceMismatch);
    }

    let observations = discover_tag_observations(client, compiled, state, authority).await;
    remote_state_with_contracts(state.instance().clone(), observations)
        .map_err(DiscoverRemoteError::InvalidRemoteState)
}

async fn discover_tag_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    authority: TagTopologyAuthority,
) -> Vec<(ResourceAddress, RemoteObservation)> {
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
        .filter(|address| address.kind() == ResourceKind::Tag)
        .cloned()
        .collect();
    if addresses.is_empty() {
        return Vec::new();
    }

    let collection = client.tags().all().await;
    addresses
        .into_iter()
        .map(|address| {
            let observation = match &collection {
                Err(error) => RemoteObservation::Unavailable(classify_sdk_error(error)),
                Ok(collection) => observe_tag(&address, compiled, state, collection, authority),
            };

            (address, observation)
        })
        .collect()
}

fn observe_tag(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    collection: &dokploy_sdk::TagCollection,
    authority: TagTopologyAuthority,
) -> RemoteObservation {
    let matched = match state.resource(address) {
        // A tracked tag is found by its stored identity, so a rename is seen as a diff.
        Some(stored) => collection
            .tags()
            .iter()
            .find(|tag| tag.tag_id.as_str() == stored.remote_id().as_str()),
        // An untracked tag is only ever found by the exact name it is declared with.
        None => {
            let desired_address = compiled
                .desired_state()
                .moves()
                .iter()
                .find(|directive| directive.from() == address)
                .map_or(address, |directive| directive.to());
            compiled
                .bindings()
                .tag_name(desired_address)
                .and_then(|name| collection.tags().iter().find(|tag| tag.name == name))
        }
    };
    let Some(tag) = matched else {
        return if authority == TagTopologyAuthority::Authoritative {
            RemoteObservation::Missing
        } else {
            RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
        };
    };

    let mut properties = BTreeMap::new();
    if property_is_requested(address, compiled, &PropertyPath::Name) {
        properties.insert(
            PropertyPath::Name,
            PropertyObservation::Known(
                ComparableValue::try_from_json(serde_json::json!(tag.name))
                    .expect("a tag name is comparable"),
            ),
        );
    }
    if property_is_requested(address, compiled, &PropertyPath::Color) {
        properties.insert(PropertyPath::Color, observe_string_field(&tag.color));
    }

    RemoteObservation::Present(RemoteResource::new(
        RemoteId::new(tag.tag_id.as_str()).expect("Dokploy tag IDs are non-empty"),
        properties,
    ))
}
