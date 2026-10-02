//! Property paths: which dotted paths a kind spec makes legal, and what each means.
//!
//! A *property* is a field at planning granularity (ADR 0004). A scalar, enum, list,
//! set, file, reference, selector, blob, shared-type, or union field is one property
//! named by the field. An `env` or `map` field with `granularity: key` is a
//! collection: its root (`environment`) can only be cleared or declared empty, and
//! each entry (`environment.LOG_LEVEL`) is a property of its own, so a plan names the
//! variables that change and never their values. A `struct` with `granularity: field`
//! has one property per member (`swarm.mode`); otherwise it is one atomic property.
//! The members of a union arm are not separate properties: the union is planned as
//! one value.

use thiserror::Error;

use crate::model::{Granularity, KindSpec, Mutability, ValueClass};
use crate::registry::SpecRegistry;
use crate::types::{FieldType, parse_type};

/// Longest accepted collection key.
const MAX_KEY_LEN: usize = 256;

/// How a property relates to a keyed collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathShape {
    /// One value planned as a unit.
    Atomic,
    /// The root of a keyed collection: owned only to clear it or to declare it empty.
    CollectionRoot,
    /// One entry of a keyed collection.
    CollectionEntry,
}

/// Value constraints a spec attaches to a property.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValueRules {
    /// Minimum text length.
    pub min_len: Option<u64>,
    /// Minimum numeric value.
    pub min: Option<f64>,
    /// Maximum numeric value.
    pub max: Option<f64>,
}

/// Everything the planner needs to know about one legal property path.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyInfo {
    /// The kind whose spec makes the path legal.
    pub kind: String,
    /// The canonical dotted path.
    pub path: String,
    /// Atomic value, collection root, or collection entry.
    pub shape: PathShape,
    /// The type of the value at this path (the element type for an entry).
    pub ty: FieldType,
    /// Public, secret, or content.
    pub class: ValueClass,
    /// How the property changes.
    pub mutability: Mutability,
    /// Whether `null` clears it.
    pub nullable: bool,
    /// The kind this property selects, for a `selector(kind)` property.
    pub selector: Option<String>,
    /// The collection root, for an entry.
    pub root: Option<String>,
    /// Constraints on the value.
    pub rules: ValueRules,
    /// Whether the document supplies a default (`default: key`).
    pub has_default: bool,
}

impl PropertyInfo {
    /// Whether values at this path are never shown, only fingerprinted.
    ///
    /// Secret and content fields and write-only fields qualify, and so does every
    /// `env` entry: environment values are secret by default (ADR 0010).
    #[must_use]
    pub fn is_sensitive(&self) -> bool {
        self.class != ValueClass::Public
            || self.mutability == Mutability::WriteOnly
            || (self.shape == PathShape::CollectionEntry && self.ty == FieldType::Env)
    }

    /// Whether the property is readable but never configurable.
    #[must_use]
    pub fn is_lifecycle_only(&self) -> bool {
        self.mutability == Mutability::Computed
    }

    /// Whether the value names a resource outside the document.
    #[must_use]
    pub fn is_selector(&self) -> bool {
        self.selector.is_some()
    }

    /// Whether the property must be supplied when the resource is created.
    #[must_use]
    pub fn is_required_on_create(&self) -> bool {
        self.shape == PathShape::Atomic
            && !self.nullable
            && !self.has_default
            && matches!(
                self.mutability,
                Mutability::InPlace | Mutability::CreateOnly | Mutability::WriteOnly
            )
    }
}

