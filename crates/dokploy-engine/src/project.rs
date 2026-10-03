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
        observed.insert(path.clone(), observe_entry(path.info(), item));
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

/// One variable of an environment block: there, or not. Its value is secret and never read.
fn observe_entry(info: &dokploy_spec::PropertyInfo, item: &Json) -> PropertyObservation {
    let key = info.path.split_once('.').map_or("", |(_, key)| key);
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
