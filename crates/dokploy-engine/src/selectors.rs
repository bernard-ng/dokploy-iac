//! Selectors: names of resources outside the document, resolved against fresh reads
//! (ADR 0007, ADR 0013).
//!
//! A `selector(kind)` field holds `{ name }` (or `{ local: true }` for a server). Dokploy
//! holds the target's id. The index reads each target kind's collection once, so the same
//! reads serve three jobs: turning an id Dokploy returned back into the name the document
//! uses, resolving the document's name to an id before the planner runs, and resolving it
//! again right before a write. A name that matches nothing or more than one resource never
//! becomes an id.

use std::collections::{BTreeMap, BTreeSet};

use dokploy_core::{ExternalResolution, RemoteFailureKind};
use dokploy_sdk::Transport;
use dokploy_spec::SpecRegistry;
use dokploy_state::RemoteId;
use serde_json::Value as Json;

use crate::discover::read_collection;
use crate::project::{field_value, item_id};

/// The kind whose selectors may also say `{ local: true }`: the Dokploy host itself.
const LOCAL_KIND: &str = "server";

struct Entry {
    id: String,
    name: String,
    parent_id: Option<String>,
}

/// The names and ids of every selector target kind that was read.
#[derive(Default)]
pub(crate) struct SelectorIndex {
    /// A kind that was not read (no spec, or nested) is absent: its selectors are unobserved.
    kinds: BTreeMap<String, Result<Vec<Entry>, RemoteFailureKind>>,
}

