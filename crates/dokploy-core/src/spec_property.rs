//! Spec-driven behavior for [`PropertyPath`]: value validation, stored-state projection,
//! checkpoint materialization, and the mutation contract.
//!
//! Everything here is decided by the facts a kind spec attached to the path
//! ([`PropertyInfo`]); nothing names a kind (ADR 0002).

use std::collections::{BTreeMap, BTreeSet};

use dokploy_spec::{
    FieldType, KindSpec, Mutability, PathError, PathShape, PropertyInfo, ValueRules,
};
use dokploy_state::{SensitiveFingerprint, SensitivePropertyPath};
use serde_json::Value;

use crate::mutation::{MutationContract, MutationMode, PropertyMutation, ReplacementOrder};
use crate::plan::CheckpointMaterializationError;
use crate::property::{ComparableValue, OwnedValue, PropertyPath};
use crate::snapshot::{PropertyObservation, PropertyUnknownReason, StoredStateError};
use dokploy_state::ResourceAddress;

/// Whether `value` is an acceptable owned value at a spec-resolved path.
pub(crate) fn owned_value_valid(
    path: &PropertyPath,
    info: &PropertyInfo,
    value: &OwnedValue,
) -> bool {
    match value {
        OwnedValue::Null => info.nullable,
        OwnedValue::EmptyCollection => info.shape == PathShape::CollectionRoot,
        // A sensitive property is owned only through its receipt, and nothing else is.
        OwnedValue::Sensitive(_) => info.is_sensitive() && info.shape != PathShape::CollectionRoot,
        OwnedValue::Value(value) => {
            if info.is_sensitive() || info.shape == PathShape::CollectionRoot {
                return false;
            }
            if info.is_selector() {
                return selector_valid(path, value.as_json());
            }
            json_matches(value.as_json(), &info.ty, &info.rules)
        }
    }
}

/// Whether a remote observation is acceptable at a spec-resolved path.
pub(crate) fn observation_valid(
    path: &PropertyPath,
    info: &PropertyInfo,
    observation: &PropertyObservation,
) -> bool {
    if info.is_sensitive() {
        return matches!(
            observation,
            PropertyObservation::KnownAbsent | PropertyObservation::Unknown(_)
        );
    }
    match observation {
        PropertyObservation::Unknown(reason) => *reason != PropertyUnknownReason::Sensitive,
        PropertyObservation::KnownAbsent => info.nullable,
        PropertyObservation::Known(value) => {
            if info.shape == PathShape::CollectionRoot {
                value.as_json().is_object()
            } else if info.is_selector() {
                selector_valid(path, value.as_json())
            } else {
                json_matches(value.as_json(), &info.ty, &info.rules)
            }
        }
    }
}

fn selector_valid(path: &PropertyPath, value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.len() != 1 {
        return false;
    }
    match (object.get("local"), object.get("name")) {
        (Some(local), None) => path.accepts_local_selector() && local == &Value::Bool(true),
        (None, Some(name)) => name.as_str().is_some_and(|name| {
            !name.is_empty()
                && name.len() <= 256
                && name.trim() == name
                && !name.chars().any(char::is_control)
        }),
        _ => false,
    }
}

/// Whether a JSON value has the shape a spec type describes.
///
/// Patterns are not checked here; the document model enforces them when it parses.
fn json_matches(value: &Value, ty: &FieldType, rules: &ValueRules) -> bool {
    match ty {
        FieldType::Text => value.as_str().is_some_and(|text| {
            rules
                .min_len
                .is_none_or(|min| text.chars().count() as u64 >= min)
        }),
        FieldType::Int => {
            let Some(number) = value.as_i64() else {
                return value.as_u64().is_some_and(|n| in_range(n as f64, rules));
            };
            in_range(number as f64, rules)
        }
        FieldType::Number => value.as_f64().is_some_and(|n| in_range(n, rules)),
        FieldType::Bool => value.is_boolean(),
        FieldType::Enum(allowed) => value
            .as_str()
            .is_some_and(|text| allowed.iter().any(|candidate| candidate == text)),
        FieldType::List(item) => value.as_array().is_some_and(|items| {
            items
                .iter()
                .all(|v| json_matches(v, item, &ValueRules::default()))
        }),
        FieldType::Set(item) => value.as_array().is_some_and(|items| {
            let distinct: BTreeSet<String> = items.iter().map(Value::to_string).collect();
            distinct.len() == items.len()
                && items
                    .iter()
                    .all(|v| json_matches(v, item, &ValueRules::default()))
        }),
        FieldType::Map(item) => value.as_object().is_some_and(|map| {
            map.values()
                .all(|v| json_matches(v, item, &ValueRules::default()))
        }),
        FieldType::Env | FieldType::Struct | FieldType::Blob(_) | FieldType::Shared(_) => {
            value.is_object() || matches!(ty, FieldType::Blob(_) | FieldType::Shared(_))
        }
        FieldType::Union { tag } => value
            .as_object()
            .is_some_and(|object| object.get(tag).is_some_and(Value::is_string)),
        FieldType::Ref(_) | FieldType::File => value.is_string(),
        FieldType::Selector(_) => value.is_object(),
    }
}

