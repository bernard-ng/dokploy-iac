//! A kind spec turned into a test case: sample values for its fields, the write group of each,
//! and documents that carry them.

use std::collections::BTreeMap;

use dokploy_spec::{
    Field, FieldType, KindSpec, Mutability, PathShape, ValueClass, WriteGroup, parse_type,
};
use serde_json::{Value as Json, json};

/// A value a document gives a field.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Val {
    /// A plain value.
    Json(Json),
    /// A secret: the document names an environment variable, the world sets it to this.
    Secret(String),
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

/// Everything a scenario needs to know about one kind.
pub(crate) struct Case {
    pub(crate) spec: KindSpec,
    pub(crate) key: String,
    pub(crate) fields: Vec<FieldCase>,
}

impl Case {
    /// Builds the case, or says why this kind cannot be exercised yet.
    pub(crate) fn new(spec: &KindSpec) -> Result<Self, String> {
        if spec.parent.is_some() {
            return Err(
                "a nested kind needs its ancestors, which the suite seeds in a later step"
                    .to_owned(),
            );
        }
        let key = "conf-key".to_owned();
        let mut fields = Vec::new();
        for (name, field) in &spec.fields {
            let Ok(info) = spec.property(name) else {
                continue;
            };
            if info.shape != PathShape::Atomic
                || info.is_selector()
                || field.mutability == Mutability::Computed
            {
                continue;
            }
            let Ok(ty) = parse_type(&field.ty) else {
                continue;
            };
            let secret =
                field.class != ValueClass::Public || field.mutability == Mutability::WriteOnly;
            let (a, b) = if secret {
                (
                    Some(Val::Secret(format!("canary-{name}-a"))),
                    Some(Val::Secret(format!("canary-{name}-b"))),
                )
            } else {
                (
                    sample(&ty, field, name, 0).map(Val::Json),
                    sample(&ty, field, name, 1).map(Val::Json),
                )
            };
            let default_key = field.default.as_deref() == Some("key");
            let required = info.is_required_on_create();
            let Some(a) = a else {
                if required {
                    return Err(format!(
                        "cannot make a sample value for required field `{name}`"
                    ));
                }
                continue;
            };
            let b = b.filter(|b| *b != a);
            let group = spec.write.iter().find_map(|group| match group {
                WriteGroup::Op { op, fields, shape } if fields.contains(name) => {
                    Some((op.clone(), *shape, fields.clone()))
                }
                _ => None,
            });
            fields.push(FieldCase {
                name: name.clone(),
                wire: field.request_name(name).to_owned(),
                mutability: field.mutability,
                secret,
                required,
                default_key,
                a,
                b,
                group,
            });
        }

        Ok(Self {
            spec: spec.clone(),
            key,
            fields,
        })
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
        values
            .iter()
            .filter_map(|(name, value)| match value {
                Val::Secret(secret) => Some((env_name(name), secret.clone())),
                Val::Json(_) => None,
            })
            .collect()
    }

    /// The settings document that carries `values`.
    pub(crate) fn document(&self, values: &Values, protect: bool) -> String {
        let mut text = format!(
            "version: 2\nsettings:\n  {}:\n    {}:\n",
            self.spec.section, self.key
        );
        if values.is_empty() && !protect {
            text.push_str("      {}\n");
        }
        for (name, value) in values {
            let rendered = match value {
                Val::Json(json) => json.to_string(),
                Val::Secret(_) => format!("{{ env: {} }}", env_name(name)),
            };
            text.push_str(&format!("      {name}: {rendered}\n"));
        }
        if protect {
            text.push_str("      lifecycle: { protect: true }\n");
        }

        text
    }

    pub(crate) fn empty_document() -> &'static str {
        "version: 2\nsettings: {}\n"
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
            .filter_map(|value| match value {
                Val::Secret(secret) => Some(secret.clone()),
                Val::Json(_) => None,
            })
            .collect()
    }
}

fn env_name(field: &str) -> String {
    format!("CONF_{}", field.to_ascii_uppercase())
}

/// A sample value of a field's type; `variant` 0 and 1 differ when the type allows two.
fn sample(ty: &FieldType, field: &Field, name: &str, variant: usize) -> Option<Json> {
    if field.pattern.is_some() {
        return None;
    }
    match ty {
        FieldType::Text => {
            let mut text = format!("{name}-{}", ["a", "b"][variant]);
            while (text.len() as u64) < field.min_len.unwrap_or(0) {
                text.push('x');
            }
            Some(json!(text))
        }
        FieldType::Int => {
            let base = field.min.unwrap_or(0.0).ceil() as i64;
            let value = base + 1 + variant as i64;
            if field.max.is_some_and(|max| value as f64 > max) {
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
            sample(item, field, name, variant).map(|value| json!([value]))
        }
        _ => None,
    }
}