impl SelectorIndex {
    /// The selector target kinds the specs of `kinds` refer to.
    pub(crate) fn targets<'k>(
        specs: &SpecRegistry,
        kinds: impl IntoIterator<Item = &'k str>,
    ) -> BTreeSet<String> {
        kinds
            .into_iter()
            .filter_map(|kind| specs.get(kind))
            .flat_map(|spec| spec.properties())
            .flat_map(|info| {
                // A set of selectors names its target on the element type of its root.
                info.selector
                    .or_else(|| info.ty.selector_set_kind().map(str::to_owned))
                    .into_iter()
                    .chain(info.key_selector)
            })
            .collect()
    }

    /// Reads the collection of each target kind that has a top-level collection.
    pub(crate) async fn load<T: Transport>(
        transport: &T,
        specs: &SpecRegistry,
        targets: &BTreeSet<String>,
    ) -> Self {
        let mut kinds = BTreeMap::new();
        for target in targets {
            let Some(spec) = specs.get(target) else {
                continue;
            };
            let Some(key) = spec.identity.key.as_deref() else {
                continue;
            };
            if spec.api.read.list.is_none() {
                continue;
            }
            let entries = if spec.parents.is_empty() {
                read_collection(transport, specs, spec, None)
                    .await
                    .and_then(|items| entries(spec, key, &items, None))
            } else if spec.parents.len() == 1 {
                let parent_kind = &spec.parents[0];
                let Some(parent_spec) = specs.get(parent_kind) else {
                    continue;
                };
                let Some(parent_key) = parent_spec.identity.key.as_deref() else {
                    continue;
                };
                match read_collection(transport, specs, parent_spec, None).await {
                    Err(failure) => Err(failure),
                    Ok(parents) => {
                        let mut children = Vec::new();
                        let mut failure = None;
                        for parent in parents {
                            let Some(parent_id) = item_id(parent_spec, &parent) else {
                                failure = Some(RemoteFailureKind::InvalidResponse);
                                break;
                            };
                            // Validate the parent collection's identity while it is here. A scoped
                            // child selector needs a unique parent name as well as a child name.
                            if field_value(parent_spec, parent_key, &parent).is_none() {
                                failure = Some(RemoteFailureKind::InvalidResponse);
                                break;
                            }
                            match read_collection(
                                transport,
                                specs,
                                spec,
                                Some((parent_kind, parent_id)),
                            )
                            .await
                            {
                                Ok(items) => match entries(spec, key, &items, Some(parent_id)) {
                                    Ok(mut found) => children.append(&mut found),
                                    Err(error) => {
                                        failure = Some(error);
                                        break;
                                    }
                                },
                                Err(error) => {
                                    failure = Some(error);
                                    break;
                                }
                            }
                        }
                        failure.map_or(Ok(children), Err)
                    }
                }
            } else {
                Err(RemoteFailureKind::Unavailable)
            };
            kinds.insert(target.clone(), entries);
        }

        Self { kinds }
    }

    /// The name of the resource Dokploy holds under `id`, when its collection was read and
    /// lists it.
    pub(crate) fn name_of(&self, kind: &str, id: &str) -> Option<&str> {
        let Some(Ok(entries)) = self.kinds.get(kind) else {
            return None;
        };

        entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.name.as_str())
    }

    /// A resource the apply just made, so a selector that names it resolves from now on.
    pub(crate) fn record(&mut self, kind: &str, id: &str, name: &str) {
        let entry = Entry {
            id: id.to_owned(),
            name: name.to_owned(),
            parent_id: None,
        };
        match self.kinds.entry(kind.to_owned()) {
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if let Ok(entries) = slot.get_mut()
                    && !entries.iter().any(|existing| existing.id == entry.id)
                {
                    entries.push(entry);
                }
            }
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(Ok(vec![entry]));
            }
        }
    }

    /// What a selector value means now: the id of the one resource with that name, `Local`,
    /// or why it cannot be resolved. `None` when the kind was not read.
    pub(crate) fn resolve(&self, kind: &str, selector: &Json) -> Option<ExternalResolution> {
        if selector.get("local").and_then(Json::as_bool) == Some(true) {
            return (kind == LOCAL_KIND).then_some(ExternalResolution::Local);
        }
        let name = selector.get("name")?.as_str()?;
        if let Some(scope) = selector.get("scope") {
            let parent_kind = scope.get("kind")?.as_str()?;
            let parent_name = scope.get("name")?.as_str()?;
            let ExternalResolution::Resolved(parent_id) =
                self.resolve(parent_kind, &serde_json::json!({ "name": parent_name }))?
            else {
                return Some(ExternalResolution::Unmatched);
            };
            return self.resolve_scoped(kind, name, parent_id.as_str());
        }
        let entries = match self.kinds.get(kind)? {
            Ok(entries) => entries,
            Err(failure) => return Some(ExternalResolution::Unavailable(*failure)),
        };
        let mut matches = entries.iter().filter(|entry| entry.name == name);

        Some(match (matches.next(), matches.next()) {
            (None, _) => ExternalResolution::Unmatched,
            (Some(entry), None) => RemoteId::new(entry.id.clone())
                .map_or(ExternalResolution::Ambiguous, ExternalResolution::Resolved),
            (Some(_), Some(_)) => ExternalResolution::Ambiguous,
        })
    }

    /// Resolves a child name below one parent id.
    pub(crate) fn resolve_scoped(
        &self,
        kind: &str,
        name: &str,
        parent_id: &str,
    ) -> Option<ExternalResolution> {
        let entries = match self.kinds.get(kind)? {
            Ok(entries) => entries,
            Err(failure) => return Some(ExternalResolution::Unavailable(*failure)),
        };
        let mut matches = entries
            .iter()
            .filter(|entry| entry.name == name && entry.parent_id.as_deref() == Some(parent_id));

        Some(match (matches.next(), matches.next()) {
            (None, _) => ExternalResolution::Unmatched,
            (Some(entry), None) => RemoteId::new(entry.id.clone())
                .map_or(ExternalResolution::Ambiguous, ExternalResolution::Resolved),
            (Some(_), Some(_)) => ExternalResolution::Ambiguous,
        })
    }

    /// Names a child id below one parent id.
    pub(crate) fn name_of_scoped(&self, kind: &str, id: &str, parent_id: &str) -> Option<&str> {
        let Some(Ok(entries)) = self.kinds.get(kind) else {
            return None;
        };
        entries
            .iter()
            .find(|entry| entry.id == id && entry.parent_id.as_deref() == Some(parent_id))
            .map(|entry| entry.name.as_str())
    }
}

fn entries(
    spec: &dokploy_spec::KindSpec,
    key: &str,
    items: &[Json],
    parent_id: Option<&str>,
) -> Result<Vec<Entry>, RemoteFailureKind> {
    let mut entries = Vec::new();
    let mut ids = BTreeSet::new();
    for item in items {
        let (Some(id), Some(Json::String(name))) =
            (item_id(spec, item), field_value(spec, key, item))
        else {
            continue;
        };
        // A collection that lists one identity twice contradicts itself.
        if !ids.insert(id.to_owned()) {
            return Err(RemoteFailureKind::InvalidResponse);
        }
        entries.push(Entry {
            id: id.to_owned(),
            name,
            parent_id: parent_id.map(str::to_owned),
        });
    }

    Ok(entries)
}
