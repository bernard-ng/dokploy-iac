//! The JSON Schema of a document, built from the kind specs.

use dokploy_spec::{
    Field, FieldType, Granularity, KindSpec, Scope, SpecRegistry, ValueClass, parse_type,
};
use serde_json::{Map, Value, json};

use crate::read::FORMAT_VERSION;

const KEY_PATTERN: &str = "^[a-z][a-z0-9_-]*$";
const ENV_NAME_PATTERN: &str = "^[A-Za-z_][A-Za-z0-9_]*$";

/// The JSON Schema (draft 2020-12) of the document of one scope.
///
/// It accepts what [`Document::parse`](crate::Document::parse) accepts, for editor
/// completion and early errors. Rules that need the whole document or the filesystem (that
/// a `depends_on` address exists, that a file is readable) stay with the parser.
#[must_use]
pub fn json_schema(registry: &SpecRegistry, scope: Scope) -> Value {
    let mut definitions = Map::new();
    definitions.insert("secret_source".to_owned(), secret_source());
    definitions.insert("file_source".to_owned(), file_source());
    definitions.insert("env_value".to_owned(), env_value());
    definitions.insert("selector".to_owned(), selector(false));
    definitions.insert("selector_server".to_owned(), selector(true));
    definitions.insert("lifecycle".to_owned(), lifecycle());
    for spec in registry.kinds().filter(|spec| spec.scope == scope) {
        definitions.insert(spec.kind.clone(), resource(registry, spec));
    }

    let (name, title, root) = match scope {
        Scope::Settings => (
            "settings",
            "Dokploy settings document",
            settings_root(registry),
        ),
        Scope::Project => (
            "project",
            "Dokploy project document",
            project_root(registry),
        ),
    };
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "type": "object",
        "additionalProperties": false,
        "required": ["version", name],
        "properties": {
            "version": { "const": FORMAT_VERSION },
            name: root,
            "moves": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["from", "to"],
                    "properties": { "from": address(), "to": address() },
                },
            },
            "removed": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["from"],
                    "properties": { "from": address(), "destroy": { "type": "boolean" } },
                },
            },
        },
        "$defs": definitions,
    })
}

/// An address as a person writes it: `kind.key`, joined with `/` for each level.
fn address() -> Value {
    json!({ "type": "string", "pattern": "^[^./\\s]+\\.[^./\\s]+(/[^./\\s]+\\.[^./\\s]+)*$" })
}

fn reference(name: &str) -> Value {
    json!({ "$ref": format!("#/$defs/{name}") })
}

fn settings_root(registry: &SpecRegistry) -> Value {
    let mut sections = Map::new();
    for spec in registry.roots(Scope::Settings) {
        sections.insert(spec.section.clone(), keyed(&spec.kind));
    }
    json!({ "type": "object", "additionalProperties": false, "properties": sections })
}

fn project_root(registry: &SpecRegistry) -> Value {
    let Some(spec) = registry.roots(Scope::Project).into_iter().next() else {
        return json!({ "type": "object" });
    };
    let mut root = resource(registry, spec);
    if let Some(object) = root.as_object_mut() {
        if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
            properties.insert(
                "slug".to_owned(),
                json!({ "type": "string", "pattern": KEY_PATTERN }),
            );
        }
        object.insert("required".to_owned(), json!(["slug"]));
    }

    root
}

/// A mapping from resource keys to resources of one kind.
fn keyed(kind: &str) -> Value {
    json!({
        "type": "object",
        "propertyNames": { "pattern": KEY_PATTERN },
        "additionalProperties": reference(kind),
    })
}

fn resource(registry: &SpecRegistry, spec: &KindSpec) -> Value {
    let mut properties = Map::new();
    for (name, field) in &spec.fields {
        if let Ok(ty) = parse_type(&field.ty) {
            properties.insert(name.clone(), field_schema(field, &ty));
        }
    }
    for child in &spec.children {
        if registry.get(&child.kind).is_some() {
            properties.insert(child.section.clone(), keyed(&child.kind));
        }
    }
    properties.insert("lifecycle".to_owned(), reference("lifecycle"));
    properties.insert(
        "depends_on".to_owned(),
        json!({ "type": "array", "items": { "type": "string", "minLength": 1 } }),
    );

    json!({
        "title": spec.title,
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
    })
}

fn field_schema(field: &Field, ty: &FieldType) -> Value {
    let mut schema = match field.class {
        ValueClass::Secret if matches!(ty, FieldType::Text) => reference("secret_source"),
        ValueClass::Content => reference("file_source"),
        _ => type_schema(ty, field),
    };
    if let (Some(object), Some(doc)) = (schema.as_object_mut(), &field.doc) {
        object.insert("description".to_owned(), json!(doc));
    }
    let collection = matches!(ty, FieldType::Env | FieldType::Map(_))
        && field.granularity == Some(Granularity::Key);
    if field.nullable || collection {
        json!({ "anyOf": [schema, { "type": "null" }] })
    } else {
        schema
    }
}

