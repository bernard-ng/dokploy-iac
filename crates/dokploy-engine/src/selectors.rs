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
            .filter_map(|info| {
                // A set of selectors names its target on the element type of its root.
                info.selector.or_else(|| match info.ty {
                    dokploy_spec::FieldType::Set(item) => match *item {
                        dokploy_spec::FieldType::Selector(kind) => Some(kind),
                        _ => None,
                    },
                    _ => None,
                })
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
            if !spec.parents.is_empty() || spec.api.read.list.is_none() {
                continue;
            }
            let entries = read_collection(transport, specs, spec, None)
                .await
                .and_then(|items| entries(spec, key, &items));
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

    /// What a selector value means now: the id of the one resource with that name, `Local`,
    /// or why it cannot be resolved. `None` when the kind was not read.
    pub(crate) fn resolve(&self, kind: &str, selector: &Json) -> Option<ExternalResolution> {
        if selector.get("local").and_then(Json::as_bool) == Some(true) {
            return (kind == LOCAL_KIND).then_some(ExternalResolution::Local);
        }
        let name = selector.get("name")?.as_str()?;
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
}

fn entries(
    spec: &dokploy_spec::KindSpec,
    key: &str,
    items: &[Json],
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
        });
    }

    Ok(entries)
}
