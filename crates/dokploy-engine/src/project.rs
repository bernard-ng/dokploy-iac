//! Tolerant projection of a raw Dokploy response onto a kind's properties (ADR 0007).
//!
//! Reading is presence-aware: a field Dokploy did not return is *not returned*, which is
//! different from `null`, which is a value. Unknown response fields are ignored. Secret and
//! write-only values are dropped at this boundary and never retained.

use std::collections::BTreeMap;

use dokploy_core::{ComparableValue, PropertyObservation, PropertyPath, PropertyUnknownReason};
use dokploy_spec::{Field, FieldType, KindSpec, PathShape, parse_type};
use serde_json::Value as Json;

use crate::canonical::canonical_remote;
use crate::envtext::EnvText;
use crate::selectors::SelectorIndex;

/// The response key (or JSON pointer) a field is read from.
pub(crate) fn api_name<'a>(name: &'a str, field: &'a Field) -> &'a str {
    field.api.as_deref().unwrap_or(name)
}

fn lookup<'v>(item: &'v Json, pointer: &str) -> Option<&'v Json> {
    if pointer.starts_with('/') {
        item.pointer(pointer)
    } else {
        item.get(pointer)
    }
}

/// The identity of a response item, read from the kind's id field.
pub(crate) fn item_id<'v>(spec: &KindSpec, item: &'v Json) -> Option<&'v str> {
    item.get(&spec.api.id)
        .and_then(Json::as_str)
        .filter(|id| !id.trim().is_empty())
}

/// The canonical value of one field of a response item, when it is readable.
pub(crate) fn field_value(spec: &KindSpec, name: &str, item: &Json) -> Option<Json> {
    let field = spec.fields.get(name)?;
    let ty = parse_type(&field.ty).ok()?;
    canonical_remote(lookup(item, api_name(name, field))?, &ty)
}

/// Projects an item onto every property of the kind, and onto the entries of keyed
/// collections the document or state names (`entries`): a collection's entries are named by
/// whoever owns them, so they are not in the spec.
pub(crate) fn project(
    spec: &KindSpec,
    item: &Json,
    selectors: &SelectorIndex,
    entries: &[PropertyPath],
) -> BTreeMap<PropertyPath, PropertyObservation> {
    let mut observed = BTreeMap::new();
    // A collection is observed by its owned entries or as a whole, never both.
    let entered: Vec<&str> = entries
        .iter()
        .filter_map(|path| path.info().root.as_deref())
        .collect();
    for info in spec.properties() {
        if info.shape == PathShape::CollectionRoot && entered.contains(&info.path.as_str()) {
            continue;
        }
        let observation = observe(&info, item, selectors);
        observed.insert(PropertyPath::from_property_info(info), observation);
    }
    for path in entries {
        observed.insert(path.clone(), observe_entry(path.info(), item, selectors));
    }

    observed
}

fn observe(
    info: &dokploy_spec::PropertyInfo,
    item: &Json,
    selectors: &SelectorIndex,
) -> PropertyObservation {
    if info.is_sensitive() {
        return PropertyObservation::Unknown(PropertyUnknownReason::Sensitive);
    }
    let not_returned = PropertyObservation::Unknown(PropertyUnknownReason::NotReturned);
    match info.shape {
        PathShape::CollectionRoot if matches!(info.ty, FieldType::Env) => {
            return observe_environment(info, item);
        }
        PathShape::CollectionRoot if is_selector_set(&info.ty) => {
            return observe_selector_set(info, item, selectors);
        }
        PathShape::CollectionRoot | PathShape::CollectionEntry => return not_returned,
        PathShape::Atomic => {}
    }
    if info.union_tag {
        return observe_tag(info, item);
    }
    // A member of an arm exists only while Dokploy holds that arm.
    if let (Some(arm), Some(tag_api)) = (info.arm.as_deref(), info.tag_api.as_deref()) {
        match lookup(item, tag_api) {
            None => return not_returned,
            Some(Json::String(held)) if held == arm => {}
            Some(_) => return PropertyObservation::KnownAbsent,
        }
    }
    if let Some(target) = info.selector.as_deref() {
        return observe_selector(target, lookup(item, &info.api), selectors);
    }
    // Structs without per-member planning, unions, and files are not read back yet; a desired
    // property of those kinds stays unknown, which blocks planning for it alone.
    if !matches!(
        info.ty,
        FieldType::Text
            | FieldType::Ref(_)
            | FieldType::Enum(_)
            | FieldType::Int
            | FieldType::Number
            | FieldType::Bool
            | FieldType::List(_)
            | FieldType::Set(_)
            | FieldType::Map(_)
            | FieldType::Blob(_)
    ) {
        return not_returned;
    }

    match lookup(item, &info.api) {
        None => not_returned,
        Some(Json::Null) => PropertyObservation::KnownAbsent,
        Some(value) => canonical_remote(value, &info.ty)
            .and_then(|canonical| ComparableValue::try_from_json(canonical).ok())
            .map_or(
                PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
                PropertyObservation::Known,
            ),
    }
}

