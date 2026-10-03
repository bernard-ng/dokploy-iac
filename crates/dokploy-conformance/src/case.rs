//! A kind spec turned into a test case: sample values for its fields, the write group of each,
//! and documents that carry them.

use std::collections::BTreeMap;

use dokploy_spec::{
    FieldType, KindSpec, Mutability, PathShape, Scope, SpecRegistry, ValueClass, WriteGroup,
};
use dokploy_state::{DocumentId, ResourceName};
use serde_json::{Value as Json, json};

/// A value a document gives a field.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Val {
    /// A plain value.
    Json(Json),
    /// A secret: the document names an environment variable, the world sets it to this.
    Secret(String),
    /// A resource outside the document, named by the document and held by id at Dokploy. The
    /// world seeds it with this id.
    Selector {
        kind: String,
        name: String,
        id: String,
    },
    /// An environment block: variables whose values are secrets read from the environment.
    Environment {
        /// The request and response key of the block.
        wire: String,
        /// Variable name and secret value, in name order.
        variables: Vec<(String, String)>,
    },
}

pub(crate) type Values = BTreeMap<String, Val>;

/// How one field behaves in a scenario.
#[derive(Clone, Debug)]
pub(crate) struct FieldCase {
    pub(crate) name: String,
    /// Where the document writes it: the name, without the arm of a union member
    /// (`source.owner` for `source.github.owner`).
    pub(crate) doc: String,
    /// The request and response key.
    pub(crate) wire: String,
    pub(crate) mutability: Mutability,
    pub(crate) secret: bool,
    /// Its value is a file's content, written `{ file: path }`, not an environment variable.
    pub(crate) content: bool,
    pub(crate) required: bool,
    pub(crate) default_key: bool,
    /// The value the first document gives, and a different one for changes.
    pub(crate) a: Val,
    pub(crate) b: Option<Val>,
    /// The operation and shape of the write group that carries it.
    pub(crate) group: Option<(String, dokploy_spec::Shape, Vec<String>)>,
}

/// A resource the kind lives under. The suite seeds these as already applied: it exercises
/// the kind, not the kinds above it.
#[derive(Clone, Debug)]
pub(crate) struct Ancestor {
    pub(crate) spec: KindSpec,
    /// The key in the document (the project's slug).
    pub(crate) key: String,
    /// The section of the parent's mapping this kind is written in (`environments`).
    pub(crate) section: String,
    /// The request field that attaches it to its parent (`projectId`).
    pub(crate) attach: Option<String>,
}

/// Everything a scenario needs to know about one kind.
pub(crate) struct Case {
    pub(crate) spec: KindSpec,
    /// The kind it is exercised under, for a kind that can live under several.
    pub(crate) parent_kind: Option<String>,
    pub(crate) key: String,
    pub(crate) fields: Vec<FieldCase>,
    /// The resources above the kind, outermost first; empty for a settings kind.
    pub(crate) ancestors: Vec<Ancestor>,
}

