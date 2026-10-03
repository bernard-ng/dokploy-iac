//! Structural lint of one spec. Cross-spec rules live in the registry.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::Issue;
use crate::model::{
    Api, Authority, CreateIdentity, Field, KindClass, KindSpec, Mutability, ValueClass, WriteGroup,
};
use crate::types::{FieldType, parse_type};

fn is_snake(text: &str) -> bool {
    let mut characters = text.chars();
    characters.next().is_some_and(|c| c.is_ascii_lowercase())
        && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_operation(text: &str) -> bool {
    let Some((resource, operation)) = text.split_once('.') else {
        return false;
    };
    let word = |part: &str| {
        let mut characters = part.chars();
        characters.next().is_some_and(|c| c.is_ascii_alphabetic())
            && characters.all(|c| c.is_ascii_alphanumeric())
    };
    word(resource) && word(operation)
}

fn is_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

struct Context<'a> {
    subject: &'a str,
    issues: Vec<Issue>,
}

impl Context<'_> {
    fn add(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(Issue::new(self.subject, path, message));
    }
}

/// Lints one spec. `shared_types` are the names defined in `specs/types.yaml`.
#[must_use]
pub fn validate_spec(spec: &KindSpec, shared_types: &BTreeSet<String>) -> Vec<Issue> {
    let mut context = Context {
        subject: &spec.kind,
        issues: Vec::new(),
    };

    if !is_snake(&spec.kind) {
        context.add("kind", "must be lower snake case, starting with a letter");
    }
    if !is_snake(&spec.section) {
        context.add(
            "section",
            "must be lower snake case, starting with a letter",
        );
    }
    if spec.title.trim().is_empty() {
        context.add("title", "must not be empty");
    }
    if let Some(since) = &spec.since
        && !is_version(since)
    {
        context.add("since", "must be a version such as 0.30.6");
    }

    check_identity(&mut context, spec);
    check_api(&mut context, spec);
    for (name, field) in &spec.fields {
        check_field(
            &mut context,
            &format!("fields.{name}"),
            name,
            field,
            shared_types,
            true,
        );
    }
    check_write_groups(&mut context, spec);
    check_children(&mut context, spec);
    if let Some(hook) = &spec.hook
        && (hook.name.trim().is_empty() || hook.reason.trim().is_empty())
    {
        context.add("hook", "a hook needs a name and a written reason");
    }

    context.issues
}

fn check_identity(context: &mut Context<'_>, spec: &KindSpec) {
    let identity = &spec.identity;
    if !identity.address.contains("{key}") {
        context.add("identity.address", "must contain the {key} placeholder");
    }
    if let Some(key) = &identity.key
        && !spec.fields.contains_key(key)
    {
        context.add("identity.key", format!("`{key}` is not a field"));
    }
    for name in &identity.collision {
        if !spec.fields.contains_key(name) {
            context.add("identity.collision", format!("`{name}` is not a field"));
        }
    }
}

fn check_operation(context: &mut Context<'_>, path: &str, op: &str) {
    if !is_operation(op) {
        context.add(path, format!("`{op}` is not a `resource.operation` id"));
    }
}