fn type_schema(ty: &FieldType, field: &Field) -> Value {
    match ty {
        FieldType::Text => {
            let mut schema = json!({ "type": "string" });
            if let Some(object) = schema.as_object_mut() {
                if let Some(min) = field.min_len {
                    object.insert("minLength".to_owned(), json!(min));
                }
                if let Some(pattern) = &field.pattern {
                    object.insert("pattern".to_owned(), json!(pattern));
                }
            }
            schema
        }
        FieldType::Int | FieldType::Number => {
            let name = if matches!(ty, FieldType::Int) {
                "integer"
            } else {
                "number"
            };
            let mut schema = json!({ "type": name });
            if let Some(object) = schema.as_object_mut() {
                if let Some(min) = field.min {
                    object.insert("minimum".to_owned(), json!(min));
                }
                if let Some(max) = field.max {
                    object.insert("maximum".to_owned(), json!(max));
                }
            }
            schema
        }
        FieldType::Bool => json!({ "type": "boolean" }),
        FieldType::Enum(values) => json!({ "enum": values }),
        FieldType::List(item) => json!({ "type": "array", "items": element(item) }),
        FieldType::Set(item) => {
            json!({ "type": "array", "uniqueItems": true, "items": element(item) })
        }
        FieldType::Map(item) => json!({ "type": "object", "additionalProperties": element(item) }),
        FieldType::Struct => {
            let mut members = Map::new();
            for (name, member) in &field.members {
                if let Ok(member_type) = parse_type(&member.ty) {
                    members.insert(name.clone(), field_schema(member, &member_type));
                }
            }
            json!({ "type": "object", "additionalProperties": false, "properties": members })
        }
        FieldType::Union { tag } => {
            let arms: Vec<Value> = field
                .arms
                .iter()
                .map(|(arm, members)| {
                    let mut properties = Map::new();
                    properties.insert(tag.clone(), json!({ "const": arm }));
                    for (name, member) in members {
                        if let Ok(member_type) = parse_type(&member.ty) {
                            properties.insert(name.clone(), field_schema(member, &member_type));
                        }
                    }
                    json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": [tag],
                        "properties": properties,
                    })
                })
                .collect();
            json!({ "oneOf": arms })
        }
        FieldType::Env => json!({
            "type": "object",
            "propertyNames": { "pattern": ENV_NAME_PATTERN },
            "additionalProperties": reference("env_value"),
        }),
        FieldType::File => reference("file_source"),
        FieldType::Ref(_) => json!({ "type": "string", "minLength": 1 }),
        FieldType::Selector(kind) => reference(if kind == "server" {
            "selector_server"
        } else {
            "selector"
        }),
        FieldType::Blob(_) | FieldType::Shared(_) => json!({}),
    }
}

/// The schema of a list item or map value, which carry no field attributes.
fn element(ty: &FieldType) -> Value {
    type_schema(ty, &Field::default())
}

fn secret_source() -> Value {
    json!({
        "oneOf": [
            { "type": "object", "additionalProperties": false, "required": ["env"],
              "properties": { "env": { "type": "string", "pattern": ENV_NAME_PATTERN } } },
            file_source(),
            vault(),
        ]
    })
}

fn vault() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["vault"],
        "properties": { "vault": {
            "type": "object", "additionalProperties": false, "required": ["provider", "secret"],
            "properties": {
                "provider": { "type": "string", "minLength": 1 },
                "secret": { "type": "string", "minLength": 1 }
            }
        } }
    })
}

fn file_source() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["file"],
        "properties": { "file": { "type": "string", "minLength": 1 } }
    })
}

fn env_value() -> Value {
    json!({
        "anyOf": [
            { "type": "string" },
            { "type": "object", "additionalProperties": false, "required": ["value"],
              "properties": { "value": { "type": "string" } } },
            { "type": "object", "additionalProperties": false, "required": ["secret"],
              "properties": { "secret": reference("secret_source") } },
            vault(),
        ]
    })
}

fn selector(local_allowed: bool) -> Value {
    let by_name = json!({
        "type": "object", "additionalProperties": false, "required": ["name"],
        "properties": { "name": { "type": "string", "minLength": 1, "maxLength": 256 } }
    });
    if local_allowed {
        json!({ "oneOf": [by_name, {
            "type": "object", "additionalProperties": false, "required": ["local"],
            "properties": { "local": { "const": true } }
        }] })
    } else {
        by_name
    }
}

fn lifecycle() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "protect": { "type": "boolean" },
            "ignore_changes": { "type": "array", "items": { "type": "string", "minLength": 1 } },
        }
    })
}
