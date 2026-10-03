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
    /// The regular expression a text value must match, when the spec gives one.
    pub pattern: Option<String>,
    /// For a member of a union, the arm it belongs to (`github` for `source.github.owner`).
    pub arm: Option<String>,
    /// Whether this is the tag of a union (`source.type`).
    pub union_tag: bool,
    /// For a member of a union, the response key that holds the union's tag, which says whether
    /// the arm of the member is the one Dokploy holds.
    pub tag_api: Option<String>,
    /// What to send when a write needs the property and nothing else supplies it.
    pub fallback: Option<serde_json::Value>,
    /// The response key or JSON pointer the value is read from: the field's `api`, or its own
    /// name. A struct member is a key of its own, not nested in the struct, and an entry of a
    /// keyed collection has its root's.
    pub api: String,
}

impl PropertyInfo {
    /// The key the value is sent under in a request body: the first segment of [`Self::api`].
    #[must_use]
    pub fn request_key(&self) -> &str {
        let trimmed = self.api.strip_prefix('/').unwrap_or(&self.api);
        trimmed.split('/').next().unwrap_or(trimmed)
    }

    /// The name of the document field this property belongs to: the first segment of its path.
    #[must_use]
    pub fn field_name(&self) -> &str {
        self.path.split('.').next().unwrap_or(&self.path)
    }
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
            && self.arm.is_none()
            && !self.union_tag
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
    /// The path names a union without its tag or one of its members.
    #[error("`{prefix}` is a union; name its tag, or a member as `{prefix}.<arm>.<member>`")]
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
    (matches!(parsed, FieldType::Env | FieldType::Map(_)) || parsed.selector_set_kind().is_some())
        && granularity == Some(Granularity::Key)
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
    name: &str,
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
        pattern: field.pattern.clone(),
        arm: None,
        union_tag: false,
        tag_api: None,
        fallback: field.fallback.clone(),
        api: field.api.clone().unwrap_or_else(|| name.to_owned()),
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
        if matches!(parsed, FieldType::Union { .. }) {
            return Err(PathError::UnionMember { prefix });
        }
        let name = prefix.rsplit('.').next().unwrap_or(&prefix).to_owned();
        let mut info = base(spec, prefix, &name, field, parsed, inherited);
        if is_collection(parsed, granularity) {
            info.shape = PathShape::CollectionRoot;
            info.nullable = true;
        }
        return Ok(info);
    };

    if is_collection(parsed, granularity) {
        validate_key(parsed, rest, full)?;
        // The entry of a map of sets is a member of one of the sets: `service.network`.
        let element = match parsed {
            FieldType::Map(inner) if parsed.is_map_of_selector_sets() => match &**inner {
                FieldType::Set(item) => (**item).clone(),
                other => other.clone(),
            },
            FieldType::Map(inner) | FieldType::Set(inner) => (**inner).clone(),
            _ => FieldType::Env,
        };
        let name = prefix.rsplit('.').next().unwrap_or(&prefix).to_owned();
        let mut info = base(spec, full.to_owned(), &name, field, &element, inherited);
        info.shape = PathShape::CollectionEntry;
        info.nullable = true;
        info.selector = selector_of(&element);
        info.root = Some(prefix);
        // A collection's own rules describe the collection, not its entries.
        info.rules = ValueRules::default();
        info.has_default = false;
        // An entry of a map of sets is `key.member`: the key says which element the read and the
        // write address, so it replaces the placeholder in the pointer.
        if parsed.is_map_of_selector_sets() {
            let Some((key, member)) = rest.split_once('.') else {
                return Err(PathError::InvalidKey {
                    path: full.to_owned(),
                    reason: "an entry names a key and a member, as `key.member`",
                });
            };
            if key.is_empty() || member.is_empty() {
                return Err(PathError::InvalidKey {
                    path: full.to_owned(),
                    reason: "an entry names a key and a member, as `key.member`",
                });
            }
            info.api = info.api.replace("$key", key);
        }
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
    if let FieldType::Union { tag } = parsed {
        return resolve_union(spec, &prefix, field, tag, rest, full, inherited);
    }
    Err(PathError::NotComposite { prefix })
}

