//! Building the requests of a mutation from a spec, a checkpoint, and the document's
//! secrets. Everything here is pure: nothing is sent, so a plan can be checked for what the
//! engine can do before the first byte goes out (the preflight).

use std::collections::BTreeMap;

use dokploy_api::{BodyShape, request_contract};
use dokploy_core::{CheckpointValueRef, PropertyPath, ResourceCheckpoint};
use dokploy_sdk::OperationRequest;
use dokploy_spec::{
    Field, FieldType, KindSpec, Mutability, PathShape, Shape, WriteGroup, parse_type,
};
use dokploy_state::ResourceAddress;
use serde_json::{Map, Value as Json};

use super::ApplyError;
use crate::compile::Compiled;

/// A property the executor can write: an atomic top-level value.
pub(crate) struct Writable<'s> {
    /// The document's field name (equal to the dotted path of an atomic property).
    pub(crate) name: &'s str,
    pub(crate) field: &'s Field,
    /// The body key.
    pub(crate) wire: &'s str,
}

/// Resolves a property path to something the executor can write, or says why it cannot.
pub(crate) fn writable<'s>(
    spec: &'s KindSpec,
    address: &ResourceAddress,
    path: &PropertyPath,
) -> Result<Writable<'s>, ApplyError> {
    let unsupported = |reason: &'static str| ApplyError::Unsupported {
        address: address.clone(),
        property: Some(path.to_string()),
        reason,
    };
    let info = path
        .spec_info()
        .ok_or_else(|| unsupported("is not a property of a spec kind"))?;
    if info.shape != PathShape::Atomic {
        return Err(unsupported(
            "is a keyed collection, which the executor does not write yet",
        ));
    }
    let (name, field) = spec
        .fields
        .get_key_value(info.path.as_str())
        .ok_or_else(|| unsupported("is not a field of the kind"))?;
    let ty =
        parse_type(&field.ty).map_err(|_| unsupported("has a type the executor cannot read"))?;
    if matches!(
        ty,
        FieldType::Union { .. } | FieldType::Env | FieldType::File | FieldType::Struct
    ) {
        return Err(unsupported(
            "is a composite value, which the executor does not write yet",
        ));
    }
    if info.is_selector() {
        return Err(unsupported(
            "is a selector, which the executor does not resolve yet",
        ));
    }
    let wire = field.request_name(name);
    if field
        .api
        .as_deref()
        .is_some_and(|api| api.trim_start_matches('/').contains('/'))
    {
        return Err(unsupported(
            "maps to a nested request key, which the executor does not write yet",
        ));
    }

    Ok(Writable { name, field, wire })
}

/// The JSON a checkpoint property is sent as: a value, `null`, or a secret read from the
/// document's source.
fn body_value(
    compiled: &Compiled,
    address: &ResourceAddress,
    checkpoint: &ResourceCheckpoint,
    path: &PropertyPath,
) -> Result<Json, ApplyError> {
    match checkpoint.property(path) {
        Some(CheckpointValueRef::Null) => Ok(Json::Null),
        Some(CheckpointValueRef::NonSensitive(value)) => Ok(value.clone()),
        Some(CheckpointValueRef::Sensitive) => secret_text(compiled, address, &path.to_string()),
        Some(CheckpointValueRef::EmptyCollection) | None => Err(ApplyError::Unsupported {
            address: address.clone(),
            property: Some(path.to_string()),
            reason: "has no value to send",
        }),
    }
}

fn secret_text(
    compiled: &Compiled,
    address: &ResourceAddress,
    dotted: &str,
) -> Result<Json, ApplyError> {
    let bytes = compiled
        .secrets
        .get(&(address.clone(), dotted.to_owned()))
        .ok_or_else(|| ApplyError::MissingSecret {
            address: address.clone(),
            property: dotted.to_owned(),
        })?;
    std::str::from_utf8(bytes)
        .map(|text| Json::String(text.to_owned()))
        .map_err(|_| ApplyError::SecretNotText {
            address: address.clone(),
            property: dotted.to_owned(),
        })
}

/// The create request: every managed property the create operation accepts, the attachment to
/// the parent, and `null` for a field the contract requires that the document leaves out and
/// the spec allows to be null.
///
/// A managed property the create operation does not accept would need a follow-up update;
/// that is refused for now (it arrives with the project kinds).
pub(crate) fn create_request(
    spec: &KindSpec,
    address: &ResourceAddress,
    checkpoint: &ResourceCheckpoint,
    parent_id: Option<&str>,
    compiled: &Compiled,
) -> Result<OperationRequest, ApplyError> {
    let operation = spec
        .api
        .create
        .as_ref()
        .ok_or_else(|| ApplyError::Unsupported {
            address: address.clone(),
            property: None,
            reason: "has no create operation",
        })?;
    let contract = request_contract(&operation.op);
    let mut body = Map::new();

    for path in checkpoint.property_paths() {
        let target = writable(spec, address, path)?;
        if target.field.mutability == Mutability::Computed {
            continue;
        }
        if let Some(contract) = contract
            && matches!(contract.body(), BodyShape::Object(_))
            && contract.body_field(target.wire).is_none()
        {
            return Err(ApplyError::Unsupported {
                address: address.clone(),
                property: Some(path.to_string()),
                reason: "is not accepted by the create operation and would need a follow-up update",
            });
        }
        body.insert(
            target.wire.to_owned(),
            body_value(compiled, address, checkpoint, path)?,
        );
    }

    for (field, source) in &operation.attach {
        if source == "parent_id" {
            let id = parent_id.ok_or_else(|| ApplyError::Unsupported {
                address: address.clone(),
                property: None,
                reason: "needs its parent to exist",
            })?;
            body.insert(field.clone(), Json::String(id.to_owned()));
        }
    }

    if let Some(contract) = contract
        && let BodyShape::Object(fields) = contract.body()
    {
        for required in fields.iter().filter(|field| field.required()) {
            if body.contains_key(required.name()) {
                continue;
            }
            let nullable = spec
                .fields
                .iter()
                .find(|(name, field)| field.request_name(name) == required.name())
                .is_some_and(|(_, field)| field.nullable);
            if !nullable {
                return Err(ApplyError::MissingRequired {
                    address: address.clone(),
                    field: required.name().to_owned(),
                });
            }
            body.insert(required.name().to_owned(), Json::Null);
        }
    }

    Ok(OperationRequest::new(&operation.op).body(Json::Object(body)))
}

