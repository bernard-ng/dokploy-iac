//! One canonical JSON form for a value, so what the document says and what Dokploy
//! returns compare equal exactly when they mean the same (ADR 0004, "Comparison").

use dokploy_model::{Selector, Value};
use dokploy_spec::FieldType;
use serde_json::Value as Json;

/// Why a document value has no canonical comparable form yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NotComparable {
    /// The value holds a secret, an environment block, or a file source, which are
    /// fingerprinted per property instead of compared.
    HoldsSecret,
}

/// The canonical JSON of a document value of type `ty`.
pub(crate) fn canonical(value: &Value, ty: &FieldType) -> Result<Json, NotComparable> {
    Ok(match value {
        Value::Null => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Int(number) => Json::from(*number),
        Value::Number(number) => number_json(*number),
        Value::Text(text) => Json::String(text.clone()),
        Value::List(items) => {
            let item_type = match ty {
                FieldType::List(item) | FieldType::Set(item) => item.as_ref(),
                _ => ty,
            };
            let mut values = items
                .iter()
                .map(|item| canonical(item, item_type))
                .collect::<Result<Vec<_>, _>>()?;
            if matches!(ty, FieldType::Set(_)) {
                sort_set(&mut values);
            }
            Json::Array(values)
        }
        Value::Map(entries) => {
            let item_type = match ty {
                FieldType::Map(item) => item.as_ref(),
                _ => ty,
            };
            Json::Object(
                entries
                    .iter()
                    .map(|(name, entry)| Ok((name.clone(), canonical(entry, item_type)?)))
                    .collect::<Result<_, NotComparable>>()?,
            )
        }
        Value::Union { tag, fields } => {
            let tag_name = match ty {
                FieldType::Union { tag } => tag.as_str(),
                _ => "type",
            };
            let mut object = serde_json::Map::new();
            object.insert(tag_name.to_owned(), Json::String(tag.clone()));
            for (name, field) in fields {
                object.insert(name.clone(), canonical(field, ty)?);
            }
            Json::Object(object)
        }
        Value::Selector(Selector::Name(name)) => serde_json::json!({ "name": name }),
        Value::Selector(Selector::Local) => serde_json::json!({ "local": true }),
        Value::Blob(json) => json.clone(),
        Value::Env(_) | Value::Source(_) => return Err(NotComparable::HoldsSecret),
    })
}

/// Canonical JSON for a number of type `number`: always a float, so `1` and `1.0` agree.
pub(crate) fn number_json(number: f64) -> Json {
    serde_json::Number::from_f64(number).map_or(Json::Null, Json::Number)
}

/// A set compares as its elements in a fixed order.
pub(crate) fn sort_set(values: &mut [Json]) {
    values.sort_by_key(ToString::to_string);
}

/// Canonicalises a value returned by Dokploy for a property of type `ty`, or `None` when the
/// value does not have that type.
pub(crate) fn canonical_remote(value: &Json, ty: &FieldType) -> Option<Json> {
    match ty {
        FieldType::Text | FieldType::Ref(_) => value.is_string().then(|| value.clone()),
        FieldType::Enum(allowed) => value
            .as_str()
            .filter(|text| allowed.iter().any(|candidate| candidate == text))
            .map(|_| value.clone()),
        FieldType::Int => (value.is_i64() || value.is_u64()).then(|| value.clone()),
        FieldType::Number => value.as_f64().map(number_json),
        FieldType::Bool => value.is_boolean().then(|| value.clone()),
        FieldType::List(item) => value
            .as_array()?
            .iter()
            .map(|element| canonical_remote(element, item))
            .collect::<Option<Vec<_>>>()
            .map(Json::Array),
        FieldType::Set(item) => {
            let mut values = value
                .as_array()?
                .iter()
                .map(|element| canonical_remote(element, item))
                .collect::<Option<Vec<_>>>()?;
            sort_set(&mut values);
            Some(Json::Array(values))
        }
        FieldType::Map(item) => value
            .as_object()?
            .iter()
            .map(|(name, entry)| Some((name.clone(), canonical_remote(entry, item)?)))
            .collect::<Option<serde_json::Map<_, _>>>()
            .map(Json::Object),
        FieldType::Blob(_) => Some(value.clone()),
        FieldType::Struct
        | FieldType::Union { .. }
        | FieldType::Env
        | FieldType::File
        | FieldType::Selector(_)
        | FieldType::Shared(_) => None,
    }
}