impl Case {
    /// Builds the case, or says why this kind cannot be exercised yet. A kind with several
    /// parents is exercised under one of them, `parent`.
    pub(crate) fn new(
        spec: &KindSpec,
        specs: &SpecRegistry,
        parent: Option<&str>,
    ) -> Result<Self, String> {
        let mut ancestors = Vec::new();
        let mut current = spec;
        let mut chosen = parent;
        while let Some(parent) = chosen
            .map(str::to_owned)
            .or_else(|| current.parents.first().cloned())
        {
            let parent = &parent;
            let parent_spec = specs
                .get(parent)
                .ok_or_else(|| format!("the parent kind `{parent}` has no spec"))?;
            // How this ancestor attaches to *its* parent, and where it is written.
            let attach = parent_spec
                .api
                .create
                .as_ref()
                .and_then(|op| op.parent_id_field(parent_spec.parents.first().map(String::as_str)))
                .map(str::to_owned);
            ancestors.push(Ancestor {
                spec: parent_spec.clone(),
                key: format!("conf-{parent}"),
                section: parent_spec.section.clone(),
                attach,
            });
            current = parent_spec;
            chosen = None;
        }
        ancestors.reverse();
        if spec.scope == Scope::Project && ancestors.is_empty() {
            return Err("a project-scoped kind without a parent is the project itself".to_owned());
        }
        let key = "conf-key".to_owned();
        let mut fields = Vec::new();
        for info in spec.properties() {
            let name = info.path.clone();
            if info.mutability == Mutability::Computed {
                continue;
            }
            // A union is exercised through one arm: the first the spec names.
            let first_arm = |field: &str| {
                spec.fields
                    .get(field)
                    .and_then(|f| f.arms.keys().next().cloned())
            };
            if let Some(arm) = &info.arm
                && first_arm(info.field_name()).as_ref() != Some(arm)
            {
                continue;
            }
            let doc = match &info.arm {
                Some(arm) => name.replacen(&format!(".{arm}."), ".", 1),
                None => name.clone(),
            };
            let secret =
                info.class != ValueClass::Public || info.mutability == Mutability::WriteOnly;
            let group = spec.write.iter().find_map(|group| match group {
                WriteGroup::Op { op, fields, shape }
                    if fields.iter().any(|f| f == info.field_name()) =>
                {
                    Some((op.clone(), *shape, fields.clone()))
                }
                WriteGroup::ByVariant {
                    by_variant,
                    ops,
                    shape,
                } if by_variant == info.field_name() => first_arm(by_variant)
                    .and_then(|arm| ops.get(&arm))
                    .map(|op| (op.clone(), *shape, vec![by_variant.clone()])),
                _ => None,
            });
            let field_case = |a: Val, b: Option<Val>, secret: bool, default_key: bool| FieldCase {
                name: name.clone(),
                doc: doc.clone(),
                wire: info.request_key().to_owned(),
                mutability: info.mutability,
                secret,
                content: info.class == ValueClass::Content,
                required: dokploy_engine::required_at_creation(spec, &info),
                default_key,
                a,
                b,
                group: group.clone(),
            };

            if let Some(target) = info.selector.as_deref() {
                // Only a target the suite can seed: a kind with a spec and a name to match.
                let seedable = specs.get(target).is_some_and(|spec| {
                    spec.parents.is_empty()
                        && spec.api.read.list.is_some()
                        && spec.identity.key.is_some()
                });
                if !seedable {
                    continue;
                }
                let selector = |variant: &str| Val::Selector {
                    kind: target.to_owned(),
                    name: format!("{name}-{variant}"),
                    id: format!("sel-{target}-{name}-{variant}"),
                };
                fields.push(field_case(selector("a"), Some(selector("b")), false, false));
                continue;
            }
            // An environment block: two variables whose values are secret.
            if info.shape == PathShape::CollectionRoot {
                if !matches!(info.ty, FieldType::Env) {
                    continue;
                }
                let block = |variant: &str| Val::Environment {
                    wire: info.request_key().to_owned(),
                    variables: ["LOG_LEVEL", "REGION"]
                        .iter()
                        .map(|key| {
                            (
                                (*key).to_owned(),
                                format!("canary-{name}-{}-{variant}", key.to_ascii_lowercase()),
                            )
                        })
                        .collect(),
                };
                fields.push(field_case(block("a"), Some(block("b")), true, false));
                continue;
            }
            if info.shape != PathShape::Atomic {
                continue;
            }
            let (a, b) = if secret {
                (
                    Some(Val::Secret(format!("canary-{name}-a"))),
                    Some(Val::Secret(format!("canary-{name}-b"))),
                )
            } else {
                (
                    sample(&info, &name, 0).map(Val::Json),
                    sample(&info, &name, 1).map(Val::Json),
                )
            };
            let default_key = spec
                .fields
                .get(&name)
                .is_some_and(|field| field.default.as_deref() == Some("key"));
            // The arm cannot be switched by a second value: its members would have to change too.
            let b = if info.union_tag { None } else { b };
            let Some(a) = a else {
                if dokploy_engine::required_at_creation(spec, &info) {
                    return Err(format!(
                        "cannot make a sample value for required field `{name}`"
                    ));
                }
                continue;
            };
            let b = b.filter(|b| *b != a);
            fields.push(field_case(a, b, secret, default_key));
        }

        Ok(Self {
            parent_kind: ancestors.last().map(|parent| parent.spec.kind.clone()),
            spec: spec.clone(),
            key,
            fields,
            ancestors,
        })
    }