fn check_api(context: &mut Context<'_>, spec: &KindSpec) {
    let api: &Api = &spec.api;
    if api.id.trim().is_empty() {
        context.add("api.id", "must name the id field");
    }
    for (path, operation) in [
        ("api.create.op", api.create.as_ref().map(|o| &o.op)),
        ("api.update.op", api.update.as_ref().map(|o| &o.op)),
        ("api.remove.op", api.remove.as_ref().map(|o| &o.op)),
        ("api.deploy.op", api.deploy.as_ref().map(|o| &o.op)),
        ("api.read.one.op", api.read.one.as_ref().map(|o| &o.op)),
    ] {
        if let Some(op) = operation {
            check_operation(context, path, op);
        }
    }
    match spec.class {
        KindClass::Managed => {
            if api.create.is_none() {
                context.add("api.create", "a managed kind needs a create operation");
            }
            if api.remove.is_none() {
                context.add("api.remove", "a managed kind needs a remove operation");
            }
            if api.create.is_some() && api.create_identity.is_none() {
                context.add(
                    "api.create_identity",
                    "say how the new identity is learned (from_response, diff_collection, or none)",
                );
            }
        }
        KindClass::AdoptOnly => {
            if api.create.is_some() || api.remove.is_some() {
                context.add(
                    "api",
                    "an adopt-only kind has no create or remove operation",
                );
            }
            if api.update.is_none() {
                context.add("api.update", "an adopt-only kind needs an update operation");
            }
        }
    }
    if api.read.list.is_none() && api.read.one.is_none() {
        context.add("api.read", "a kind needs at least one read");
    }
    for parent in api.parent_field.keys() {
        if !spec.parents.contains(parent) {
            context.add(
                "api.parent_field",
                format!("`{parent}` is not a parent kind of this kind"),
            );
        }
    }
    if let Some(list) = &api.read.list {
        match (&list.op, &list.embedded_in) {
            (None, None) => context.add("api.read.list", "needs `op` or `embedded_in`"),
            (Some(_), Some(_)) => {
                context.add("api.read.list", "`op` and `embedded_in` are exclusive");
            }
            (Some(op), None) => check_operation(context, "api.read.list.op", op),
            (None, Some(embedded)) => {
                if let Some(parent_op) = &embedded.parent_op {
                    check_operation(context, "api.read.list.embedded_in.parent_op", parent_op);
                }
                if spec.parents.is_empty() {
                    context.add(
                        "api.read.list.embedded_in",
                        "needs a parent kind to embed in",
                    );
                }
                if !embedded.pointer.starts_with('/') {
                    context.add(
                        "api.read.list.embedded_in.pointer",
                        "must be a JSON pointer",
                    );
                }
            }
        }
        if let Some(scope) = &list.scope {
            if list.op.is_none() {
                context.add(
                    "api.read.list.scope",
                    "applies to a list `op`, not an embedded one",
                );
            }
            if spec.parents.is_empty() {
                context.add("api.read.list.scope", "needs a parent kind to scope by");
            }
            // One parent id parameter serves several parent kinds only when the list is told
            // which kind the id names.
            if spec.parents.len() > 1
                && spec
                    .parents
                    .iter()
                    .any(|parent| !scope.query_by_parent.contains_key(parent))
            {
                context.add(
                    "api.read.list.scope",
                    "a collection scoped by one parent needs `query_by_parent` for every parent kind it serves",
                );
            }
            for parent in scope.query_by_parent.keys() {
                if !spec.parents.contains(parent) {
                    context.add(
                        "api.read.list.scope.query_by_parent",
                        format!("`{parent}` is not a parent of the kind"),
                    );
                }
            }
            if scope.param.trim().is_empty() {
                context.add("api.read.list.scope.param", "must name a query parameter");
            }
        }
        if list.authority == Authority::Partial && api.read.one.is_none() {
            context.add(
                "api.read",
                "a partial collection needs a direct read as well",
            );
        }
    }
    if let Some(one) = &api.read.one
        && one.agree
        && api.read.list.is_none()
    {
        context.add(
            "api.read.one.agree",
            "agreement needs a collection to agree with",
        );
    }
    match &api.create_identity {
        Some(CreateIdentity::FromResponse(pointer)) if !pointer.starts_with('/') => {
            context.add(
                "api.create_identity.from_response",
                "must be a JSON pointer",
            );
        }
        Some(CreateIdentity::DiffCollection { key }) => {
            if api.read.list.is_none() {
                context.add(
                    "api.create_identity",
                    "diff_collection needs a collection read",
                );
            }
            if key.is_empty() {
                context.add(
                    "api.create_identity.diff_collection.key",
                    "needs at least one field",
                );
            }
            for name in key {
                if !spec.fields.contains_key(name) {
                    context.add(
                        "api.create_identity.diff_collection.key",
                        format!("`{name}` is not a field"),
                    );
                }
            }
        }
        _ => {}
    }
}

