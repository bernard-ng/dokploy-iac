//! Building the requests of a mutation from a spec, a checkpoint, and the document's
//! secrets. Everything here is pure: nothing is sent, so a plan can be checked for what the
//! engine can do before the first byte goes out (the preflight).

use std::collections::BTreeMap;

use dokploy_api::{BodyShape, request_contract};
use dokploy_core::{CheckpointValueRef, ExternalResolution, PropertyPath, ResourceCheckpoint};
use dokploy_sdk::OperationRequest;
use dokploy_spec::{FieldType, KindSpec, Mutability, PathShape, Shape, WriteGroup, parse_type};
use dokploy_state::ResourceAddress;
use serde_json::{Map, Value as Json};

use super::ApplyError;
use crate::compile::Compiled;
use crate::envtext::EnvText;
use crate::selectors::SelectorIndex;

/// What a request is built from besides the spec and the checkpoint: the document's secrets and
/// the selectors as they resolve now. The preflight builds requests without reading, so it has
/// no selector index.
#[derive(Clone, Copy)]
pub(crate) struct Inputs<'a> {
    pub(crate) compiled: &'a Compiled,
    pub(crate) selectors: Option<&'a SelectorIndex>,
}

/// A property the executor can write.
pub(crate) struct Writable {
    /// The document field the property belongs to (its first path segment): what a write group
    /// names.
    pub(crate) name: String,
    /// The body key.
    pub(crate) wire: String,
    pub(crate) mutability: Mutability,
}

/// Resolves a property path to something the executor can write, or says why it cannot.
pub(crate) fn writable(
    spec: &KindSpec,
    address: &ResourceAddress,
    path: &PropertyPath,
) -> Result<Writable, ApplyError> {
    let unsupported = |reason: &'static str| ApplyError::Unsupported {
        address: address.clone(),
        property: Some(path.to_string()),
        reason,
    };
    let info = path.info();
    if !spec.fields.contains_key(info.field_name()) {
        return Err(unsupported("is not a field of the kind"));
    }
    match info.shape {
        // The variables of an environment block are written as the block's one text.
        PathShape::CollectionRoot | PathShape::CollectionEntry
            if matches!(info.ty, FieldType::Env) => {}
        PathShape::CollectionRoot | PathShape::CollectionEntry => {
            return Err(unsupported(
                "is a keyed collection of plain values, which the executor does not write yet",
            ));
        }
        PathShape::Atomic => {
            // A file is written as the bytes of the file the document names, which is a secret
            // value as far as the request is concerned.
            if matches!(
                info.ty,
                FieldType::Union { .. } | FieldType::Env | FieldType::Struct
            ) || (matches!(info.ty, FieldType::File) && !info.is_sensitive())
            {
                return Err(unsupported(
                    "is a composite value, which the executor does not write yet",
                ));
            }
        }
    }
    if info.api.trim_start_matches('/').contains('/') {
        return Err(unsupported(
            "maps to a nested request key, which the executor does not write yet",
        ));
    }

    Ok(Writable {
        name: info.field_name().to_owned(),
        wire: info.request_key().to_owned(),
        mutability: info.mutability,
    })
}

/// The JSON a checkpoint property is sent as: a value, `null`, or a secret read from the
/// document's source.
fn body_value(
    inputs: Inputs<'_>,
    address: &ResourceAddress,
    checkpoint: &ResourceCheckpoint,
    path: &PropertyPath,
) -> Result<Json, ApplyError> {
    let Inputs {
        compiled,
        selectors,
    } = inputs;
    match checkpoint.property(path) {
        Some(CheckpointValueRef::Null) => Ok(Json::Null),
        Some(CheckpointValueRef::NonSensitive(value)) if path.info().is_selector() => {
            selector_id(address, path, value, selectors)
        }
        Some(CheckpointValueRef::NonSensitive(value)) => Ok(value.clone()),
        Some(CheckpointValueRef::Sensitive) => secret_text(compiled, address, &path.to_string()),
        Some(CheckpointValueRef::EmptyCollection) | None => Err(ApplyError::Unsupported {
            address: address.clone(),
            property: Some(path.to_string()),
            reason: "has no value to send",
        }),
    }
}