/// A path the spec does not make legal.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PathError {
    /// No kind has this name.
    #[error("unknown kind `{0}`")]
    UnknownKind(String),
    /// The path is empty or has an empty segment.
    #[error("`{0}` is not a property path")]
    Malformed(String),
    /// The first segment is not a field of the kind.
    #[error("kind `{kind}` has no field `{field}`")]
    UnknownField {
        /// The kind.
        kind: String,
        /// The unknown field.
        field: String,
    },
    /// The path continues below a value that has no sub-properties.
    #[error("`{prefix}` is one value and has no sub-properties")]
    NotComposite {
        /// The path that cannot be descended into.
        prefix: String,
    },
    /// A union arm member was addressed; the union is planned as one value.
    #[error("`{prefix}` is a union; plan it as one value, not by arm member")]
    UnionMember {
        /// The union's path.
        prefix: String,
    },
    /// A struct member does not exist.
    #[error("`{prefix}` has no member `{member}`")]
    UnknownMember {
        /// The struct's path.
        prefix: String,
        /// The unknown member.
        member: String,
    },
    /// A collection key is not acceptable.
    #[error("`{path}`: {reason}")]
    InvalidKey {
        /// The full path.
        path: String,
        /// What is wrong with the key.
        reason: &'static str,
    },
    /// The path names a struct that is planned per member.
    #[error("`{prefix}` is planned per member; name a member")]
    NeedsMember {
        /// The struct's path.
        prefix: String,
    },
}

impl KindSpec {
    /// Resolves a dotted property path against this spec.
    pub fn property(&self, path: &str) -> Result<PropertyInfo, PathError> {
        if path.is_empty() || path.starts_with('.') || path.ends_with('.') || path.contains("..") {
            return Err(PathError::Malformed(path.to_owned()));
        }
        let (field_name, rest) = match path.split_once('.') {
            Some((head, tail)) => (head, Some(tail)),
            None => (path, None),
        };
        let field = self
            .fields
            .get(field_name)
            .ok_or_else(|| PathError::UnknownField {
                kind: self.kind.clone(),
                field: field_name.to_owned(),
            })?;
        let parsed = parse_type(&field.ty).map_err(|_| PathError::Malformed(path.to_owned()))?;
        resolve_inside(
            self,
            field_name.to_owned(),
            field,
            &parsed,
            rest,
            path,
            None,
        )
    }

    /// Every addressable property without a collection key, in path order.
    ///
    /// A keyed collection contributes its root only, since its entries are named by
    /// the document.
    #[must_use]
    pub fn properties(&self) -> Vec<PropertyInfo> {
        let mut found = Vec::new();
        for (name, field) in &self.fields {
            if let Ok(parsed) = parse_type(&field.ty) {
                collect(self, name, field, &parsed, None, &mut found);
            }
        }
        found.sort_by(|left, right| left.path.cmp(&right.path));
        found
    }
}

impl SpecRegistry {
    /// Resolves `path` for `kind`.
    pub fn property(&self, kind: &str, path: &str) -> Result<PropertyInfo, PathError> {
        self.get(kind)
            .ok_or_else(|| PathError::UnknownKind(kind.to_owned()))?
            .property(path)
    }
}

fn is_collection(parsed: &FieldType, granularity: Option<Granularity>) -> bool {
    matches!(parsed, FieldType::Env | FieldType::Map(_)) && granularity == Some(Granularity::Key)
}

fn is_per_member(parsed: &FieldType, granularity: Option<Granularity>) -> bool {
    matches!(parsed, FieldType::Struct) && granularity == Some(Granularity::Field)
}

fn selector_of(parsed: &FieldType) -> Option<String> {
    match parsed {
        FieldType::Selector(kind) => Some(kind.clone()),
        _ => None,
    }
}

fn base(
    spec: &KindSpec,
    path: String,
    field: &crate::model::Field,
    parsed: &FieldType,
    inherited: Option<Mutability>,
) -> PropertyInfo {
    let mutability = match (inherited, field.mutability) {
        (Some(parent), Mutability::InPlace) => parent,
        (_, own) => own,
    };
    PropertyInfo {
        kind: spec.kind.clone(),
        path,
        shape: PathShape::Atomic,
        ty: parsed.clone(),
        class: field.class,
        mutability,
        nullable: field.nullable,
        selector: selector_of(parsed),
        root: None,
        rules: ValueRules {
            min_len: field.min_len,
            min: field.min,
            max: field.max,
        },
        has_default: field.default.is_some(),
    }
}