fn check_field(
    context: &mut Context<'_>,
    path: &str,
    name: &str,
    field: &Field,
    shared_types: &BTreeSet<String>,
    top_level: bool,
) {
    if !is_snake(name) {
        context.add(path, "field names are lower snake case");
    }
    let parsed = match parse_type(&field.ty) {
        Ok(parsed) => Some(parsed),
        Err(error) => {
            context.add(format!("{path}.type"), error.to_string());
            None
        }
    };
    if let Some(parsed) = &parsed {
        check_type_rules(context, path, field, parsed, shared_types);
    }

    if field.class == ValueClass::Public
        && !matches!(parsed, Some(FieldType::Env))
        && looks_secret_bearing(name)
    {
        context.add(
            format!("{path}.class"),
            "the name suggests a secret or file content; declare `class: secret` or `class: content` (state refuses plain values under such names)",
        );
    }

    match (field.class, field.mutability) {
        (ValueClass::Secret, mutability) if mutability != Mutability::WriteOnly => {
            context.add(
                format!("{path}.mutability"),
                "a secret field must be write_only (it cannot be read back)",
            );
        }
        (class, Mutability::WriteOnly) if class != ValueClass::Secret => {
            context.add(
                format!("{path}.class"),
                "a write_only field must be class secret",
            );
        }
        _ => {}
    }
    if field.resend_on_update && field.mutability != Mutability::WriteOnly {
        context.add(
            format!("{path}.resend_on_update"),
            "only a write_only field is resent",
        );
    }
    if field.mutability == Mutability::Computed && field.nullable {
        context.add(
            format!("{path}.nullable"),
            "a computed field is not configurable",
        );
    }
    if let Some(since) = &field.since
        && !is_version(since)
    {
        context.add(format!("{path}.since"), "must be a version such as 0.30.6");
    }
    if let Some(until) = &field.until
        && !is_version(until)
    {
        context.add(format!("{path}.until"), "must be a version such as 0.30.6");
    }
    if !top_level && field.default.is_some() {
        context.add(
            format!("{path}.default"),
            "defaults apply to top-level fields only",
        );
    }
    if let Some(pattern) = &field.pattern
        && pattern.is_empty()
    {
        context.add(format!("{path}.pattern"), "must not be empty");
    }
}

/// Names whose values are secrets or file bodies. A public field cannot carry one: the
/// durable state keeps the same list as a backstop and refuses plain values under them
/// (ADR 0010), so the spec has to say which class the field is.
const SECRET_BEARING_SUFFIXES: &[&str] = &[
    "password",
    "apikey",
    "accesskey",
    "privatekey",
    "secret",
    "buildsecrets",
    "token",
    "refreshtoken",
    "buildargs",
    "previewenv",
    "previewbuildargs",
    "document",
    "composefile",
    "content",
    "script",
];

fn looks_secret_bearing(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect();

    SECRET_BEARING_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}

fn check_type_rules(
    context: &mut Context<'_>,
    path: &str,
    field: &Field,
    parsed: &FieldType,
    shared_types: &BTreeSet<String>,
) {
    if let FieldType::Shared(name) = parsed
        && !shared_types.contains(name)
    {
        context.add(
            format!("{path}.type"),
            format!("unknown shared type `{name}`"),
        );
    }
    let is_text = matches!(parsed, FieldType::Text);
    let is_number = matches!(parsed, FieldType::Int | FieldType::Number);
    if field.min_len.is_some() && !is_text {
        context.add(format!("{path}.min_len"), "applies to text only");
    }
    if field.pattern.is_some() && !is_text {
        context.add(format!("{path}.pattern"), "applies to text only");
    }
    if (field.min.is_some() || field.max.is_some()) && !is_number {
        context.add(
            format!("{path}.min"),
            "min and max apply to int and number only",
        );
    }
    if let (Some(min), Some(max)) = (field.min, field.max)
        && min > max
    {
        context.add(format!("{path}.min"), "min is greater than max");
    }
    if field.default.as_deref() == Some("key") && !is_text {
        context.add(format!("{path}.default"), "`key` defaults a text field");
    }
    if (field.class == ValueClass::Content) != matches!(parsed, FieldType::File) {
        context.add(
            format!("{path}.class"),
            "class content and type file go together",
        );
    }
    if field.granularity.is_some()
        && !matches!(
            parsed,
            FieldType::Map(_) | FieldType::Env | FieldType::Struct
        )
    {
        context.add(
            format!("{path}.granularity"),
            "applies to map, env, and struct only",
        );
    }

    match parsed {
        FieldType::Union { tag } => {
            if field.arms.is_empty() {
                context.add(format!("{path}.arms"), "a union needs at least one arm");
            }
            if !field.members.is_empty() {
                context.add(format!("{path}.members"), "a union has arms, not members");
            }
            for (arm, fields) in &field.arms {
                if !is_snake(arm) {
                    context.add(
                        format!("{path}.arms.{arm}"),
                        "arm names are lower snake case",
                    );
                }
                if fields.contains_key(tag) {
                    context.add(
                        format!("{path}.arms.{arm}"),
                        format!("`{tag}` is the tag and cannot also be a field"),
                    );
                }
                for (member, member_field) in fields {
                    check_field(
                        context,
                        &format!("{path}.arms.{arm}.{member}"),
                        member,
                        member_field,
                        &BTreeSet::new(),
                        false,
                    );
                }
            }
        }
        FieldType::Struct => {
            if field.members.is_empty() {
                context.add(format!("{path}.members"), "a struct needs members");
            }
            if !field.arms.is_empty() {
                context.add(format!("{path}.arms"), "only a union has arms");
            }
            for (member, member_field) in &field.members {
                check_field(
                    context,
                    &format!("{path}.members.{member}"),
                    member,
                    member_field,
                    &BTreeSet::new(),
                    false,
                );
            }
        }
        _ => {
            if !field.arms.is_empty() {
                context.add(format!("{path}.arms"), "only a union has arms");
            }
            if !field.members.is_empty() {
                context.add(format!("{path}.members"), "only a struct has members");
            }
        }
    }
}