/// A path below a union: its tag (`source.type`) or a member of one arm (`source.github.owner`).
fn resolve_union(
    spec: &KindSpec,
    prefix: &str,
    field: &crate::model::Field,
    tag: &str,
    rest: &str,
    full: &str,
    inherited: Option<Mutability>,
) -> Result<PropertyInfo, PathError> {
    let name = prefix.rsplit('.').next().unwrap_or(prefix);
    if rest == tag {
        return Ok(union_tag_info(spec, prefix, name, field, tag, inherited));
    }
    let Some((arm, member)) = rest.split_once('.') else {
        return Err(PathError::UnknownMember {
            prefix: prefix.to_owned(),
            member: rest.to_owned(),
        });
    };
    let members = field
        .arms
        .get(arm)
        .ok_or_else(|| PathError::UnknownMember {
            prefix: prefix.to_owned(),
            member: arm.to_owned(),
        })?;
    let member_field = members
        .get(member)
        .ok_or_else(|| PathError::UnknownMember {
            prefix: format!("{prefix}.{arm}"),
            member: member.to_owned(),
        })?;
    let member_type =
        parse_type(&member_field.ty).map_err(|_| PathError::Malformed(full.to_owned()))?;
    Ok(union_member_info(
        spec,
        prefix,
        (arm, field.api.as_deref().unwrap_or(name)),
        member,
        member_field,
        &member_type,
        inherited,
    ))
}

fn union_tag_info(
    spec: &KindSpec,
    prefix: &str,
    name: &str,
    field: &crate::model::Field,
    tag: &str,
    inherited: Option<Mutability>,
) -> PropertyInfo {
    let arms: Vec<String> = field.arms.keys().cloned().collect();
    let mut info = base(
        spec,
        format!("{prefix}.{tag}"),
        name,
        field,
        &FieldType::Enum(arms),
        inherited,
    );
    info.union_tag = true;
    info.nullable = false;
    info.has_default = false;
    info.rules = ValueRules::default();
    info.fallback = None;

    info
}

fn union_member_info(
    spec: &KindSpec,
    prefix: &str,
    (arm, tag_api): (&str, &str),
    member: &str,
    member_field: &crate::model::Field,
    member_type: &FieldType,
    inherited: Option<Mutability>,
) -> PropertyInfo {
    let mut info = base(
        spec,
        format!("{prefix}.{arm}.{member}"),
        member,
        member_field,
        member_type,
        inherited,
    );
    info.arm = Some(arm.to_owned());
    info.tag_api = Some(tag_api.to_owned());

    info
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
    if let FieldType::Union { tag } = parsed {
        let effective = match (inherited, field.mutability) {
            (Some(parent), Mutability::InPlace) => parent,
            (_, own) => own,
        };
        let name = path.rsplit('.').next().unwrap_or(path);
        found.push(union_tag_info(spec, path, name, field, tag, inherited));
        for (arm, members) in &field.arms {
            for (member, member_field) in members {
                if let Ok(member_type) = parse_type(&member_field.ty) {
                    found.push(union_member_info(
                        spec,
                        path,
                        (arm, field.api.as_deref().unwrap_or(name)),
                        member,
                        member_field,
                        &member_type,
                        Some(effective),
                    ));
                }
            }
        }
        return;
    }
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
    let name = path.rsplit('.').next().unwrap_or(path);
    let mut info = base(spec, path.to_owned(), name, field, parsed, inherited);
    if is_collection(parsed, field.granularity) {
        info.shape = PathShape::CollectionRoot;
        info.nullable = true;
    }
    found.push(info);
}