    /// Every key a full write group of these fields may carry: the keys of the fields, of the
    /// members of a struct, and of the members of every arm of a union.
    pub(crate) fn group_wires(&self, fields: &[String]) -> Vec<String> {
        let mut wires = Vec::new();
        for name in fields {
            let Some(field) = self.spec.fields.get(name) else {
                continue;
            };
            wires.push(field.request_name(name).to_owned());
            for (member, member_field) in &field.members {
                wires.push(member_field.request_name(member).to_owned());
            }
            for members in field.arms.values() {
                for (member, member_field) in members {
                    wires.push(member_field.request_name(member).to_owned());
                }
            }
        }

        wires
    }

    /// The cases of one field of a write group: the field itself, or the members of a struct.
    pub(crate) fn cases_of(&self, field: &str) -> Vec<&FieldCase> {
        self.fields
            .iter()
            .filter(|f| f.name == field || f.name.starts_with(&format!("{field}.")))
            .collect()
    }

    /// The response field that names the parent.
    pub(crate) fn parent_id_field(&self) -> Option<String> {
        self.parent_kind
            .as_deref()
            .and_then(|parent| self.spec.parent_column(parent))
            .map(str::to_owned)
    }

    /// The operation whose response holds the kind's collection, and the pointer into it (empty
    /// for a list operation): the list itself, or the parent's direct read.
    pub(crate) fn collection_source(&self) -> Option<(String, String)> {
        let list = self.spec.api.read.list.as_ref()?;
        match (&list.op, &list.embedded_in) {
            (Some(op), _) => Some((op.clone(), String::new())),
            (None, Some(embedded)) => {
                let parent_one = self
                    .ancestors
                    .last()
                    .and_then(|parent| parent.spec.api.read.one.as_ref())
                    .map(|one| one.op.clone());
                embedded
                    .parent_op
                    .clone()
                    .or(parent_one)
                    .map(|op| (op, embedded.pointer.clone()))
            }
            (None, None) => None,
        }
    }