/// The arm of a union Dokploy holds: one of the arms the spec names.
fn observe_tag(info: &dokploy_spec::PropertyInfo, item: &Json) -> PropertyObservation {
    match lookup(item, &info.api) {
        None => PropertyObservation::Unknown(PropertyUnknownReason::NotReturned),
        Some(Json::String(held)) => canonical_remote(&Json::String(held.clone()), &info.ty)
            .and_then(|canonical| ComparableValue::try_from_json(canonical).ok())
            .map_or(
                PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
                PropertyObservation::Known,
            ),
        Some(_) => PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
    }
}

/// An environment block read as a whole: no text is nothing at all, an empty text is an empty
/// block, and otherwise the names of the variables with nothing else about them.
fn observe_environment(info: &dokploy_spec::PropertyInfo, item: &Json) -> PropertyObservation {
    match lookup(item, &info.api) {
        None => PropertyObservation::Unknown(PropertyUnknownReason::NotReturned),
        Some(Json::Null) => PropertyObservation::KnownAbsent,
        Some(Json::String(text)) => {
            let names: serde_json::Map<String, Json> = EnvText::parse(text)
                .keys()
                .map(|key| (key.to_owned(), Json::Bool(true)))
                .collect();
            ComparableValue::try_from_json(Json::Object(names)).map_or(
                PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
                PropertyObservation::Known,
            )
        }
        Some(_) => PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
    }
}

/// The ids Dokploy holds for a set of selectors.
enum Held {
    NotReturned,
    Null,
    Ids(Vec<String>),
    Invalid,
}

/// Reads the ids a set of selectors holds: an array of ids under a key, or, for a relation, the
/// ids inside the objects of an array (`/projectTags/*/tagId`). An element that does not hold an
/// id is not guessed at: the whole read is invalid.
fn held_ids(item: &Json, api: &str) -> Held {
    let ids = |items: &[Json], member: Option<&str>| -> Held {
        let mut ids = Vec::new();
        for element in items {
            let id = match member {
                Some(member) => element.get(member).and_then(Json::as_str),
                None => element.as_str(),
            };
            match id {
                Some(id) => ids.push(id.to_owned()),
                None => return Held::Invalid,
            }
        }

        Held::Ids(ids)
    };
    let (base, member) = match api.split_once("/*/") {
        Some((base, member)) => (base, Some(member)),
        None => (api, None),
    };
    match lookup(item, base) {
        None => Held::NotReturned,
        Some(Json::Null) => Held::Null,
        Some(Json::Array(items)) => ids(items, member),
        Some(_) => Held::Invalid,
    }
}

/// The ids a set of selectors holds in `item`, when the read says so.
pub(crate) fn held_member_ids(item: &Json, api: &str) -> Option<Vec<String>> {
    match held_ids(item, api) {
        Held::Ids(ids) => Some(ids),
        Held::Null => Some(Vec::new()),
        Held::NotReturned | Held::Invalid => None,
    }
}

/// Whether a type is a set of selectors, planned per member.
fn is_selector_set(ty: &FieldType) -> bool {
    matches!(ty, FieldType::Set(item) if matches!(**item, FieldType::Selector(_)))
}

/// The kind a set of selectors selects.
fn selector_set_kind(ty: &FieldType) -> Option<&str> {
    match ty {
        FieldType::Set(item) => match &**item {
            FieldType::Selector(kind) => Some(kind),
            _ => None,
        },
        _ => None,
    }
}

