//! Response shapes copied from recorded fixtures (ADR 0015).
//!
//! A fixture is the sanitised response of a real Dokploy. The simulator uses it as a
//! template: an object template says which keys a response has, and each value comes from
//! the stored object; any other template (`true`, `{"success": true}`) is returned as is. A
//! kind without fixtures gets the plain stored object.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Where templates come from.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shapes {
    directory: Option<PathBuf>,
}

/// Which response of an operation a template describes.
#[derive(Clone, Copy)]
pub(crate) enum Role {
    /// The response of create, update, or remove.
    Mutation,
    /// A direct read.
    One,
    /// A collection read.
    List,
}

impl Shapes {
    pub(crate) fn from_directory(directory: &Path) -> Self {
        Self {
            directory: Some(directory.to_path_buf()),
        }
    }

    /// The fixture for `operation`, by the capture scripts' naming: `<resource>-<action>`,
    /// with the resource singular (`redirects.create` is `redirect-create`).
    pub(crate) fn template(&self, operation: &str, role: Role) -> Option<Value> {
        let directory = self.directory.as_ref()?;
        let (resource, action) = operation.split_once('.')?;
        let singular = resource.strip_suffix('s').unwrap_or(resource);
        let names: Vec<String> = [resource, singular]
            .iter()
            .flat_map(|resource| match role {
                Role::Mutation => vec![format!("{resource}-{action}.owner.json")],
                Role::One => vec![
                    format!("{resource}-one.created.owner.json"),
                    format!("{resource}-one.owner.json"),
                ],
                Role::List => vec![format!("{resource}-all.created.owner.json")],
            })
            .collect();
        names.iter().find_map(|name| {
            let text = std::fs::read_to_string(directory.join(name)).ok()?;
            serde_json::from_str(&text).ok()
        })
    }
}

/// Fills `template` from `stored`.
pub(crate) fn fill(template: &Value, stored: &Map<String, Value>) -> Value {
    match template {
        Value::Object(keys) if keys.contains_key("success") && keys.len() == 1 => template.clone(),
        // A key the stored object does not have is not returned, rather than returned as null.
        Value::Object(keys) => Value::Object(
            keys.keys()
                .filter_map(|key| stored.get(key).map(|value| (key.clone(), value.clone())))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Fills a collection template: each stored object takes the shape of the first item.
pub(crate) fn fill_list(template: &Value, stored: &[&Map<String, Value>]) -> Value {
    let item = template.as_array().and_then(|items| items.first());
    Value::Array(
        stored
            .iter()
            .map(|object| match item {
                Some(item) => fill(item, object),
                None => Value::Object((*object).clone()),
            })
            .collect(),
    )
}
