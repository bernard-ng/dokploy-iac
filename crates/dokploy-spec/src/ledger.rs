//! The coverage ledger: every request field of every operation a spec uses must be
//! classified (ADR 0002). Only the request side is checkable from the OpenAPI
//! document; response fields need recorded live fixtures (ADR 0007).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::error::Issue;
use crate::model::{Coverage, Field, KindSpec, Mutability, WriteGroup};
use crate::registry::SpecRegistry;
use crate::types::{FieldType, parse_type};

/// The request fields of one operation.
#[derive(Clone, Debug, Eq, PartialEq)]
struct OperationInfo {
    request_fields: BTreeSet<String>,
}

/// An index of the OpenAPI document's operations and their request fields.
#[derive(Clone, Debug)]
pub struct OperationIndex {
    operations: BTreeMap<String, OperationInfo>,
}

impl OperationIndex {
    /// Indexes an OpenAPI 3 document.
    #[must_use]
    pub fn from_openapi(document: &Value) -> Self {
        let components = document
            .pointer("/components/schemas")
            .and_then(Value::as_object);
        let mut operations = BTreeMap::new();
        let Some(paths) = document.get("paths").and_then(Value::as_object) else {
            return Self { operations };
        };
        for item in paths.values() {
            let Some(item) = item.as_object() else {
                continue;
            };
            for operation in item.values() {
                let Some(id) = operation.get("operationId").and_then(Value::as_str) else {
                    continue;
                };
                let mut fields = BTreeSet::new();
                if let Some(parameters) = operation.get("parameters").and_then(Value::as_array) {
                    for parameter in parameters {
                        let location = parameter.get("in").and_then(Value::as_str);
                        if matches!(location, Some("query" | "path"))
                            && let Some(name) = parameter.get("name").and_then(Value::as_str)
                        {
                            fields.insert(name.to_owned());
                        }
                    }
                }
                if let Some(schema) =
                    operation.pointer("/requestBody/content/application~1json/schema")
                {
                    collect_properties(schema, components, &mut fields, 0);
                }
                operations.insert(
                    id.to_owned(),
                    OperationInfo {
                        request_fields: fields,
                    },
                );
            }
        }
        Self { operations }
    }

    fn lookup(&self, op: &str) -> Option<&OperationInfo> {
        self.operations.get(&op.replace('.', "-"))
    }
}

fn collect_properties(
    schema: &Value,
    components: Option<&serde_json::Map<String, Value>>,
    fields: &mut BTreeSet<String>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or(reference);
        if let Some(target) = components.and_then(|c| c.get(name)) {
            collect_properties(target, components, fields, depth + 1);
        }
        return;
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        fields.extend(properties.keys().cloned());
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(key).and_then(Value::as_array) {
            for branch in branches {
                collect_properties(branch, components, fields, depth + 1);
            }
        }
    }
}

/// The ledger of one kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KindLedger {
    /// The kind id.
    pub kind: String,
    /// Whether the ledger must be complete.
    pub coverage: Coverage,
    /// Distinct request fields mapped to spec fields.
    pub mapped: usize,
    /// Request fields classified `readonly`.
    pub readonly: usize,
    /// Request fields classified `action`.
    pub action: usize,
    /// Request fields classified `ignored`.
    pub ignored: usize,
    /// Request fields classified `derived`.
    pub derived: usize,
    /// `(operation, field)` pairs that are not classified.
    pub unclassified: Vec<(String, String)>,
}

/// The result of checking every spec against the OpenAPI document.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LedgerReport {
    /// One entry per kind.
    pub kinds: Vec<KindLedger>,
    /// Failures: unknown operations, vanished fields, contradictions, and (for
    /// full-coverage kinds) unclassified fields.
    pub issues: Vec<Issue>,
}