/// A set of selectors as a whole: the names of the resources Dokploy holds the ids of. An id the
/// index cannot name is kept under its own id, so a set that holds something unknown is never
/// taken for an empty one.
fn observe_selector_set(
    info: &dokploy_spec::PropertyInfo,
    item: &Json,
    selectors: &SelectorIndex,
) -> PropertyObservation {
    let not_returned = PropertyObservation::Unknown(PropertyUnknownReason::NotReturned);
    let Some(kind) = selector_set_kind(&info.ty) else {
        return not_returned;
    };
    match held_ids(item, &info.api) {
        Held::NotReturned => not_returned,
        Held::Null => PropertyObservation::KnownAbsent,
        Held::Ids(ids) => {
            let mut names = serde_json::Map::new();
            for id in &ids {
                let key = selectors
                    .name_of(kind, id)
                    .map_or_else(|| format!("?{id}"), str::to_owned);
                names.insert(key, Json::Bool(true));
            }
            ComparableValue::try_from_json(Json::Object(names)).map_or(
                PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
                PropertyObservation::Known,
            )
        }
        Held::Invalid => PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
    }
}

/// One member of a set of selectors: there, or not, by the name of the resource each id Dokploy
/// holds belongs to. An id that cannot be named might be this member, so it is not an absence.
fn observe_selector_entry(
    info: &dokploy_spec::PropertyInfo,
    item: &Json,
    selectors: &SelectorIndex,
    kind: &str,
) -> PropertyObservation {
    let not_returned = PropertyObservation::Unknown(PropertyUnknownReason::NotReturned);
    let name = info
        .root
        .as_deref()
        .and_then(|root| info.path.strip_prefix(root))
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or_default();
    match held_ids(item, &info.api) {
        Held::NotReturned => not_returned,
        Held::Null => PropertyObservation::KnownAbsent,
        Held::Ids(ids) => {
            let mut unnamed = false;
            for id in &ids {
                match selectors.name_of(kind, id) {
                    Some(held) if held == name => {
                        return ComparableValue::try_from_json(serde_json::json!({ "name": name }))
                            .map_or(
                                PropertyObservation::Unknown(
                                    PropertyUnknownReason::InvalidResponse,
                                ),
                                PropertyObservation::Known,
                            );
                    }
                    Some(_) => {}
                    None => unnamed = true,
                }
            }
            if unnamed {
                not_returned
            } else {
                PropertyObservation::KnownAbsent
            }
        }
        Held::Invalid => PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
    }
}

/// One variable of an environment block: there, or not. Its value is secret and never read.
fn observe_entry(
    info: &dokploy_spec::PropertyInfo,
    item: &Json,
    selectors: &SelectorIndex,
) -> PropertyObservation {
    if let Some(kind) = info.selector.as_deref() {
        return observe_selector_entry(info, item, selectors, kind);
    }
    // The entry is named below the path of its block, which may itself be a struct member.
    let key = info
        .root
        .as_deref()
        .and_then(|root| info.path.strip_prefix(root))
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or_default();
    match lookup(item, &info.api) {
        None => PropertyObservation::Unknown(PropertyUnknownReason::NotReturned),
        Some(Json::Null) => PropertyObservation::KnownAbsent,
        Some(Json::String(text)) if EnvText::parse(text).contains(key) => {
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)
        }
        Some(Json::String(_)) => PropertyObservation::KnownAbsent,
        Some(_) => PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
    }
}

/// A selector as the document writes it: the name of the resource Dokploy holds the id of. No
/// id means no resource, which for a server is the Dokploy host itself.
fn observe_selector(
    target: &str,
    held: Option<&Json>,
    selectors: &SelectorIndex,
) -> PropertyObservation {
    let not_returned = PropertyObservation::Unknown(PropertyUnknownReason::NotReturned);
    let comparable = |value: Json| {
        ComparableValue::try_from_json(value).map_or(
            PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
            PropertyObservation::Known,
        )
    };
    match held {
        None => not_returned,
        Some(Json::Null) if target == "server" => comparable(serde_json::json!({ "local": true })),
        Some(Json::Null) => PropertyObservation::KnownAbsent,
        Some(Json::String(id)) => selectors.name_of(target, id).map_or(not_returned, |name| {
            comparable(serde_json::json!({ "name": name }))
        }),
        Some(_) => PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
    }
}
