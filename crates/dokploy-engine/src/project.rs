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

/// Projects an item onto every property of the kind.
pub(crate) fn project(spec: &KindSpec, item: &Json) -> BTreeMap<PropertyPath, PropertyObservation> {
    let mut observed = BTreeMap::new();
    for info in spec.properties() {
        let observation = observe(spec, &info.path, &info, item);
        observed.insert(PropertyPath::from_property_info(info), observation);
    }

    observed
}

fn observe(
    spec: &KindSpec,
    path: &str,
    info: &dokploy_spec::PropertyInfo,
    item: &Json,
) -> PropertyObservation {
    if info.is_sensitive() {
        return PropertyObservation::Unknown(PropertyUnknownReason::Sensitive);
    }
    // Collections, struct members, selectors, and unions are not read back yet; a desired
    // property of those kinds stays unknown, which blocks planning for it alone.
    let not_returned = PropertyObservation::Unknown(PropertyUnknownReason::NotReturned);
    let Some(field) = spec.fields.get(path) else {
        return not_returned;
    };
    if info.shape != PathShape::Atomic || info.is_selector() {
        return not_returned;
    }
    let Ok(ty) = parse_type(&field.ty) else {
        return not_returned;
    };
    if !matches!(
        ty,
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

    match lookup(item, api_name(path, field)) {
        None => not_returned,
        Some(Json::Null) => PropertyObservation::KnownAbsent,
        Some(value) => canonical_remote(value, &ty)
            .and_then(|canonical| ComparableValue::try_from_json(canonical).ok())
            .map_or(
                PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
                PropertyObservation::Known,
            ),
    }
}
