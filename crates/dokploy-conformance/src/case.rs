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
    /// The request and response key.
    pub(crate) wire: String,
    pub(crate) mutability: Mutability,
    pub(crate) secret: bool,
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
    pub(crate) key: String,
    pub(crate) fields: Vec<FieldCase>,
    /// The resources above the kind, outermost first; empty for a settings kind.
    pub(crate) ancestors: Vec<Ancestor>,
}

impl Case {
    /// Builds the case, or says why this kind cannot be exercised yet.
    pub(crate) fn new(spec: &KindSpec, specs: &SpecRegistry) -> Result<Self, String> {
        let mut ancestors = Vec::new();
        let mut current = spec;
        while let Some(parent) = &current.parent {
            let parent_spec = specs
                .get(parent)
                .ok_or_else(|| format!("the parent kind `{parent}` has no spec"))?;
            // How this ancestor attaches to *its* parent, and where it is written.
            let attach = parent_spec
                .api
                .create
                .as_ref()
                .and_then(|op| op.attach.iter().find(|(_, source)| *source == "parent_id"))
                .map(|(field, _)| field.clone());
            ancestors.push(Ancestor {
                spec: parent_spec.clone(),
                key: format!("conf-{parent}"),
                section: parent_spec.section.clone(),
                attach,
            });
            current = parent_spec;
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
            let secret =
                info.class != ValueClass::Public || info.mutability == Mutability::WriteOnly;
            let group = spec.write.iter().find_map(|group| match group {
                WriteGroup::Op { op, fields, shape }
                    if fields.iter().any(|f| f == info.field_name()) =>
                {
                    Some((op.clone(), *shape, fields.clone()))
                }
                _ => None,
            });
            let field_case = |a: Val, b: Option<Val>, secret: bool, default_key: bool| FieldCase {
                name: name.clone(),
                wire: info.request_key().to_owned(),
                mutability: info.mutability,
                secret,
                required: dokploy_engine::required_at_creation(spec, &info),
                default_key,
                a,
                b,
                group: group.clone(),
            };

            if let Some(target) = info.selector.as_deref() {
                // Only a target the suite can seed: a kind with a spec and a name to match.
                let seedable = specs.get(target).is_some_and(|spec| {
                    spec.parent.is_none()
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
            spec: spec.clone(),
            key,
            fields,
            ancestors,
        })
    }

    /// The cases of one field of a write group: the field itself, or the members of a struct.
    pub(crate) fn cases_of(&self, field: &str) -> Vec<&FieldCase> {
        self.fields
            .iter()
            .filter(|f| f.name == field || f.name.starts_with(&format!("{field}.")))
            .collect()
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
            match name.split_once('.') {
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