fn resolve_inside(
    spec: &KindSpec,
    prefix: String,
    field: &crate::model::Field,
    parsed: &FieldType,
    rest: Option<&str>,
    full: &str,
    inherited: Option<Mutability>,
) -> Result<PropertyInfo, PathError> {
    let granularity = field.granularity;
    let Some(rest) = rest else {
        if is_per_member(parsed, granularity) {
            return Err(PathError::NeedsMember { prefix });
        }
        let mut info = base(spec, prefix, field, parsed, inherited);
        if is_collection(parsed, granularity) {
            info.shape = PathShape::CollectionRoot;
            info.nullable = true;
        }
        return Ok(info);
    };

    if is_collection(parsed, granularity) {
        validate_key(parsed, rest, full)?;
        let element = match parsed {
            FieldType::Map(inner) => (**inner).clone(),
            _ => FieldType::Env,
        };
        let mut info = base(spec, full.to_owned(), field, &element, inherited);
        info.shape = PathShape::CollectionEntry;
        info.nullable = true;
        info.selector = selector_of(&element);
        info.root = Some(prefix);
        // A collection's own rules describe the collection, not its entries.
        info.rules = ValueRules::default();
        info.has_default = false;
        return Ok(info);
    }
    if is_per_member(parsed, granularity) {
        let (member_name, below) = match rest.split_once('.') {
            Some((head, tail)) => (head, Some(tail)),
            None => (rest, None),
        };
        let member = field
            .members
            .get(member_name)
            .ok_or_else(|| PathError::UnknownMember {
                prefix: prefix.clone(),
                member: member_name.to_owned(),
            })?;
        let member_type =
            parse_type(&member.ty).map_err(|_| PathError::Malformed(full.to_owned()))?;
        let effective = match (inherited, field.mutability) {
            (Some(parent), Mutability::InPlace) => parent,
            (_, own) => own,
        };
        return resolve_inside(
            spec,
            format!("{prefix}.{member_name}"),
            member,
            &member_type,
            below,
            full,
            Some(effective),
        );
    }
    if matches!(parsed, FieldType::Union { .. }) {
        return Err(PathError::UnionMember { prefix });
    }
    Err(PathError::NotComposite { prefix })
}

fn validate_key(parsed: &FieldType, key: &str, full: &str) -> Result<(), PathError> {
    let invalid = |reason| PathError::InvalidKey {
        path: full.to_owned(),
        reason,
    };
    if key.is_empty() {
        return Err(invalid("the key is empty"));
    }
    if key.len() > MAX_KEY_LEN {
        return Err(invalid("the key is longer than 256 characters"));
    }
    if key.trim() != key || key.chars().any(char::is_control) {
        return Err(invalid(
            "the key has surrounding whitespace or control characters",
        ));
    }
    if matches!(parsed, FieldType::Env) {
        let mut characters = key.chars();
        let first_ok = characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_');
        if !first_ok || !characters.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(invalid(
                "an environment variable name is letters, digits, and underscores, and does not start with a digit",
            ));
        }
    }
    Ok(())
}

fn collect(
    spec: &KindSpec,
    path: &str,
    field: &crate::model::Field,
    parsed: &FieldType,
    inherited: Option<Mutability>,
    found: &mut Vec<PropertyInfo>,
) {
    if is_per_member(parsed, field.granularity) {
        let effective = match (inherited, field.mutability) {
            (Some(parent), Mutability::InPlace) => parent,
            (_, own) => own,
        };
        for (name, member) in &field.members {
            if let Ok(member_type) = parse_type(&member.ty) {
                collect(
                    spec,
                    &format!("{path}.{name}"),
                    member,
                    &member_type,
                    Some(effective),
                    found,
                );
            }
        }
        return;
    }
    let mut info = base(spec, path.to_owned(), field, parsed, inherited);
    if is_collection(parsed, field.granularity) {
        info.shape = PathShape::CollectionRoot;
        info.nullable = true;
    }
    found.push(info);
}
