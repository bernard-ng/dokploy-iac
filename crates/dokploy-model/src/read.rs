//! The spec-driven reader: turns the raw tree into a typed [`Document`], or reports every
//! problem it finds, each with the position of the offending node.

use std::collections::BTreeMap;

use dokploy_spec::{
    Field as SpecField, FieldType, Granularity, KindSpec, Scope, SpecRegistry, ValueClass,
    parse_type,
};

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::document::{Document, Lifecycle, Resource, Root, Sections};
use crate::raw::{Key, Node, Raw};
use crate::value::{EnvValue, Selector, Source, Value};
use crate::{Field, Span};

/// The document format this crate reads and writes.
pub const FORMAT_VERSION: i64 = 2;

const MAX_KEY_LEN: usize = 128;
const MAX_NAME_LEN: usize = 256;
const MAX_PATH_LEN: usize = 4096;
const LISTED_NAMES: usize = 12;

pub(crate) struct Reader<'a> {
    registry: &'a SpecRegistry,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// The value rules a field attaches to its own value (not to its elements).
#[derive(Default)]
struct Rules<'a> {
    min_len: Option<u64>,
    min: Option<f64>,
    max: Option<f64>,
    pattern: Option<&'a str>,
}

impl<'a> Rules<'a> {
    fn of(field: &'a SpecField) -> Self {
        Self {
            min_len: field.min_len,
            min: field.min,
            max: field.max,
            pattern: field.pattern.as_deref(),
        }
    }
}

impl<'a> Reader<'a> {
    pub(crate) fn new(registry: &'a SpecRegistry) -> Self {
        Self {
            registry,
            diagnostics: Vec::new(),
        }
    }