/// What Dokploy is given for a selector: the id of the one resource with that name, resolved
/// now, or `null` for the host. The preflight reads nothing, so it only checks the shape.
fn selector_id(
    address: &ResourceAddress,
    path: &PropertyPath,
    value: &Json,
    selectors: Option<&SelectorIndex>,
) -> Result<Json, ApplyError> {
    let Some(selectors) = selectors else {
        return Ok(Json::Null);
    };
    let kind = path.info().selector.as_deref().unwrap_or_default();
    match selectors.resolve(kind, value) {
        Some(ExternalResolution::Local) => Ok(Json::Null),
        Some(ExternalResolution::Resolved(id)) => Ok(Json::String(id.as_str().to_owned())),
        _ => Err(ApplyError::UnresolvedSelector {
            address: address.clone(),
            property: path.to_string(),
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

/// A create request, and the managed properties it cannot carry.
pub(crate) struct CreateRequest {
    pub(crate) request: OperationRequest,
    /// Properties the create operation does not accept: written by an update right after.
    pub(crate) deferred: Vec<PropertyPath>,
}

/// The create request: every managed property the create operation accepts, the attachment to
/// the parent, and `null` for a field the contract requires that the document leaves out and
/// the spec allows to be null.
///
/// A managed property the create operation does not accept is not sent: it is `deferred`, and
/// the executor writes it with the spec's write groups once the resource exists.
pub(crate) fn create_request(
    spec: &KindSpec,
    address: &ResourceAddress,
    checkpoint: &ResourceCheckpoint,
    parent_id: Option<&str>,
    inputs: Inputs<'_>,
) -> Result<CreateRequest, ApplyError> {
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
    let mut deferred = Vec::new();

    for path in checkpoint.property_paths() {
        let target = writable(spec, address, path)?;
        if target.mutability == Mutability::Computed {
            continue;
        }
        // An environment block is one text that is built from what Dokploy holds, so it is
        // always written after the create.
        if path.info().shape != PathShape::Atomic
            || contract.is_some_and(|contract| {
                matches!(contract.body(), BodyShape::Object(_))
                    && contract.body_field(&target.wire).is_none()
            })
        {
            deferred.push(path.clone());
            continue;
        }
        body.insert(target.wire, body_value(inputs, address, checkpoint, path)?);
    }

    let parent_kind = checkpoint
        .containment()
        .map(|parent| parent.kind().as_str());
    for (field, source) in operation.attachments(parent_kind) {
        if source == "parent_id" {
            let id = parent_id.ok_or_else(|| ApplyError::Unsupported {
                address: address.clone(),
                property: None,
                reason: "needs its parent to exist",
            })?;
            body.insert(field.to_owned(), Json::String(id.to_owned()));
        } else {
            // A value fixed by the spec, such as the type of the parent.
            body.insert(field.to_owned(), Json::String(source.to_owned()));
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

    Ok(CreateRequest {
        request: OperationRequest::new(&operation.op).body(Json::Object(body)),
        deferred,
    })
}

/// One update request of a write group: the operation, and the fields it carries.
pub(crate) struct GroupRequest {
    pub(crate) operation: String,
    pub(crate) shape: Shape,
    /// The document field names of the group that changed.
    pub(crate) changed: Vec<String>,
    /// The dotted paths of the properties written (a member of a struct, a variable of an
    /// environment block, or a field).
    pub(crate) written: Vec<String>,
    /// Every field of the group, in the spec's order.
    pub(crate) fields: Vec<String>,
    /// Whether the body is built from what Dokploy holds now: a `full` group re-sends it, and
    /// an environment block keeps the variables the document does not own.
    pub(crate) reads_fresh: bool,
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
        names.push(writable(spec, address, path)?.name);
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
            let has_environment = fields.iter().any(|name| {
                spec.fields
                    .get(name)
                    .and_then(|field| parse_type(&field.ty).ok())
                    .is_some_and(|ty| matches!(ty, FieldType::Env))
            });
            requests.push(GroupRequest {
                operation: op.clone(),
                shape: *shape,
                written: written
                    .iter()
                    .filter(|path| fields.iter().any(|f| f == path.info().field_name()))
                    .map(|path| path.to_string())
                    .collect(),
                changed,
                fields: fields.clone(),
                reads_fresh: *shape == Shape::Full || has_environment,
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
/// `full` group sends every field of the group from a fresh read, overlaid with the changes. A
/// struct planned per member contributes its members, each under the key of its own; an
/// environment block is rebuilt from the fresh text with the owned variables set.
pub(crate) fn group_body(
    spec: &KindSpec,
    address: &ResourceAddress,
    checkpoint: &ResourceCheckpoint,
    group: &GroupRequest,
    remote_id: &str,
    fresh: Option<&Json>,
    inputs: Inputs<'_>,
) -> Result<Json, ApplyError> {
    let mut body = Map::new();
    body.insert(spec.api.id.clone(), Json::String(remote_id.to_owned()));

    for name in &group.fields {
        let field = spec
            .fields
            .get(name)
            .expect("a write group names fields of its kind");
        let ty = parse_type(&field.ty).ok();
        if matches!(ty, Some(FieldType::Env)) {
            let wire = field.request_name(name).to_owned();
            if group.changed.contains(name) {
                body.insert(
                    wire.clone(),
                    environment_text(spec, address, checkpoint, name, &wire, fresh, inputs)?,
                );
            } else if group.shape == Shape::Full
                && let Some(current) = fresh.and_then(|fresh| fresh.get(&wire))
            {
                body.insert(wire, current.clone());
            }
            continue;
        }
        // The properties the field plans as: its members, or itself.
        let paths: Vec<String> = if matches!(ty, Some(FieldType::Struct)) {
            field
                .members
                .keys()
                .map(|member| format!("{name}.{member}"))
                .collect()
        } else {
            vec![name.clone()]
        };
        for dotted in paths {
            let path =
                PropertyPath::from_spec(spec, &dotted).map_err(|_| ApplyError::Unsupported {
                    address: address.clone(),
                    property: Some(dotted.clone()),
                    reason: "is not a property of the kind",
                })?;
            let wire = path.info().request_key().to_owned();
            if group.written.contains(&dotted) {
                body.insert(wire, body_value(inputs, address, checkpoint, &path)?);
                continue;
            }
            if group.shape == Shape::Partial {
                continue;
            }
            // A full group re-sends what is already there. A secret cannot be read back, so its
            // source must be declared in the document.
            if path.info().is_sensitive() {
                match checkpoint.property(&path) {
                    Some(CheckpointValueRef::Sensitive) => {
                        body.insert(wire, secret_text(inputs.compiled, address, &dotted)?);
                    }
                    _ => {
                        return Err(ApplyError::NeedsSecret {
                            address: address.clone(),
                            property: dotted,
                        });
                    }
                }
            } else if let Some(current) = fresh.and_then(|fresh| fresh.get(&wire)) {
                body.insert(wire, current.clone());
            }
        }
    }

    Ok(Json::Object(body))
}

/// The text of an environment block after the update: the fresh text with every variable the
/// document owns set to its value, so the variables it does not own stay as they are. Clearing
/// the block sends `null`, declaring it empty sends an empty text.
fn environment_text(
    spec: &KindSpec,
    address: &ResourceAddress,
    checkpoint: &ResourceCheckpoint,
    name: &str,
    wire: &str,
    fresh: Option<&Json>,
    inputs: Inputs<'_>,
) -> Result<Json, ApplyError> {
    let unsupported = |reason: &'static str| ApplyError::Unsupported {
        address: address.clone(),
        property: Some(name.to_owned()),
        reason,
    };
    let root = PropertyPath::from_spec(spec, name)
        .map_err(|_| unsupported("is not a property of the kind"))?;
    match checkpoint.property(&root) {
        Some(CheckpointValueRef::Null) => return Ok(Json::Null),
        Some(CheckpointValueRef::EmptyCollection) => return Ok(Json::String(String::new())),
        _ => {}
    }
    let mut text = EnvText::parse(
        fresh
            .and_then(|fresh| fresh.get(wire))
            .and_then(Json::as_str)
            .unwrap_or_default(),
    );
    for path in checkpoint.property_paths() {
        if path.info().root.as_deref() != Some(name) {
            continue;
        }
        let dotted = path.to_string();
        let key = dotted.split_once('.').map_or("", |(_, key)| key);
        let value = match secret_text(inputs.compiled, address, &dotted)? {
            Json::String(value) => value,
            _ => return Err(unsupported("has a variable that is not text")),
        };
        if value.contains(['\n', '\r']) {
            return Err(unsupported(
                "has a variable whose value spans lines, which an environment block cannot carry",
            ));
        }
        text.set(key, &value);
    }

    Ok(Json::String(text.render()))
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
