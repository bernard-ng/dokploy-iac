//! The canonical text of a document.
//!
//! Rendering is deterministic: `version`, then the root; within a resource, fields in
//! alphabetical order, then `depends_on`, `lifecycle`, and the child sections in
//! alphabetical order; resources by key. Parsing the output gives back an equal document,
//! and rendering that gives back the same text.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::document::{Document, Resource, Root, Sections};
use crate::read::FORMAT_VERSION;
use crate::value::{EnvValue, Selector, Source, Value};

/// A tree ready to print.
enum Out {
    Scalar(String),
    Seq(Vec<Out>),
    Map(Vec<(String, Out)>),
}

impl Document {
    /// Renders the document as canonical YAML.
    #[must_use]
    pub fn render(&self) -> String {
        let (name, root) = match &self.root {
            Root::Project(project) => {
                let mut entries = vec![("slug".to_owned(), Out::Scalar(scalar(&project.key)))];
                entries.extend(resource_entries(project));
                ("project", Out::Map(entries))
            }
            Root::Settings(sections) => ("settings", Out::Map(section_entries(sections))),
        };
        let mut top = vec![
            (
                "version".to_owned(),
                Out::Scalar(FORMAT_VERSION.to_string()),
            ),
            (name.to_owned(), root),
        ];
        if !self.moves.is_empty() {
            let mut moves: Vec<_> = self.moves.iter().collect();
            moves.sort_by(|a, b| (&a.to, &a.from).cmp(&(&b.to, &b.from)));
            top.push((
                "moves".to_owned(),
                Out::Seq(
                    moves
                        .into_iter()
                        .map(|m| {
                            Out::Map(vec![
                                ("from".to_owned(), Out::Scalar(scalar(&m.from))),
                                ("to".to_owned(), Out::Scalar(scalar(&m.to))),
                            ])
                        })
                        .collect(),
                ),
            ));
        }
        if !self.removed.is_empty() {
            let mut removed: Vec<_> = self.removed.iter().collect();
            removed.sort_by(|a, b| a.from.cmp(&b.from));
            top.push((
                "removed".to_owned(),
                Out::Seq(
                    removed
                        .into_iter()
                        .map(|r| {
                            Out::Map(vec![
                                ("from".to_owned(), Out::Scalar(scalar(&r.from))),
                                ("destroy".to_owned(), Out::Scalar(r.destroy.to_string())),
                            ])
                        })
                        .collect(),
                ),
            ));
        }
        let document = Out::Map(top);
        let mut text = String::new();
        emit(&document, 0, &mut text);

        text
    }
}

fn section_entries(sections: &Sections) -> Vec<(String, Out)> {
    sections
        .iter()
        .map(|(section, resources)| {
            let entries = resources
                .iter()
                .map(|(key, resource)| (key.clone(), Out::Map(resource_entries(resource))))
                .collect();
            (section.clone(), Out::Map(entries))
        })
        .collect()
}

fn resource_entries(resource: &Resource) -> Vec<(String, Out)> {
    let mut entries: Vec<(String, Out)> = resource
        .fields
        .iter()
        .map(|(name, field)| (name.clone(), value_out(&field.value)))
        .collect();
    if !resource.depends_on.is_empty() {
        entries.push((
            "depends_on".to_owned(),
            Out::Seq(
                resource
                    .depends_on
                    .iter()
                    .map(|address| Out::Scalar(scalar(address)))
                    .collect(),
            ),
        ));
    }
    let mut lifecycle = Vec::new();
    if let Some(protect) = resource.lifecycle.protect {
        lifecycle.push(("protect".to_owned(), Out::Scalar(protect.to_string())));
    }
    if !resource.lifecycle.ignore_changes.is_empty() {
        lifecycle.push((
            "ignore_changes".to_owned(),
            Out::Seq(
                resource
                    .lifecycle
                    .ignore_changes
                    .iter()
                    .map(|path| Out::Scalar(scalar(path)))
                    .collect(),
            ),
        ));
    }
    if !lifecycle.is_empty() {
        entries.push(("lifecycle".to_owned(), Out::Map(lifecycle)));
    }
    entries.extend(section_entries(&resource.children));

    entries
}

fn value_out(value: &Value) -> Out {
    match value {
        Value::Null => Out::Scalar("null".to_owned()),
        Value::Bool(flag) => Out::Scalar(flag.to_string()),
        Value::Int(number) => Out::Scalar(number.to_string()),
        Value::Number(number) => Out::Scalar(format!("{number:?}")),
        Value::Text(text) => Out::Scalar(scalar(text)),
        Value::List(items) => Out::Seq(items.iter().map(value_out).collect()),
        Value::Map(entries) => map_out(entries),
        Value::Union { tag, fields } => {
            let mut entries = vec![("type".to_owned(), Out::Scalar(scalar(tag)))];
            entries.extend(
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), value_out(value))),
            );
            Out::Map(entries)
        }
        Value::Env(variables) => Out::Map(
            variables
                .iter()
                .map(|(name, value)| (name.clone(), env_out(value)))
                .collect(),
        ),
        Value::Source(source) => source_out(source),
        Value::Selector(Selector::Name(name)) => {
            Out::Map(vec![("name".to_owned(), Out::Scalar(scalar(name)))])
        }
        Value::Selector(Selector::Local) => {
            Out::Map(vec![("local".to_owned(), Out::Scalar("true".to_owned()))])
        }
        Value::Blob(json) => json_out(json),
    }
}

