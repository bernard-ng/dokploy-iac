//! A seeded generator of valid documents, driven only by the kind specs.

use std::collections::BTreeMap;

use dokploy_model::{
    Document, EnvValue, Field, Lifecycle, Resource, Root, Sections, Selector, Source, Span, Value,
};
use dokploy_spec::{
    Field as SpecField, FieldType, Granularity, KindSpec, Scope, SpecRegistry, ValueClass,
    parse_type,
};

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn chance(&mut self, numerator: usize, denominator: usize) -> bool {
        self.below(denominator) < numerator
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Strings chosen to break naive YAML emitters.
const TRICKY: &[&str] = &[
    "plain",
    "ghcr.io",
    "true",
    "False",
    "null",
    "~",
    "123",
    "1.5",
    "1e3",
    "0x1F",
    "-",
    "---",
    "a: b",
    "- dash",
    "#hash",
    "key: value # c",
    "[x]",
    "{y}",
    "line\nbreak",
    " leading",
    "trailing ",
    "quote\"s",
    "back\\slash",
    "tab\there",
    "yes",
    "no",
    "on",
    "y",
    "@at",
    "ünïcode",
    "emoji 🙂",
    "'single'",
    "a/b/c",
    "x-",
    "inf",
    ".nan",
];

/// The strings the generator uses to break emitters.
pub fn awkward() -> &'static [&'static str] {
    TRICKY
}

pub fn text(rng: &mut Rng, min_len: u64) -> String {
    loop {
        let candidate = if rng.chance(2, 3) {
            (*rng.pick(TRICKY)).to_owned()
        } else {
            let length = 1 + rng.below(8);
            (0..length)
                .map(|_| char::from(b'a' + u8::try_from(rng.below(26)).unwrap()))
                .collect()
        };
        if candidate.chars().count() as u64 >= min_len {
            return candidate;
        }
    }
}

fn identifier(rng: &mut Rng) -> String {
    format!(
        "{}{}",
        rng.pick(&["a", "b", "k", "svc", "x-y", "z_1"]),
        rng.below(100)
    )
}

fn env_name(rng: &mut Rng) -> String {
    (*rng.pick(&["LOG_LEVEL", "_PRIVATE", "a", "Mixed_Case9", "PORT"])).to_owned()
}

fn source(rng: &mut Rng, vault: bool) -> Source {
    match rng.below(if vault { 3 } else { 2 }) {
        0 => Source::Env(env_name(rng)),
        1 => Source::File(format!("secrets/{}.txt", identifier(rng))),
        _ => Source::Vault {
            provider: text(rng, 1),
            secret: text(rng, 1),
        },
    }
}

fn json(rng: &mut Rng, depth: usize) -> serde_json::Value {
    match rng.below(if depth == 0 { 6 } else { 8 }) {
        0 => serde_json::Value::Null,
        1 => serde_json::Value::Bool(rng.chance(1, 2)),
        2 => serde_json::Value::from(i64::try_from(rng.below(1000)).unwrap() - 500),
        3 => serde_json::Number::from_f64(*rng.pick(&[0.5, 2.0, -3.25, 1e10, 1e-7]))
            .unwrap()
            .into(),
        4 | 5 => serde_json::Value::String(text(rng, 0)),
        6 => serde_json::Value::Array((0..rng.below(3)).map(|_| json(rng, depth - 1)).collect()),
        _ => serde_json::Value::Object(
            (0..rng.below(3))
                .map(|_| (text(rng, 0), json(rng, depth - 1)))
                .collect(),
        ),
    }
}