    fn error(&mut self, code: DiagnosticCode, path: &str, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            code,
            path: path.to_owned(),
            span,
            message: message.into(),
        });
    }

    // ----- the document ---------------------------------------------------

    pub(crate) fn document(&mut self, root: &Node) -> Option<Document> {
        let Raw::Map(entries) = &root.value else {
            self.error(
                DiagnosticCode::Root,
                "",
                root.span,
                "a document is a mapping with `version` and one of `project` or `settings`",
            );
            return None;
        };

        let mut version_ok = false;
        let mut project = None;
        let mut settings = None;
        for (key, node) in entries {
            match key.name.as_str() {
                "version" => version_ok = self.version(node),
                "project" => project = Some((key, node)),
                "settings" => settings = Some((key, node)),
                other => self.error(
                    DiagnosticCode::UnknownField,
                    other,
                    key.span,
                    format!(
                        "unknown top-level key `{other}`; a document has `version` and one of `project` or `settings`"
                    ),
                ),
            }
        }
        if !entries.iter().any(|(key, _)| key.name == "version") {
            self.error(
                DiagnosticCode::Version,
                "version",
                root.span,
                format!("missing `version: {FORMAT_VERSION}`"),
            );
        }

        let document = match (project, settings) {
            (Some(_), Some((key, _))) => {
                self.error(
                    DiagnosticCode::Root,
                    "settings",
                    key.span,
                    "a document holds either `project` or `settings`, not both",
                );
                None
            }
            (None, None) => {
                self.error(
                    DiagnosticCode::Root,
                    "",
                    root.span,
                    "a document needs a `project` or a `settings` root",
                );
                None
            }
            (Some((_, node)), None) => self.project(node),
            (None, Some((_, node))) => self.settings(node),
        };
        (version_ok && self.diagnostics.is_empty())
            .then_some(document)
            .flatten()
    }

    fn version(&mut self, node: &Node) -> bool {
        match &node.value {
            Raw::Int(FORMAT_VERSION) => true,
            Raw::Int(1) => {
                self.error(
                    DiagnosticCode::Version,
                    "version",
                    node.span,
                    "this document is format 1, written for the first engine; re-import it to get a version 2 document",
                );
                false
            }
            _ => {
                self.error(
                    DiagnosticCode::Version,
                    "version",
                    node.span,
                    format!("`version` must be {FORMAT_VERSION}"),
                );
                false
            }
        }
    }

    fn settings(&mut self, node: &Node) -> Option<Document> {
        let Raw::Map(sections) = &node.value else {
            self.error(
                DiagnosticCode::Type,
                "settings",
                node.span,
                format!(
                    "`settings` must be a mapping, found {}",
                    node.value.describe()
                ),
            );
            return None;
        };
        let roots = self.registry.roots(Scope::Settings);
        let mut document: Sections = BTreeMap::new();
        for (section, section_node) in sections {
            let path = format!("settings.{}", section.name);
            let Some(spec) = roots.iter().find(|spec| spec.section == section.name) else {
                let valid: Vec<&str> = roots.iter().map(|spec| spec.section.as_str()).collect();
                self.unknown(&path, section.span, "settings", &section.name, &valid);
                continue;
            };
            let resources = self.resource_map(spec, section_node, &path);
            document.insert(section.name.clone(), resources);
        }

        Some(Document {
            scope: Scope::Settings,
            root: Root::Settings(document),
        })
    }

    fn project(&mut self, node: &Node) -> Option<Document> {
        let roots = self.registry.roots(Scope::Project);
        let [spec] = roots.as_slice() else {
            self.error(
                DiagnosticCode::Root,
                "project",
                node.span,
                "the specs define no single project kind",
            );
            return None;
        };
        let Raw::Map(entries) = &node.value else {
            self.error(
                DiagnosticCode::Type,
                "project",
                node.span,
                format!(
                    "`project` must be a mapping, found {}",
                    node.value.describe()
                ),
            );
            return None;
        };
        let Some((slug_key, slug_node)) = entries.iter().find(|(key, _)| key.name == "slug") else {
            self.error(
                DiagnosticCode::Key,
                "project.slug",
                node.span,
                "a project needs a `slug`: its key in addresses and its state directory",
            );
            return None;
        };
        let key = self.key_text(slug_node, "project.slug")?;
        let resource = self.body(spec, &key, slug_key.span, entries, "project", Some("slug"))?;

        Some(Document {
            scope: Scope::Project,
            root: Root::Project(Box::new(resource)),
        })
    }

    // ----- resources ------------------------------------------------------

    fn resource_map(
        &mut self,
        spec: &KindSpec,
        node: &Node,
        path: &str,
    ) -> BTreeMap<String, Resource> {
        let mut resources = BTreeMap::new();
        let Raw::Map(entries) = &node.value else {
            self.error(
                DiagnosticCode::Type,
                path,
                node.span,
                format!(
                    "`{}` maps keys to {} resources, found {}",
                    spec.section,
                    spec.kind,
                    node.value.describe()
                ),
            );
            return resources;
        };
        for (key, resource_node) in entries {
            let resource_path = format!("{path}.{}", key.name);
            if !valid_key(&key.name) {
                self.error(
                    DiagnosticCode::Key,
                    &resource_path,
                    key.span,
                    format!(
                        "`{}` is not a valid key; use lower-case letters, digits, `-` and `_`, starting with a letter",
                        key.name
                    ),
                );
                continue;
            }
            let Raw::Map(fields) = &resource_node.value else {
                self.error(
                    DiagnosticCode::Type,
                    &resource_path,
                    resource_node.span,
                    format!(
                        "a {} is a mapping of fields, found {}",
                        spec.kind,
                        resource_node.value.describe()
                    ),
                );
                continue;
            };
            if let Some(resource) =
                self.body(spec, &key.name, key.span, fields, &resource_path, None)
            {
                resources.insert(key.name.clone(), resource);
            }
        }

        resources
    }

    fn body(
        &mut self,
        spec: &KindSpec,
        key: &str,
        key_span: Span,
        entries: &[(Key, Node)],
        path: &str,
        skip: Option<&str>,
    ) -> Option<Resource> {
        let mut fields = BTreeMap::new();
        let mut children: Sections = BTreeMap::new();
        let mut lifecycle = Lifecycle::default();
        let mut depends_on = Vec::new();
        let mut failed = false;
        let before = self.diagnostics.len();

        for (name, node) in entries {
            if skip == Some(name.name.as_str()) {
                continue;
            }
            let field_path = format!("{path}.{}", name.name);
            if name.name == "lifecycle" {
                lifecycle = self.lifecycle(spec, node, &field_path);
            } else if name.name == "depends_on" {
                depends_on = self.depends_on(node, &field_path);
            } else if let Some(field) = spec.fields.get(&name.name) {
                if let Some(value) = self.field(field, node, &field_path) {
                    fields.insert(
                        name.name.clone(),
                        Field {
                            value,
                            span: name.span,
                        },
                    );
                }
            } else if let Some(child) = spec.children.iter().find(|c| c.section == name.name) {
                let Some(child_spec) = self.registry.get(&child.kind) else {
                    failed = true;
                    continue;
                };
                let resources = self.resource_map(child_spec, node, &field_path);
                children.insert(name.name.clone(), resources);
            } else {
                let mut valid: Vec<&str> = spec.fields.keys().map(String::as_str).collect();
                valid.extend(spec.children.iter().map(|c| c.section.as_str()));
                valid.extend(["depends_on", "lifecycle"]);
                valid.sort_unstable();
                self.unknown(&field_path, name.span, &spec.kind, &name.name, &valid);
            }
        }

        (!failed && self.diagnostics.len() == before).then(|| Resource {
            kind: spec.kind.clone(),
            key: key.to_owned(),
            span: key_span,
            fields,
            lifecycle,
            depends_on,
            children,
        })
    }

    fn unknown(&mut self, path: &str, span: Span, owner: &str, name: &str, valid: &[&str]) {
        let shown = valid
            .iter()
            .take(LISTED_NAMES)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let more = if valid.len() > LISTED_NAMES {
            ", ..."
        } else {
            ""
        };
        self.error(
            DiagnosticCode::UnknownField,
            path,
            span,
            format!("`{owner}` has no `{name}`; it has: {shown}{more}"),
        );
    }

    fn key_text(&mut self, node: &Node, path: &str) -> Option<String> {
        match &node.value {
            Raw::Text(text) if valid_key(text) => Some(text.clone()),
            Raw::Text(text) => {
                self.error(
                    DiagnosticCode::Key,
                    path,
                    node.span,
                    format!(
                        "`{text}` is not a valid key; use lower-case letters, digits, `-` and `_`, starting with a letter"
                    ),
                );
                None
            }
            other => {
                self.error(
                    DiagnosticCode::Type,
                    path,
                    node.span,
                    format!("a key is text, found {}", other.describe()),
                );
                None
            }
        }
    }

    // ----- directives -----------------------------------------------------

    fn lifecycle(&mut self, spec: &KindSpec, node: &Node, path: &str) -> Lifecycle {
        let mut lifecycle = Lifecycle::default();
        let Raw::Map(entries) = &node.value else {
            self.error(
                DiagnosticCode::Directive,
                path,
                node.span,
                "`lifecycle` is a mapping with `protect` and `ignore_changes`",
            );
            return lifecycle;
        };
        for (key, value) in entries {
            let entry_path = format!("{path}.{}", key.name);
            match (key.name.as_str(), &value.value) {
                ("protect", Raw::Bool(flag)) => lifecycle.protect = Some(*flag),
                ("protect", other) => self.error(
                    DiagnosticCode::Directive,
                    &entry_path,
                    value.span,
                    format!("`protect` is true or false, found {}", other.describe()),
                ),
                ("ignore_changes", Raw::Seq(items)) => {
                    for (index, item) in items.iter().enumerate() {
                        let item_path = format!("{entry_path}[{index}]");
                        match &item.value {
                            Raw::Text(text) => match spec.property(text) {
                                Ok(_) => lifecycle.ignore_changes.push(text.clone()),
                                Err(error) => self.error(
                                    DiagnosticCode::Directive,
                                    &item_path,
                                    item.span,
                                    format!("`{text}` cannot be ignored: {error}"),
                                ),
                            },
                            other => self.error(
                                DiagnosticCode::Directive,
                                &item_path,
                                item.span,
                                format!("a property path is text, found {}", other.describe()),
                            ),
                        }
                    }
                }
                ("ignore_changes", other) => self.error(
                    DiagnosticCode::Directive,
                    &entry_path,
                    value.span,
                    format!("`ignore_changes` is a list, found {}", other.describe()),
                ),
                (other, _) => self.error(
                    DiagnosticCode::UnknownField,
                    &entry_path,
                    key.span,
                    format!("`lifecycle` has no `{other}`; it has: ignore_changes, protect"),
                ),
            }
        }

        lifecycle
    }

    fn depends_on(&mut self, node: &Node, path: &str) -> Vec<String> {
        let Raw::Seq(items) = &node.value else {
            self.error(
                DiagnosticCode::Directive,
                path,
                node.span,
                format!(
                    "`depends_on` is a list of addresses, found {}",
                    node.value.describe()
                ),
            );
            return Vec::new();
        };
        let mut addresses = Vec::new();
        for (index, item) in items.iter().enumerate() {
            match &item.value {
                Raw::Text(text) if !text.trim().is_empty() => addresses.push(text.clone()),
                _ => self.error(
                    DiagnosticCode::Directive,
                    &format!("{path}[{index}]"),
                    item.span,
                    "an address is non-empty text",
                ),
            }
        }

        addresses
    }

    // ----- fields ---------------------------------------------------------

    fn field(&mut self, field: &SpecField, node: &Node, path: &str) -> Option<Value> {
        let ty = match parse_type(&field.ty) {
            Ok(ty) => ty,
            Err(error) => {
                self.error(DiagnosticCode::Type, path, node.span, error.to_string());
                return None;
            }
        };
        if matches!(node.value, Raw::Null) {
            // A keyed collection can always be cleared as a whole.
            let collection = matches!(ty, FieldType::Env | FieldType::Map(_))
                && field.granularity == Some(Granularity::Key);
            if field.nullable || collection {
                return Some(Value::Null);
            }
            self.error(
                DiagnosticCode::Null,
                path,
                node.span,
                "this field cannot be cleared with null; omit it to leave it unmanaged",
            );
            return None;
        }
        match field.class {
            ValueClass::Secret if matches!(ty, FieldType::Text) => {
                self.source(node, path, true).map(Value::Source)
            }
            ValueClass::Content => self.file_source(node, path).map(Value::Source),
            _ => self.typed(&ty, Some(field), &Rules::of(field), node, path),
        }
    }

    fn typed(
        &mut self,
        ty: &FieldType,
        field: Option<&SpecField>,
        rules: &Rules<'_>,
        node: &Node,
        path: &str,
    ) -> Option<Value> {
        match ty {
            FieldType::Text | FieldType::Ref(_) => self.text(rules, node, path).map(Value::Text),
            FieldType::Int => self.int(rules, node, path).map(Value::Int),
            FieldType::Number => self.number(rules, node, path).map(Value::Number),
            FieldType::Bool => match &node.value {
                Raw::Bool(flag) => Some(Value::Bool(*flag)),
                other => self.mismatch(path, node.span, "true or false", other),
            },
            FieldType::Enum(allowed) => match &node.value {
                Raw::Text(text) if allowed.iter().any(|candidate| candidate == text) => {
                    Some(Value::Text(text.clone()))
                }
                Raw::Text(text) => {
                    self.error(
                        DiagnosticCode::Value,
                        path,
                        node.span,
                        format!("`{text}` is not one of: {}", allowed.join(", ")),
                    );
                    None
                }
                other => self.mismatch(path, node.span, "one of the listed values", other),
            },
            FieldType::List(item) => self.list(item, node, path, false),
            FieldType::Set(item) => self.list(item, node, path, true),
            FieldType::Map(item) => self.map(item, node, path),
            FieldType::Struct => self.structure(field?, node, path),
            FieldType::Union { tag } => self.union(tag, field?, node, path),
            FieldType::Env => self.env(node, path),
            FieldType::File => self.file_source(node, path).map(Value::Source),
            FieldType::Selector(kind) => self.selector(kind, node, path),
            FieldType::Blob(_) => self.blob(node, path),
            FieldType::Shared(name) => {
                self.error(
                    DiagnosticCode::Type,
                    path,
                    node.span,
                    format!("shared type `{name}` is not supported yet"),
                );
                None
            }
        }
    }

    fn mismatch(&mut self, path: &str, span: Span, expected: &str, found: &Raw) -> Option<Value> {
        self.error(
            DiagnosticCode::Type,
            path,
            span,
            format!("expected {expected}, found {}", found.describe()),
        );
        None
    }

    fn text(&mut self, rules: &Rules<'_>, node: &Node, path: &str) -> Option<String> {
        let Raw::Text(text) = &node.value else {
            self.mismatch(
                path,
                node.span,
                "text (quote it if it looks like a number)",
                &node.value,
            );
            return None;
        };
        if let Some(min) = rules.min_len
            && (text.chars().count() as u64) < min
        {
            self.error(
                DiagnosticCode::Value,
                path,
                node.span,
                format!("must have at least {min} character(s)"),
            );
            return None;
        }
        if let Some(pattern) = rules.pattern {
            match regex_matches(pattern, text) {
                Some(true) => {}
                Some(false) => {
                    self.error(
                        DiagnosticCode::Value,
                        path,
                        node.span,
                        format!("must match `{pattern}`"),
                    );
                    return None;
                }
                None => {
                    self.error(
                        DiagnosticCode::Value,
                        path,
                        node.span,
                        format!("the spec's pattern `{pattern}` is not a valid expression"),
                    );
                    return None;
                }
            }
        }

        Some(text.clone())
    }

    fn int(&mut self, rules: &Rules<'_>, node: &Node, path: &str) -> Option<i64> {
        let Raw::Int(number) = &node.value else {
            self.mismatch(path, node.span, "an integer", &node.value);
            return None;
        };
        self.in_range(*number as f64, rules, node.span, path)
            .then_some(*number)
    }

    fn number(&mut self, rules: &Rules<'_>, node: &Node, path: &str) -> Option<f64> {
        let number = match &node.value {
            Raw::Int(number) => *number as f64,
            Raw::Float(number) if number.is_finite() => *number,
            other => {
                self.mismatch(path, node.span, "a finite number", other);
                return None;
            }
        };
        self.in_range(number, rules, node.span, path)
            .then_some(number)
    }

    fn in_range(&mut self, number: f64, rules: &Rules<'_>, span: Span, path: &str) -> bool {
        if let Some(min) = rules.min
            && number < min
        {
            self.error(
                DiagnosticCode::Value,
                path,
                span,
                format!("must be at least {min}"),
            );
            return false;
        }
        if let Some(max) = rules.max
            && number > max
        {
            self.error(
                DiagnosticCode::Value,
                path,
                span,
                format!("must be at most {max}"),
            );
            return false;
        }

        true
    }

    fn list(&mut self, item: &FieldType, node: &Node, path: &str, set: bool) -> Option<Value> {
        let Raw::Seq(nodes) = &node.value else {
            return self.mismatch(path, node.span, "a list", &node.value);
        };
        let mut values: Vec<Value> = Vec::new();
        let mut ok = true;
        for (index, element) in nodes.iter().enumerate() {
            let element_path = format!("{path}[{index}]");
            match self.typed(item, None, &Rules::default(), element, &element_path) {
                Some(value) => {
                    if set && values.contains(&value) {
                        self.error(
                            DiagnosticCode::Duplicate,
                            &element_path,
                            element.span,
                            "a set holds each value once",
                        );
                        ok = false;
                    }
                    values.push(value);
                }
                None => ok = false,
            }
        }
        if set {
            values.sort_by_key(|value| format!("{value:?}"));
        }

        ok.then_some(Value::List(values))
    }

    fn map(&mut self, item: &FieldType, node: &Node, path: &str) -> Option<Value> {
        let Raw::Map(entries) = &node.value else {
            return self.mismatch(path, node.span, "a mapping", &node.value);
        };
        let mut values = BTreeMap::new();
        let mut ok = true;
        for (key, entry) in entries {
            let entry_path = format!("{path}.{}", key.name);
            if key.name.is_empty() || key.name.len() > MAX_NAME_LEN || key.name.trim() != key.name {
                self.error(
                    DiagnosticCode::Key,
                    &entry_path,
                    key.span,
                    "a key is non-empty text without surrounding whitespace",
                );
                ok = false;
                continue;
            }
            match self.typed(item, None, &Rules::default(), entry, &entry_path) {
                Some(value) => {
                    values.insert(key.name.clone(), value);
                }
                None => ok = false,
            }
        }

        ok.then_some(Value::Map(values))
    }

    fn structure(&mut self, field: &SpecField, node: &Node, path: &str) -> Option<Value> {
        let Raw::Map(entries) = &node.value else {
            return self.mismatch(path, node.span, "a mapping", &node.value);
        };
        let mut members = BTreeMap::new();
        let mut ok = true;
        for (key, entry) in entries {
            let member_path = format!("{path}.{}", key.name);
            let Some(member) = field.members.get(&key.name) else {
                let valid: Vec<&str> = field.members.keys().map(String::as_str).collect();
                self.unknown(&member_path, key.span, path, &key.name, &valid);
                ok = false;
                continue;
            };
            match self.field(member, entry, &member_path) {
                Some(value) => {
                    members.insert(key.name.clone(), value);
                }
                None => ok = false,
            }
        }

        ok.then_some(Value::Map(members))
    }

    fn union(&mut self, tag: &str, field: &SpecField, node: &Node, path: &str) -> Option<Value> {
        let Raw::Map(entries) = &node.value else {
            return self.mismatch(path, node.span, "a mapping", &node.value);
        };
        let Some((_, tag_node)) = entries.iter().find(|(key, _)| key.name == tag) else {
            let arms: Vec<&str> = field.arms.keys().map(String::as_str).collect();
            self.error(
                DiagnosticCode::Type,
                path,
                node.span,
                format!("needs `{tag}`: one of {}", arms.join(", ")),
            );
            return None;
        };
        let Raw::Text(arm_name) = &tag_node.value else {
            return self.mismatch(
                &format!("{path}.{tag}"),
                tag_node.span,
                "text",
                &tag_node.value,
            );
        };
        let Some(arm) = field.arms.get(arm_name) else {
            let arms: Vec<&str> = field.arms.keys().map(String::as_str).collect();
            self.error(
                DiagnosticCode::Value,
                &format!("{path}.{tag}"),
                tag_node.span,
                format!("`{arm_name}` is not one of: {}", arms.join(", ")),
            );
            return None;
        };
        let mut fields = BTreeMap::new();
        let mut ok = true;
        for (key, entry) in entries {
            if key.name == tag {
                continue;
            }
            let member_path = format!("{path}.{}", key.name);
            let Some(member) = arm.get(&key.name) else {
                let valid: Vec<&str> = arm.keys().map(String::as_str).collect();
                self.unknown(
                    &member_path,
                    key.span,
                    &format!("{path} ({arm_name})"),
                    &key.name,
                    &valid,
                );
                ok = false;
                continue;
            };
            match self.field(member, entry, &member_path) {
                Some(value) => {
                    fields.insert(key.name.clone(), value);
                }
                None => ok = false,
            }
        }

        ok.then(|| Value::Union {
            tag: arm_name.clone(),
            fields,
        })
    }

    fn env(&mut self, node: &Node, path: &str) -> Option<Value> {
        let Raw::Map(entries) = &node.value else {
            return self.mismatch(path, node.span, "a mapping of variable names", &node.value);
        };
        let mut variables = BTreeMap::new();
        let mut ok = true;
        for (key, entry) in entries {
            let entry_path = format!("{path}.{}", key.name);
            if !valid_env_name(&key.name) {
                self.error(
                    DiagnosticCode::Key,
                    &entry_path,
                    key.span,
                    "a variable name is letters, digits, and underscores, and does not start with a digit",
                );
                ok = false;
                continue;
            }
            match self.env_value(entry, &entry_path) {
                Some(value) => {
                    variables.insert(key.name.clone(), value);
                }
                None => ok = false,
            }
        }

        ok.then_some(Value::Env(variables))
    }

    fn env_value(&mut self, node: &Node, path: &str) -> Option<EnvValue> {
        let expected = "text, { value: ... }, { secret: <source> }, or { vault: ... }";
        match &node.value {
            Raw::Text(text) => Some(EnvValue::Public(text.clone())),
            Raw::Map(entries) if entries.len() == 1 => {
                let (key, value) = &entries[0];
                match key.name.as_str() {
                    "value" => match &value.value {
                        Raw::Text(text) => Some(EnvValue::Public(text.clone())),
                        other => {
                            self.mismatch(
                                path,
                                value.span,
                                "text (quote it if it looks like a number)",
                                other,
                            );
                            None
                        }
                    },
                    "secret" => self.source(value, path, true).map(EnvValue::Secret),
                    "vault" => self.vault(value, path).map(EnvValue::Secret),
                    _ => {
                        self.error(
                            DiagnosticCode::Type,
                            path,
                            node.span,
                            format!("expected {expected}"),
                        );
                        None
                    }
                }
            }
            other => {
                self.mismatch(path, node.span, expected, other);
                None
            }
        }
    }

    fn selector(&mut self, kind: &str, node: &Node, path: &str) -> Option<Value> {
        let Raw::Map(entries) = &node.value else {
            return self.mismatch(path, node.span, "{ name: ... }", &node.value);
        };
        let [(key, value)] = entries.as_slice() else {
            self.error(
                DiagnosticCode::Type,
                path,
                node.span,
                "a selector has exactly one key: `name`, or `local` for a server",
            );
            return None;
        };
        match (key.name.as_str(), &value.value) {
            ("name", Raw::Text(name)) if valid_external_name(name) => {
                Some(Value::Selector(Selector::Name(name.clone())))
            }
            ("name", Raw::Text(_)) => {
                self.error(
                    DiagnosticCode::Value,
                    path,
                    value.span,
                    "a remote name is non-empty, has no surrounding whitespace or control characters, and is at most 256 bytes",
                );
                None
            }
            ("local", Raw::Bool(true)) if kind == "server" => {
                Some(Value::Selector(Selector::Local))
            }
            ("local", _) => {
                self.error(
                    DiagnosticCode::Value,
                    path,
                    value.span,
                    "`local: true` selects the Dokploy host and is valid for servers only",
                );
                None
            }
            (other, _) => {
                self.error(
                    DiagnosticCode::UnknownField,
                    path,
                    key.span,
                    format!("a selector has `name`, not `{other}`"),
                );
                None
            }
        }
    }

    fn blob(&mut self, node: &Node, path: &str) -> Option<Value> {
        match to_json(&node.value) {
            Some(json) => Some(Value::Blob(json)),
            None => {
                self.error(
                    DiagnosticCode::Value,
                    path,
                    node.span,
                    "a blob holds JSON values; a non-finite number is not one",
                );
                None
            }
        }
    }

    // ----- sources --------------------------------------------------------

    fn source(&mut self, node: &Node, path: &str, vault_allowed: bool) -> Option<Source> {
        let Raw::Map(entries) = &node.value else {
            self.error(
                DiagnosticCode::SourceRequired,
                path,
                node.span,
                "a secret is written as a source: { env: NAME }, { file: path }, or { vault: { provider, secret } }; never as a literal",
            );
            return None;
        };
        let [(key, value)] = entries.as_slice() else {
            self.error(
                DiagnosticCode::Source,
                path,
                node.span,
                "a source has exactly one key: `env`, `file`, or `vault`",
            );
            return None;
        };
        match key.name.as_str() {
            "env" => match &value.value {
                Raw::Text(name) if valid_env_name(name) => Some(Source::Env(name.clone())),
                _ => {
                    self.error(
                        DiagnosticCode::Source,
                        path,
                        value.span,
                        "`env` names an environment variable: letters, digits, and underscores",
                    );
                    None
                }
            },
            "file" => self.file_path(value, path).map(Source::File),
            "vault" if vault_allowed => self.vault(value, path),
            other => {
                self.error(
                    DiagnosticCode::Source,
                    path,
                    key.span,
                    format!("`{other}` is not a source; use `env`, `file`, or `vault`"),
                );
                None
            }
        }
    }

    fn file_source(&mut self, node: &Node, path: &str) -> Option<Source> {
        let Raw::Map(entries) = &node.value else {
            self.error(
                DiagnosticCode::SourceRequired,
                path,
                node.span,
                "file content is written as { file: path }, never inline",
            );
            return None;
        };
        match entries.as_slice() {
            [(key, value)] if key.name == "file" => self.file_path(value, path).map(Source::File),
            _ => {
                self.error(
                    DiagnosticCode::Source,
                    path,
                    node.span,
                    "file content is exactly { file: path }",
                );
                None
            }
        }
    }

    fn file_path(&mut self, node: &Node, path: &str) -> Option<String> {
        match &node.value {
            Raw::Text(text) if valid_relative_path(text) => Some(text.clone()),
            _ => {
                self.error(
                    DiagnosticCode::Source,
                    path,
                    node.span,
                    "a file path is relative to the workspace, without `..`, a leading `/`, or empty segments",
                );
                None
            }
        }
    }

    fn vault(&mut self, node: &Node, path: &str) -> Option<Source> {
        let Raw::Map(entries) = &node.value else {
            self.error(
                DiagnosticCode::Source,
                path,
                node.span,
                "a vault source is { provider: ..., secret: ... }",
            );
            return None;
        };
        let mut provider = None;
        let mut secret = None;
        for (key, value) in entries {
            let text = match &value.value {
                Raw::Text(text) if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            };
            match key.name.as_str() {
                "provider" => provider = text,
                "secret" => secret = text,
                other => {
                    self.error(
                        DiagnosticCode::Source,
                        path,
                        key.span,
                        format!("a vault source has `provider` and `secret`, not `{other}`"),
                    );
                    return None;
                }
            }
        }
        match (provider, secret) {
            (Some(provider), Some(secret)) => Some(Source::Vault { provider, secret }),
            _ => {
                self.error(
                    DiagnosticCode::Source,
                    path,
                    node.span,
                    "a vault source needs a non-empty `provider` and `secret`",
                );
                None
            }
        }
    }
}