fn in_range(number: f64, rules: &ValueRules) -> bool {
    rules.min.is_none_or(|min| number >= min) && rules.max.is_none_or(|max| number <= max)
}

/// Projects durable managed inputs and sensitive receipts into planner properties
/// by the kind's spec.
pub(crate) fn project_stored_inputs(
    address: &ResourceAddress,
    spec: &KindSpec,
    inputs: &Value,
) -> Result<BTreeMap<PropertyPath, OwnedValue>, StoredStateError> {
    let object = inputs
        .as_object()
        .expect("ManagedInputs guarantees an object root");
    let mut properties = BTreeMap::new();
    for (name, value) in object {
        project_value(address, spec, name, value, &mut properties)?;
    }
    Ok(properties)
}

fn project_value(
    address: &ResourceAddress,
    spec: &KindSpec,
    path: &str,
    value: &Value,
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
) -> Result<(), StoredStateError> {
    let unsupported = || StoredStateError::UnsupportedProperty {
        address: address.clone(),
    };
    let invalid_value = || StoredStateError::InvalidPropertyValue {
        address: address.clone(),
    };
    let info = match spec.property(path) {
        Ok(info) => info,
        Err(PathError::NeedsMember { .. }) => {
            let Some(members) = value.as_object().filter(|members| !members.is_empty()) else {
                return Err(invalid_value());
            };
            for (member, member_value) in members {
                project_value(
                    address,
                    spec,
                    &format!("{path}.{member}"),
                    member_value,
                    properties,
                )?;
            }
            return Ok(());
        }
        Err(_) => return Err(unsupported()),
    };

    if info.shape == PathShape::CollectionRoot {
        return match value {
            Value::Null => insert(address, properties, info, OwnedValue::Null),
            Value::Object(entries) if entries.is_empty() => {
                insert(address, properties, info, OwnedValue::EmptyCollection)
            }
            Value::Object(entries) => {
                for (key, entry_value) in entries {
                    let entry = spec.property(&format!("{path}.{key}")).map_err(|_| {
                        StoredStateError::InvalidPropertyPath {
                            address: address.clone(),
                        }
                    })?;
                    let owned = scalar(&entry, entry_value).ok_or_else(invalid_value)?;
                    insert(address, properties, entry, owned)?;
                }
                Ok(())
            }
            _ => Err(invalid_value()),
        };
    }
    let owned = scalar(&info, value).ok_or_else(invalid_value)?;
    insert(address, properties, info, owned)
}

/// A stored scalar: `null` is an owned clear, and a sensitive property is never a value.
fn scalar(info: &PropertyInfo, value: &Value) -> Option<OwnedValue> {
    if value.is_null() {
        return Some(OwnedValue::Null);
    }
    if info.is_sensitive() {
        return None;
    }
    ComparableValue::try_from_json(value.clone())
        .ok()
        .map(OwnedValue::Value)
}

fn insert(
    address: &ResourceAddress,
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    info: PropertyInfo,
    value: OwnedValue,
) -> Result<(), StoredStateError> {
    let path = PropertyPath::from_property_info(info);
    if properties.insert(path, value).is_some() {
        return Err(StoredStateError::ConflictingPropertyPaths {
            address: address.clone(),
        });
    }
    Ok(())
}

/// Resolves a stored sensitive receipt's path through the spec.
pub(crate) fn sensitive_path(
    address: &ResourceAddress,
    spec: &KindSpec,
    path: &str,
) -> Result<PropertyPath, StoredStateError> {
    let info = spec
        .property(path)
        .map_err(|_| StoredStateError::UnsupportedProperty {
            address: address.clone(),
        })?;
    if !info.is_sensitive() || info.shape == PathShape::CollectionRoot {
        return Err(StoredStateError::InvalidPropertyPath {
            address: address.clone(),
        });
    }
    Ok(PropertyPath::from_property_info(info))
}