fn map_out(entries: &BTreeMap<String, Value>) -> Out {
    Out::Map(
        entries
            .iter()
            .map(|(name, value)| (name.clone(), value_out(value)))
            .collect(),
    )
}

fn env_out(value: &EnvValue) -> Out {
    match value {
        EnvValue::Public(text) => Out::Scalar(scalar(text)),
        EnvValue::Secret(source @ Source::Vault { .. }) => source_out(source),
        EnvValue::Secret(source) => Out::Map(vec![("secret".to_owned(), source_out(source))]),
    }
}

fn source_out(source: &Source) -> Out {
    match source {
        Source::Env(name) => Out::Map(vec![("env".to_owned(), Out::Scalar(scalar(name)))]),
        Source::File(path) => Out::Map(vec![("file".to_owned(), Out::Scalar(scalar(path)))]),
        Source::Vault { provider, secret } => Out::Map(vec![(
            "vault".to_owned(),
            Out::Map(vec![
                ("provider".to_owned(), Out::Scalar(scalar(provider))),
                ("secret".to_owned(), Out::Scalar(scalar(secret))),
            ]),
        )]),
    }
}

fn json_out(json: &serde_json::Value) -> Out {
    match json {
        serde_json::Value::Null => Out::Scalar("null".to_owned()),
        serde_json::Value::Bool(flag) => Out::Scalar(flag.to_string()),
        serde_json::Value::Number(number) => {
            if number.is_f64() {
                Out::Scalar(format!("{:?}", number.as_f64().unwrap_or_default()))
            } else {
                Out::Scalar(number.to_string())
            }
        }
        serde_json::Value::String(text) => Out::Scalar(scalar(text)),
        serde_json::Value::Array(items) => Out::Seq(items.iter().map(json_out).collect()),
        serde_json::Value::Object(entries) => Out::Map(
            entries
                .iter()
                .map(|(name, value)| (name.clone(), json_out(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
    }
}

/// A YAML scalar for `text`: plain when that reads back as the same text, otherwise a
/// double-quoted string.
fn scalar(text: &str) -> String {
    if plain_is_safe(text) {
        text.to_owned()
    } else {
        serde_json::to_string(text).expect("a string always serializes")
    }
}

fn plain_is_safe(text: &str) -> bool {
    let mut characters = text.chars();
    let starts_well = characters
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    starts_well
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '@' | '-'))
        && !matches!(
            text.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n" | "nan" | "inf"
        )
        && !text.ends_with('-')
}

fn emit(out: &Out, indent: usize, text: &mut String) {
    match out {
        Out::Scalar(value) => {
            let _ = writeln!(text, "{value}");
        }
        Out::Map(entries) => {
            for (key, value) in entries {
                let pad = " ".repeat(indent);
                let key = scalar(key);
                match value {
                    Out::Scalar(value) => {
                        let _ = writeln!(text, "{pad}{key}: {value}");
                    }
                    Out::Map(inner) if inner.is_empty() => {
                        let _ = writeln!(text, "{pad}{key}: {{}}");
                    }
                    Out::Seq(inner) if inner.is_empty() => {
                        let _ = writeln!(text, "{pad}{key}: []");
                    }
                    nested => {
                        let _ = writeln!(text, "{pad}{key}:");
                        emit_nested(nested, indent + 2, text);
                    }
                }
            }
        }
        Out::Seq(_) => emit_nested(out, indent, text),
    }
}

fn emit_nested(out: &Out, indent: usize, text: &mut String) {
    match out {
        Out::Scalar(value) => {
            let _ = writeln!(text, "{}{value}", " ".repeat(indent));
        }
        Out::Map(_) => emit(out, indent, text),
        Out::Seq(items) => {
            let pad = " ".repeat(indent);
            for item in items {
                match item {
                    Out::Scalar(value) => {
                        let _ = writeln!(text, "{pad}- {value}");
                    }
                    Out::Map(entries) if entries.is_empty() => {
                        let _ = writeln!(text, "{pad}- {{}}");
                    }
                    Out::Seq(inner) if inner.is_empty() => {
                        let _ = writeln!(text, "{pad}- []");
                    }
                    Out::Map(_) | Out::Seq(_) => {
                        // A nested block starts on the next line, indented under the dash.
                        let _ = writeln!(text, "{pad}-");
                        emit_nested(item, indent + 2, text);
                    }
                }
            }
        }
    }
}