fn valid_key(text: &str) -> bool {
    let mut characters = text.chars();
    text.len() <= MAX_KEY_LEN
        && characters.next().is_some_and(|c| c.is_ascii_lowercase())
        && characters
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

pub(crate) fn valid_env_name(text: &str) -> bool {
    let mut characters = text.chars();
    text.len() <= MAX_NAME_LEN
        && characters
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) fn valid_external_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_NAME_LEN
        && text.trim() == text
        && !text.chars().any(char::is_control)
}

pub(crate) fn valid_relative_path(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_PATH_LEN
        && !text.starts_with('/')
        && !text.contains(['\0', '\\'])
        && text
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn regex_matches(pattern: &str, text: &str) -> Option<bool> {
    regex::Regex::new(pattern)
        .ok()
        .map(|regex| regex.is_match(text))
}

fn to_json(raw: &Raw) -> Option<serde_json::Value> {
    Some(match raw {
        Raw::Null => serde_json::Value::Null,
        Raw::Bool(flag) => serde_json::Value::Bool(*flag),
        Raw::Int(number) => serde_json::Value::from(*number),
        Raw::Float(number) => serde_json::Number::from_f64(*number)?.into(),
        Raw::Text(text) => serde_json::Value::String(text.clone()),
        Raw::Seq(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| to_json(&item.value))
                .collect::<Option<_>>()?,
        ),
        Raw::Map(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, value)| Some((key.name.clone(), to_json(&value.value)?)))
                .collect::<Option<_>>()?,
        ),
    })
}