fn typed(rng: &mut Rng, ty: &FieldType, field: Option<&SpecField>) -> Value {
    match ty {
        FieldType::Text | FieldType::Ref(_) => {
            let min_len = field.and_then(|f| f.min_len).unwrap_or(0);
            if let Some(pattern) = field.and_then(|f| f.pattern.as_deref()) {
                assert_eq!(pattern, "^[A-Z]{3}$", "the generator knows one pattern");
                return Value::Text("ABC".into());
            }
            let min_len = if matches!(ty, FieldType::Ref(_)) {
                min_len.max(1)
            } else {
                min_len
            };
            Value::Text(text(rng, min_len))
        }
        FieldType::Int => {
            let min = field.and_then(|f| f.min).unwrap_or(-50.0) as i64;
            let max = field.and_then(|f| f.max).unwrap_or(5000.0) as i64;
            Value::Int(
                min + i64::try_from(rng.below(usize::try_from(max - min + 1).unwrap())).unwrap(),
            )
        }
        FieldType::Number => {
            let min = field.and_then(|f| f.min).unwrap_or(-10.0);
            let max = field.and_then(|f| f.max).unwrap_or(10.0);
            let candidates: Vec<f64> = [0.0, 0.5, 1.0, 2.0, -3.25]
                .into_iter()
                .filter(|n| *n >= min && *n <= max)
                .collect();
            Value::Number(*rng.pick(&candidates))
        }
        FieldType::Bool => Value::Bool(rng.chance(1, 2)),
        FieldType::Enum(values) => Value::Text(rng.pick(values).clone()),
        FieldType::List(item) => {
            Value::List((0..rng.below(4)).map(|_| typed(rng, item, None)).collect())
        }
        FieldType::Set(item) => {
            let mut values: Vec<Value> = Vec::new();
            for _ in 0..rng.below(4) {
                let value = typed(rng, item, None);
                if !values.contains(&value) {
                    values.push(value);
                }
            }
            values.sort_by_key(|value| format!("{value:?}"));
            Value::List(values)
        }
        FieldType::Map(item) => Value::Map(
            (0..rng.below(4))
                .map(|_| (text(rng, 1).trim().to_owned() + "k", typed(rng, item, None)))
                .collect(),
        ),
        FieldType::Struct => {
            let field = field.expect("a struct has a field");
            let mut members = BTreeMap::new();
            for (name, member) in &field.members {
                if rng.chance(2, 3) {
                    members.insert(name.clone(), value_of(rng, member));
                }
            }
            Value::Map(members)
        }
        FieldType::Union { .. } => {
            let field = field.expect("a union has a field");
            let arms: Vec<&String> = field.arms.keys().collect();
            let tag = (*rng.pick(&arms)).clone();
            let mut fields = BTreeMap::new();
            for (name, member) in &field.arms[&tag] {
                if rng.chance(2, 3) {
                    fields.insert(name.clone(), value_of(rng, member));
                }
            }
            Value::Union { tag, fields }
        }
        FieldType::Env => Value::Env(
            (0..rng.below(4))
                .map(|_| {
                    let value = if rng.chance(1, 2) {
                        EnvValue::Public(text(rng, 0))
                    } else {
                        EnvValue::Secret(source(rng, true))
                    };
                    (env_name(rng), value)
                })
                .collect(),
        ),
        FieldType::File => Value::Source(Source::File(format!("files/{}.conf", identifier(rng)))),
        FieldType::Selector(kind) => {
            if kind == "server" && rng.chance(1, 3) {
                Value::Selector(Selector::Local)
            } else {
                let name: String = text(rng, 1).chars().filter(|c| !c.is_control()).collect();
                Value::Selector(Selector::Name(name.trim().to_owned() + "n"))
            }
        }
        FieldType::Blob(_) => loop {
            // A top-level `null` is a clear, not content (ADR 0004: null means cleared).
            let content = json(rng, 2);
            if !content.is_null() {
                break Value::Blob(content);
            }
        },
        FieldType::Shared(name) => panic!("no shared types are generated: {name}"),
    }
}

fn value_of(rng: &mut Rng, field: &SpecField) -> Value {
    let ty = parse_type(&field.ty).expect("spec types parse");
    let collection = matches!(ty, FieldType::Env | FieldType::Map(_))
        && field.granularity == Some(Granularity::Key);
    if (field.nullable || collection) && rng.chance(1, 5) {
        return Value::Null;
    }
    match field.class {
        ValueClass::Secret if matches!(ty, FieldType::Text) => Value::Source(source(rng, true)),
        ValueClass::Content => {
            Value::Source(Source::File(format!("files/{}.conf", identifier(rng))))
        }
        _ => typed(rng, &ty, Some(field)),
    }
}

fn resource(rng: &mut Rng, registry: &SpecRegistry, spec: &KindSpec, key: String) -> Resource {
    let mut fields = BTreeMap::new();
    for (name, field) in &spec.fields {
        if rng.chance(2, 3) {
            fields.insert(
                name.clone(),
                Field {
                    value: value_of(rng, field),
                    span: Span::default(),
                },
            );
        }
    }
    let properties: Vec<String> = spec.properties().into_iter().map(|p| p.path).collect();
    let mut lifecycle = Lifecycle::default();
    if rng.chance(1, 3) {
        lifecycle.protect = Some(rng.chance(1, 2));
    }
    if rng.chance(1, 3) && !properties.is_empty() {
        lifecycle.ignore_changes = vec![rng.pick(&properties).clone()];
    }
    let depends_on = if rng.chance(1, 4) {
        vec![format!("{}.{}", spec.kind, identifier(rng))]
    } else {
        Vec::new()
    };
    let mut children: Sections = BTreeMap::new();
    for child in &spec.children {
        let child_spec = registry.get(&child.kind).expect("child kinds exist");
        let mut resources = BTreeMap::new();
        for _ in 0..rng.below(3) {
            let child_key = identifier(rng);
            resources.insert(
                child_key.clone(),
                resource(rng, registry, child_spec, child_key),
            );
        }
        if !resources.is_empty() || rng.chance(1, 4) {
            children.insert(child.section.clone(), resources);
        }
    }

    Resource {
        kind: spec.kind.clone(),
        key,
        span: Span::default(),
        fields,
        lifecycle,
        depends_on,
        children,
    }
}

/// A random valid document of one scope.
pub fn document(seed: u64, registry: &SpecRegistry, scope: Scope) -> Document {
    let mut rng = Rng::new(seed);
    let root = match scope {
        Scope::Settings => {
            let mut sections: Sections = BTreeMap::new();
            for spec in registry.roots(Scope::Settings) {
                let mut resources = BTreeMap::new();
                for _ in 0..1 + rng.below(3) {
                    let key = identifier(&mut rng);
                    resources.insert(key.clone(), resource(&mut rng, registry, spec, key));
                }
                if rng.chance(4, 5) {
                    sections.insert(spec.section.clone(), resources);
                }
            }
            Root::Settings(sections)
        }
        Scope::Project => {
            let spec = registry.roots(Scope::Project)[0];
            let key = identifier(&mut rng);
            Root::Project(Box::new(resource(&mut rng, registry, spec, key)))
        }
    };

    Document {
        scope,
        root,
        moves: Vec::new(),
        removed: Vec::new(),
    }
}