    /// The request fields a created child carries that name its parent: the parent's id, and the
    /// fixed values the spec attaches for this parent kind.
    pub(crate) fn attachments(&self, parent_id: &str) -> Vec<(String, String)> {
        let mut fields: Vec<(String, String)> = self
            .spec
            .api
            .create
            .as_ref()
            .map(|op| {
                op.attachments(self.parent_kind.as_deref())
                    .into_iter()
                    .map(|(field, value)| {
                        let value = if value == "parent_id" {
                            parent_id
                        } else {
                            value
                        };
                        (field.to_owned(), value.to_owned())
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The row also names its parent in a column of its own.
        if let Some(column) = self.parent_id_field() {
            fields.push((column, parent_id.to_owned()));
        }

        fields
    }

    fn is_content(&self, name: &str) -> bool {
        self.fields.iter().any(|f| f.name == name && f.content)
    }

    pub(crate) fn field(&self, name: &str) -> &FieldCase {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .expect("a known field")
    }

    /// Every field with its first value (a field that defaults to the key is left out, so the
    /// default is exercised).
    pub(crate) fn full(&self) -> Values {
        self.fields
            .iter()
            .filter(|f| !f.default_key)
            .map(|f| (f.name.clone(), f.a.clone()))
            .collect()
    }

    /// Only what creating the kind requires.
    pub(crate) fn minimal(&self) -> Values {
        self.fields
            .iter()
            .filter(|f| f.required && !f.default_key)
            .map(|f| (f.name.clone(), f.a.clone()))
            .collect()
    }

    /// `values` with one field changed to its second value.
    pub(crate) fn changed(&self, values: &Values, name: &str) -> Values {
        let mut changed = values.clone();
        changed.insert(
            name.to_owned(),
            self.field(name).b.clone().expect("a second value"),
        );

        changed
    }

    /// The environment that makes the secrets in `values` readable.
    pub(crate) fn environment(&self, values: &Values) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::new();
        for (name, value) in values {
            match value {
                Val::Secret(secret) if self.is_content(name) => {
                    environment.insert(
                        format!("{FILE_PREFIX}{}", content_path(name)),
                        secret.clone(),
                    );
                }
                Val::Secret(secret) => {
                    environment.insert(env_name(name), secret.clone());
                }
                Val::Environment { variables, .. } => {
                    for (key, secret) in variables {
                        environment.insert(env_name(&format!("{name}.{key}")), secret.clone());
                    }
                }
                Val::Json(_) | Val::Selector { .. } => {}
            }
        }

        environment
    }

    /// Which state the kind's documents are tracked in.
    pub(crate) fn document_id(&self) -> DocumentId {
        match self.ancestors.first() {
            Some(root) => {
                DocumentId::Project(ResourceName::new(root.key.clone()).expect("a valid slug"))
            }
            None => DocumentId::Settings,
        }
    }

    /// The document that carries `values`: a settings document for a settings kind, a project
    /// document with the kind nested under its ancestors otherwise.
    pub(crate) fn document(&self, values: &Values, protect: bool) -> String {
        let mut fields = Vec::new();
        // The members of a struct are written together, under the struct.
        let mut members: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (name, value) in values {
            let rendered = match value {
                Val::Json(json) => json.to_string(),
                Val::Secret(_) if self.is_content(name) => {
                    format!("{{ file: {} }}", content_path(name))
                }
                Val::Secret(_) => format!("{{ env: {} }}", env_name(name)),
                Val::Selector { name, .. } => format!("{{ name: {name} }}"),
                Val::Environment { variables, .. } => format!(
                    "{{ {} }}",
                    variables
                        .iter()
                        .map(|(key, _)| format!(
                            "{key}: {{ secret: {{ env: {} }} }}",
                            env_name(&format!("{name}.{key}"))
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            let doc = self
                .fields
                .iter()
                .find(|f| f.name == *name)
                .map_or(name.as_str(), |f| f.doc.as_str());
            match doc.split_once('.') {
                Some((field, member)) => members
                    .entry(field)
                    .or_default()
                    .push(format!("{member}: {rendered}")),
                None => fields.push(format!("{name}: {rendered}")),
            }
        }
        for (field, entries) in members {
            fields.push(format!("{field}: {{ {} }}", entries.join(", ")));
        }
        if protect {
            fields.push("lifecycle: { protect: true }".to_owned());
        }

        self.render(Some(fields))
    }

    /// The document with the kind removed and its ancestors kept.
    pub(crate) fn without_child(&self) -> String {
        self.render(None)
    }

    /// `child` is the kind's field lines, or `None` for a document without it.
    fn render(&self, child: Option<Vec<String>>) -> String {
        let at = |indent: usize, text: String| format!("{}{text}", " ".repeat(indent));
        let mut lines = vec!["version: 2".to_owned()];

        let Some(root) = self.ancestors.first() else {
            match child {
                None => lines.push("settings: {}".to_owned()),
                Some(fields) => {
                    lines.push("settings:".to_owned());
                    lines.push(at(2, format!("{}:", self.spec.section)));
                    lines.push(at(4, format!("{}:", self.key)));
                    push_fields(&mut lines, 6, &fields);
                }
            }

            return lines.join("\n") + "\n";
        };
        lines.push("project:".to_owned());
        lines.push(at(2, format!("slug: {}", root.key)));
        let mut indent = 2;
        for (index, ancestor) in self.ancestors.iter().enumerate().skip(1) {
            lines.push(at(indent, format!("{}:", ancestor.section)));
            let last_without_child = child.is_none() && index + 1 == self.ancestors.len();
            lines.push(at(
                indent + 2,
                format!(
                    "{}:{}",
                    ancestor.key,
                    if last_without_child { " {}" } else { "" }
                ),
            ));
            indent += 4;
        }
        if let Some(fields) = child {
            lines.push(at(indent, format!("{}:", self.spec.section)));
            lines.push(at(indent + 2, format!("{}:", self.key)));
            push_fields(&mut lines, indent + 4, &fields);
        }

        lines.join("\n") + "\n"
    }

    /// What the remote holds for `values` after a create: wire key to value, with the
    /// default-to-key field filled in. Secrets are listed by their canary.
    pub(crate) fn expected_remote(&self, values: &Values) -> BTreeMap<String, Json> {
        let mut expected = BTreeMap::new();
        for field in &self.fields {
            match values.get(&field.name) {
                Some(Val::Json(json)) => {
                    expected.insert(field.wire.clone(), json.clone());
                }
                Some(Val::Secret(secret)) => {
                    expected.insert(field.wire.clone(), json!(secret));
                }
                Some(Val::Selector { id, .. }) => {
                    expected.insert(field.wire.clone(), json!(id));
                }
                Some(Val::Environment { variables, .. }) => {
                    expected.insert(field.wire.clone(), json!(environment_text(variables)));
                }
                None if field.default_key => {
                    expected.insert(field.wire.clone(), json!(self.key));
                }
                None => {}
            }
        }

        expected
    }

    /// The wire values of the fields that identify the kind among its siblings.
    pub(crate) fn collision_object(&self, values: &Values) -> Json {
        let expected = self.expected_remote(values);
        let mut object = serde_json::Map::new();
        for name in &self.spec.identity.collision {
            let field = self.field(name);
            if let Some(value) = expected.get(&field.wire) {
                object.insert(field.wire.clone(), value.clone());
            }
        }

        Json::Object(object)
    }

    /// Every secret value the suite uses, to look for them where they must not appear.
    pub(crate) fn canaries(&self) -> Vec<String> {
        self.fields
            .iter()
            .flat_map(|f| [Some(&f.a), f.b.as_ref()])
            .flatten()
            .flat_map(|value| match value {
                Val::Secret(secret) => vec![secret.clone()],
                Val::Environment { variables, .. } => {
                    variables.iter().map(|(_, secret)| secret.clone()).collect()
                }
                Val::Json(_) | Val::Selector { .. } => Vec::new(),
            })
            .collect()
    }

    /// The selector targets the suite seeds before any scenario: every value of every selector
    /// field, as (kind, id, name).
    pub(crate) fn selector_targets(&self) -> Vec<(String, String, String)> {
        self.fields
            .iter()
            .flat_map(|f| [Some(&f.a), f.b.as_ref()])
            .flatten()
            .filter_map(|value| match value {
                Val::Selector { kind, name, id } => Some((kind.clone(), id.clone(), name.clone())),
                _ => None,
            })
            .collect()
    }
}

/// The text Dokploy holds for an environment block: one `KEY=VALUE` line per variable.
pub(crate) fn environment_text(variables: &[(String, String)]) -> String {
    variables
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn push_fields(lines: &mut Vec<String>, indent: usize, fields: &[String]) {
    if fields.is_empty() {
        if let Some(last) = lines.last_mut() {
            last.push_str(" {}");
        }
        return;
    }
    for field in fields {
        lines.push(format!("{}{field}", " ".repeat(indent)));
    }
}

/// A key of [`Case::environment`] that is not a variable but a file the world writes in the
/// workspace before it compiles the document.
pub(crate) const FILE_PREFIX: &str = "@file/";

/// Where the suite writes the content a field names.
fn content_path(field: &str) -> String {
    format!("conformance/{}.txt", env_name(field).to_ascii_lowercase())
}

/// The directory of those files, which the leak scan skips: the suite wrote the canaries there.
pub(crate) const CONTENT_DIRECTORY: &str = "conformance";

fn env_name(field: &str) -> String {
    let name: String = field
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("CONF_{name}")
}

/// A sample value of a property's type; `variant` 0 and 1 differ when the type allows two.
fn sample(info: &dokploy_spec::PropertyInfo, name: &str, variant: usize) -> Option<Json> {
    if info.pattern.is_some() {
        return None;
    }
    sample_type(&info.ty, info, name, variant)
}

fn sample_type(
    ty: &FieldType,
    info: &dokploy_spec::PropertyInfo,
    name: &str,
    variant: usize,
) -> Option<Json> {
    match ty {
        FieldType::Text => {
            let mut text = format!("{name}-{}", ["a", "b"][variant]);
            while (text.len() as u64) < info.rules.min_len.unwrap_or(0) {
                text.push('x');
            }
            Some(json!(text))
        }
        FieldType::Int => {
            let base = info.rules.min.unwrap_or(0.0).ceil() as i64;
            let value = base + 1 + variant as i64;
            if info.rules.max.is_some_and(|max| value as f64 > max) {
                return None;
            }
            Some(json!(value))
        }
        FieldType::Bool => Some(json!(variant == 0)),
        FieldType::Enum(values) => values
            .get(variant)
            .or_else(|| values.first())
            .map(|v| json!(v)),
        FieldType::List(item) | FieldType::Set(item) => {
            sample_type(item, info, name, variant).map(|value| json!([value]))
        }
        _ => None,
    }
}