impl LedgerReport {
    /// Whether the ledger passed.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Every operation a spec refers to, with a role label for messages.
fn operations_of(spec: &KindSpec) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut add = |role: &str, op: &str| found.push((role.to_owned(), op.to_owned()));
    if let Some(operation) = &spec.api.create {
        add("create", &operation.op);
    }
    if let Some(operation) = &spec.api.update {
        add("update", &operation.op);
    }
    if let Some(operation) = &spec.api.remove {
        add("remove", &operation.op);
    }
    if let Some(deploy) = &spec.api.deploy {
        add("deploy", &deploy.op);
    }
    if let Some(one) = &spec.api.read.one {
        add("read.one", &one.op);
    }
    if let Some(list) = &spec.api.read.list
        && let Some(op) = &list.op
    {
        add("read.list", op);
    }
    for group in &spec.write {
        match group {
            WriteGroup::Op { op, .. } => add("write", op),
            WriteGroup::ByVariant { ops, .. } => {
                for op in ops.values() {
                    add("write", op);
                }
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Request field names that spec fields map to, including union arms and struct members.
fn mapped_names(spec: &KindSpec) -> BTreeSet<String> {
    fn walk(name: &str, field: &Field, out: &mut BTreeSet<String>) {
        out.insert(field.request_name(name).to_owned());
        for (member, member_field) in &field.members {
            walk(member, member_field, out);
        }
        for arm in field.arms.values() {
            for (member, member_field) in arm {
                walk(member, member_field, out);
            }
        }
    }
    let mut names = BTreeSet::new();
    for (name, field) in &spec.fields {
        // A struct or union maps its members, not a field of its own, unless it also names an api field.
        if field.api.is_some()
            || !matches!(
                parse_type(&field.ty),
                Ok(FieldType::Union { .. } | FieldType::Struct)
            )
        {
            names.insert(field.request_name(name).to_owned());
        }
        for (member, member_field) in &field.members {
            walk(member, member_field, &mut names);
        }
        for arm in field.arms.values() {
            for (member, member_field) in arm {
                walk(member, member_field, &mut names);
            }
        }
    }
    names.insert(spec.api.id.clone());
    for operation in [&spec.api.create, &spec.api.update, &spec.api.remove]
        .into_iter()
        .flatten()
    {
        if let Some(param) = &operation.id_param {
            names.insert(param.clone());
        }
        names.extend(operation.attach.keys().cloned());
        for overrides in operation.attach_by_parent.values() {
            names.extend(overrides.keys().cloned());
        }
    }
    if let Some(one) = &spec.api.read.one {
        names.insert(one.id_param.clone());
    }
    names
}

/// Checks every spec against the OpenAPI operation index.
#[must_use]
pub fn check_ledger(registry: &SpecRegistry, index: &OperationIndex) -> LedgerReport {
    let mut report = LedgerReport::default();
    for spec in registry.kinds() {
        report
            .kinds
            .push(check_kind(spec, index, &mut report.issues));
    }
    report.issues.sort();
    report
}

fn check_kind(spec: &KindSpec, index: &OperationIndex, issues: &mut Vec<Issue>) -> KindLedger {
    let mapped = mapped_names(spec);
    let readonly: BTreeSet<&str> = spec.ledger.readonly.iter().map(String::as_str).collect();
    let action: BTreeSet<&str> = spec.ledger.action.iter().map(String::as_str).collect();
    let ignored: BTreeSet<&str> = spec
        .ledger
        .ignored
        .iter()
        .map(|i| i.field.as_str())
        .collect();
    let derived: BTreeSet<&str> = spec.ledger.derived.iter().map(String::as_str).collect();

    for (class, names) in [
        ("readonly", &readonly),
        ("action", &action),
        ("ignored", &ignored),
        ("derived", &derived),
    ] {
        for name in names {
            if mapped.contains(*name) {
                issues.push(Issue::new(
                    &spec.kind,
                    format!("ledger.{class}"),
                    format!("`{name}` is both mapped and classified {class}"),
                ));
            }
        }
    }

    let mut operations: BTreeMap<String, &OperationInfo> = BTreeMap::new();
    for (role, op) in operations_of(spec) {
        match index.lookup(&op) {
            Some(info) => {
                operations.insert(op, info);
            }
            None => issues.push(Issue::new(
                &spec.kind,
                role,
                format!("operation `{op}` does not exist in the OpenAPI document"),
            )),
        }
    }

    let mut seen_mapped = BTreeSet::new();
    let mut seen_classified: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut unclassified = Vec::new();
    for (op, info) in &operations {
        for field in &info.request_fields {
            if mapped.contains(field) {
                seen_mapped.insert(field.clone());
            } else if readonly.contains(field.as_str()) {
                seen_classified
                    .entry("readonly")
                    .or_default()
                    .insert(field.clone());
            } else if action.contains(field.as_str()) {
                seen_classified
                    .entry("action")
                    .or_default()
                    .insert(field.clone());
            } else if ignored.contains(field.as_str()) {
                seen_classified
                    .entry("ignored")
                    .or_default()
                    .insert(field.clone());
            } else if derived.contains(field.as_str()) {
                seen_classified
                    .entry("derived")
                    .or_default()
                    .insert(field.clone());
            } else {
                unclassified.push((op.clone(), field.clone()));
            }
        }
    }
    unclassified.sort();

    check_mapped_fields_exist(spec, &operations, issues);

    if spec.coverage == Coverage::Full {
        for (op, field) in &unclassified {
            issues.push(Issue::new(
                &spec.kind,
                format!("ledger.{op}"),
                format!("request field `{field}` is not mapped or classified"),
            ));
        }
    }

    let count = |class: &str| seen_classified.get(class).map_or(0, BTreeSet::len);
    KindLedger {
        kind: spec.kind.clone(),
        coverage: spec.coverage,
        mapped: seen_mapped.len(),
        readonly: count("readonly"),
        action: count("action"),
        ignored: count("ignored"),
        derived: count("derived"),
        unclassified,
    }
}

/// Mapped fields must be accepted by the operation that writes them.
fn check_mapped_fields_exist(
    spec: &KindSpec,
    operations: &BTreeMap<String, &OperationInfo>,
    issues: &mut Vec<Issue>,
) {
    let mut require = |op: &str, name: &str, field: &Field, owner: &str| {
        let Some(info) = operations.get(op) else {
            return;
        };
        let request = field.request_name(name);
        if !info.request_fields.contains(request) {
            issues.push(Issue::new(
                &spec.kind,
                format!("fields.{owner}"),
                format!("maps to `{request}`, which `{op}` does not accept"),
            ));
        }
    };

    for group in &spec.write {
        match group {
            WriteGroup::Op { op, fields, .. } => {
                for name in fields {
                    let Some(field) = spec.fields.get(name) else {
                        continue;
                    };
                    match parse_type(&field.ty) {
                        Ok(FieldType::Union { .. }) => {
                            for arm in field.arms.values() {
                                for (member, member_field) in arm {
                                    require(op, member, member_field, name);
                                }
                            }
                        }
                        Ok(FieldType::Struct) if field.api.is_none() => {
                            for (member, member_field) in &field.members {
                                require(op, member, member_field, name);
                            }
                        }
                        _ => require(op, name, field, name),
                    }
                }
            }
            WriteGroup::ByVariant {
                by_variant, ops, ..
            } => {
                let Some(field) = spec.fields.get(by_variant) else {
                    continue;
                };
                for (arm, op) in ops {
                    if let Some(members) = field.arms.get(arm) {
                        for (member, member_field) in members {
                            require(op, member, member_field, by_variant);
                        }
                    }
                }
            }
        }
    }

    if let Some(create) = &spec.api.create {
        for (name, field) in &spec.fields {
            if field.mutability == Mutability::CreateOnly
                && matches!(parse_type(&field.ty), Ok(t) if !matches!(t, FieldType::Union{..} | FieldType::Struct))
            {
                require(&create.op, name, field, name);
            }
        }
    }
}