/// Writes one spec-resolved property into the durable managed inputs or receipts.
pub(crate) fn materialize(
    info: &PropertyInfo,
    value: &OwnedValue,
    managed: &mut serde_json::Map<String, Value>,
    sensitive: &mut Vec<(SensitivePropertyPath, SensitiveFingerprint)>,
) -> Result<(), CheckpointMaterializationError> {
    let invalid = || CheckpointMaterializationError::InvalidPropertyShape;
    let stored = match value {
        OwnedValue::Null => Value::Null,
        OwnedValue::EmptyCollection => Value::Object(serde_json::Map::new()),
        OwnedValue::Value(value) => value.as_json().clone(),
        OwnedValue::Sensitive(intent) => {
            let path = SensitivePropertyPath::parse(&info.path).map_err(|_| invalid())?;
            sensitive.push((path, intent.fingerprint().clone()));
            return Ok(());
        }
    };
    let segments: Vec<&str> = match (&info.shape, &info.root) {
        (PathShape::CollectionEntry, Some(root)) => {
            vec![root.as_str(), &info.path[root.len() + 1..]]
        }
        _ => info.path.split('.').collect(),
    };
    insert_nested(managed, &segments, stored).ok_or_else(invalid)
}

fn insert_nested(
    map: &mut serde_json::Map<String, Value>,
    segments: &[&str],
    value: Value,
) -> Option<()> {
    let (first, rest) = segments.split_first()?;
    if rest.is_empty() {
        return map
            .insert((*first).to_owned(), value)
            .is_none()
            .then_some(());
    }
    let child = map
        .entry((*first).to_owned())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    insert_nested(child.as_object_mut()?, rest, value)
}

/// Registers every kind of a spec registry with the state layer, parents first.
///
/// State and journals can only name kinds registered here. Registering is idempotent, and a
/// spec that contradicts a kind already registered is an error.
pub fn register_spec_kinds(
    specs: &dokploy_spec::SpecRegistry,
) -> Result<(), dokploy_state::KindRegistrationError> {
    use dokploy_spec::Scope;
    use dokploy_state::{ResourceKind, StateScope};

    let mut registered: BTreeMap<String, ResourceKind> = BTreeMap::new();
    let mut pending: Vec<&KindSpec> = specs.kinds().collect();
    while !pending.is_empty() {
        let before = pending.len();
        let mut waiting = Vec::new();
        for spec in pending {
            let parent = match &spec.parent {
                None => None,
                Some(parent) => match registered.get(parent) {
                    Some(kind) => Some(*kind),
                    None => {
                        waiting.push(spec);
                        continue;
                    }
                },
            };
            let scope = match spec.scope {
                Scope::Project => StateScope::Project,
                Scope::Settings => StateScope::Settings,
            };
            let kind = ResourceKind::register(&spec.kind, scope, parent)?;
            registered.insert(spec.kind.clone(), kind);
        }
        // A registry is validated, so parents exist and chains end; this guards a loop.
        if waiting.len() == before {
            break;
        }
        pending = waiting;
    }

    Ok(())
}

impl MutationContract {
    /// Builds the contract a kind spec implies (ADR 0003).
    ///
    /// `in_place` and `write_only` properties change in place; `create_only` ones
    /// force a replacement; `computed` ones are never configurable. A property can be
    /// cleared only when the spec makes it nullable. The spec does not yet state a
    /// replacement order, so replacement deletes before it creates, the safe order
    /// for name-keyed objects that cannot coexist.
    #[must_use]
    pub fn from_spec(spec: &KindSpec) -> Self {
        let mut contract = Self::deny_all(ReplacementOrder::DeleteBeforeCreate);
        for info in spec.properties() {
            let clearable = info.nullable;
            let mutation = match info.mutability {
                Mutability::InPlace | Mutability::WriteOnly => PropertyMutation::new(
                    MutationMode::InPlace,
                    unsupported_unless(clearable, MutationMode::InPlace),
                ),
                Mutability::CreateOnly => PropertyMutation::new(
                    MutationMode::Replace,
                    unsupported_unless(clearable, MutationMode::Replace),
                ),
                // A resource moves between parents by changing its address (a move), never by
                // writing a property.
                Mutability::Reparent | Mutability::Computed => {
                    PropertyMutation::new(MutationMode::Unsupported, MutationMode::Unsupported)
                }
            };
            let required = info.is_required_on_create();
            let creatable = info.mutability != Mutability::Computed;
            let path = PropertyPath::from_property_info(info);
            contract = contract.with_property(path.clone(), mutation);
            if required {
                contract = contract.requiring(path);
            } else if creatable {
                contract = contract.allowing_on_create(path);
            }
        }
        contract
    }
}

const fn unsupported_unless(allowed: bool, mode: MutationMode) -> MutationMode {
    if allowed {
        mode
    } else {
        MutationMode::Unsupported
    }
}