/// One update request of a write group: the operation, and the fields it carries.
pub(crate) struct GroupRequest {
    pub(crate) operation: String,
    pub(crate) shape: Shape,
    /// The document field names of the group that changed.
    pub(crate) changed: Vec<String>,
    /// Every field of the group, in the spec's order.
    pub(crate) fields: Vec<String>,
}

/// Splits the written properties into the spec's write groups, in the order the spec lists
/// them. A property in no group cannot be changed in place.
pub(crate) fn groups(
    spec: &KindSpec,
    address: &ResourceAddress,
    written: &[&PropertyPath],
) -> Result<Vec<GroupRequest>, ApplyError> {
    let mut names = Vec::new();
    for path in written {
        names.push(writable(spec, address, path)?.name.to_owned());
    }
    let mut requests = Vec::new();
    for group in &spec.write {
        let WriteGroup::Op { op, fields, shape } = group else {
            continue;
        };
        let changed: Vec<String> = fields
            .iter()
            .filter(|f| names.contains(f))
            .cloned()
            .collect();
        if !changed.is_empty() {
            requests.push(GroupRequest {
                operation: op.clone(),
                shape: *shape,
                changed,
                fields: fields.clone(),
            });
        }
    }
    for name in &names {
        if !requests
            .iter()
            .any(|request| request.changed.contains(name))
        {
            return Err(ApplyError::Unsupported {
                address: address.clone(),
                property: Some(name.clone()),
                reason: "is in no write group the executor can use",
            });
        }
    }

    Ok(requests)
}

/// The body of one group's update. A `partial` group sends the id and the changed fields; a
/// `full` group sends every field of the group from a fresh read, overlaid with the changes.
pub(crate) fn group_body(
    spec: &KindSpec,
    address: &ResourceAddress,
    checkpoint: &ResourceCheckpoint,
    group: &GroupRequest,
    remote_id: &str,
    fresh: Option<&Json>,
    compiled: &Compiled,
) -> Result<Json, ApplyError> {
    let mut body = Map::new();
    body.insert(spec.api.id.clone(), Json::String(remote_id.to_owned()));

    for name in &group.fields {
        let field = spec
            .fields
            .get(name)
            .expect("a write group names fields of its kind");
        let wire = field.request_name(name).to_owned();
        let path = PropertyPath::from_spec(spec, name).map_err(|_| ApplyError::Unsupported {
            address: address.clone(),
            property: Some(name.clone()),
            reason: "is not a property of the kind",
        })?;
        if group.changed.contains(name) {
            body.insert(wire, body_value(compiled, address, checkpoint, &path)?);
            continue;
        }
        if group.shape == Shape::Partial {
            continue;
        }
        // A full group re-sends what is already there. A secret cannot be read back, so its
        // source must be declared in the document.
        let secret = field.class != dokploy_spec::ValueClass::Public
            || field.mutability == Mutability::WriteOnly;
        if secret {
            match checkpoint.property(&path) {
                Some(CheckpointValueRef::Sensitive) => {
                    body.insert(wire, secret_text(compiled, address, name)?);
                }
                _ => {
                    return Err(ApplyError::NeedsSecret {
                        address: address.clone(),
                        property: name.clone(),
                    });
                }
            }
        } else if let Some(current) = fresh.and_then(|fresh| fresh.get(&wire)) {
            body.insert(wire, current.clone());
        }
    }

    Ok(Json::Object(body))
}

/// The remove request.
pub(crate) fn remove_request(
    spec: &KindSpec,
    address: &ResourceAddress,
    remote_id: &str,
) -> Result<OperationRequest, ApplyError> {
    let operation = spec
        .api
        .remove
        .as_ref()
        .ok_or_else(|| ApplyError::Unsupported {
            address: address.clone(),
            property: None,
            reason: "has no remove operation",
        })?;
    let key = operation.id_param.as_deref().unwrap_or(&spec.api.id);

    Ok(OperationRequest::new(&operation.op).body(Json::Object(
        BTreeMap::from([(key.to_owned(), Json::String(remote_id.to_owned()))])
            .into_iter()
            .collect(),
    )))
}