fn check_write_groups(context: &mut Context<'_>, spec: &KindSpec) {
    let mut carried: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, group) in spec.write.iter().enumerate() {
        let path = format!("write[{index}]");
        match group {
            WriteGroup::Op { op, fields, .. } => {
                check_operation(context, &format!("{path}.op"), op);
                if fields.is_empty() {
                    context.add(format!("{path}.fields"), "a write group needs fields");
                }
                for name in fields {
                    carry(context, &mut carried, spec, &path, name);
                }
            }
            WriteGroup::ByVariant {
                by_variant, ops, ..
            } => {
                carry(context, &mut carried, spec, &path, by_variant);
                let Some(field) = spec.fields.get(by_variant) else {
                    continue;
                };
                if !matches!(parse_type(&field.ty), Ok(FieldType::Union { .. })) {
                    context.add(
                        format!("{path}.by_variant"),
                        format!("`{by_variant}` is not a union"),
                    );
                    continue;
                }
                for operation in ops.values() {
                    check_operation(context, &format!("{path}.ops"), operation);
                }
                let arms: BTreeSet<&String> = field.arms.keys().collect();
                let covered: BTreeSet<&String> = ops.keys().collect();
                if arms != covered {
                    context.add(
                        format!("{path}.ops"),
                        format!("must name exactly the arms of `{by_variant}`"),
                    );
                }
            }
        }
    }

    for (name, field) in &spec.fields {
        let writable = matches!(
            field.mutability,
            Mutability::InPlace | Mutability::Reparent | Mutability::WriteOnly
        );
        match (writable, carried.get(name.as_str()).copied().unwrap_or(0)) {
            (true, 0) => context.add(
                format!("fields.{name}"),
                "changeable but no write group carries it (make it create_only or add it to a group)",
            ),
            (_, count) if count > 1 => context.add(
                format!("fields.{name}"),
                "carried by more than one write group",
            ),
            _ => {}
        }
    }
}

fn carry<'a>(
    context: &mut Context<'_>,
    carried: &mut BTreeMap<&'a str, usize>,
    spec: &'a KindSpec,
    path: &str,
    name: &str,
) {
    let Some((key, field)) = spec.fields.get_key_value(name) else {
        context.add(path.to_owned(), format!("`{name}` is not a field"));
        return;
    };
    if matches!(
        field.mutability,
        Mutability::CreateOnly | Mutability::Computed
    ) {
        context.add(
            path.to_owned(),
            format!(
                "`{name}` is {:?} and cannot be written by an update",
                field.mutability
            ),
        );
    }
    *carried.entry(key.as_str()).or_insert(0) += 1;
}

fn check_children(context: &mut Context<'_>, spec: &KindSpec) {
    let mut kinds = BTreeSet::new();
    let mut sections = BTreeSet::new();
    for (index, child) in spec.children.iter().enumerate() {
        let path = format!("children[{index}]");
        if !is_snake(&child.kind) || !is_snake(&child.section) {
            context.add(path.clone(), "kind and section are lower snake case");
        }
        if !kinds.insert(&child.kind) {
            context.add(
                path.clone(),
                format!("child kind `{}` is listed twice", child.kind),
            );
        }
        if !sections.insert(&child.section) {
            context.add(
                path.clone(),
                format!("section `{}` is used twice", child.section),
            );
        }
        if spec.fields.contains_key(&child.section) {
            context.add(
                path,
                format!("section `{}` collides with a field", child.section),
            );
        }
    }
}
