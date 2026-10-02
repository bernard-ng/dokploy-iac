//! Lint rules, registry relations, and ledger failures, on small synthetic specs.

use std::collections::BTreeSet;

use dokploy_spec::{
    Coverage, KindSpec, OperationIndex, SpecRegistry, check_ledger, parse_spec, validate_spec,
};
use serde_json::json;

const BASE: &str = r#"
kind: widget
scope: settings
section: widgets
title: Widget
identity: { key: name, collision: [name], address: "widget.{key}" }
api:
  id: widgetId
  create: { op: widget.create }
  update: { op: widget.update }
  remove: { op: widget.remove }
  read:
    list: { op: widget.all }
    one: { op: widget.one, id_param: widgetId, agree: true }
  create_identity: { from_response: /widgetId }
fields:
  name: { type: text }
  color: { type: text, nullable: true }
  token: { type: text, class: secret, mutability: write_only }
  size: { type: int, mutability: create_only }
write:
  - { op: widget.update, fields: [name, color, token], shape: partial }
ledger:
  readonly: [createdAt]
"#;

fn spec(source: &str) -> KindSpec {
    parse_spec(source).expect("the test spec parses")
}

fn issues(source: &str) -> Vec<String> {
    validate_spec(&spec(source), &BTreeSet::new())
        .into_iter()
        .map(|issue| issue.to_string())
        .collect()
}

fn with(replace: &str, by: &str) -> String {
    assert!(
        BASE.contains(replace),
        "{replace:?} is not in the base spec"
    );
    BASE.replacen(replace, by, 1)
}

fn openapi() -> serde_json::Value {
    json!({
        "paths": {
            "/widget.create": { "post": { "operationId": "widget-create", "requestBody": { "content": { "application/json": { "schema": {
                "type": "object", "properties": { "name": {}, "color": {}, "token": {}, "size": {} } } } } } } },
            "/widget.update": { "post": { "operationId": "widget-update", "requestBody": { "content": { "application/json": { "schema": {
                "$ref": "#/components/schemas/WidgetUpdate" } } } } } },
            "/widget.remove": { "post": { "operationId": "widget-remove", "requestBody": { "content": { "application/json": { "schema": {
                "type": "object", "properties": { "widgetId": {} } } } } } } },
            "/widget.all": { "get": { "operationId": "widget-all" } },
            "/widget.one": { "get": { "operationId": "widget-one", "parameters": [ { "in": "query", "name": "widgetId" } ] } }
        },
        "components": { "schemas": { "WidgetUpdate": { "type": "object",
            "properties": { "widgetId": {}, "name": {}, "color": {}, "token": {}, "createdAt": {} } } } }
    })
}

fn ledger(source: &str, document: &serde_json::Value) -> dokploy_spec::LedgerReport {
    let registry = SpecRegistry::from_specs(vec![spec(source)], &BTreeSet::new())
        .expect("the spec is structurally valid");
    check_ledger(&registry, &OperationIndex::from_openapi(document))
}

#[test]
fn the_base_spec_is_valid_and_its_ledger_is_clean() {
    assert_eq!(issues(BASE), Vec::<String>::new());
    let report = ledger(BASE, &openapi());
    assert!(report.is_clean(), "{:?}", report.issues);
    assert_eq!(report.kinds[0].mapped, 5, "id, name, color, token, size");
    assert_eq!(report.kinds[0].readonly, 1);
}

#[test]
fn lint_rejects_each_structural_mistake() {
    let cases: [(&str, String, &str); 10] = [
        (
            "a secret that can be read back",
            with(
                "token: { type: text, class: secret, mutability: write_only }",
                "token: { type: text, class: secret }",
            ),
            "secret field must be write_only",
        ),
        (
            "write_only without secret",
            with(
                "token: { type: text, class: secret, mutability: write_only }",
                "token: { type: text, mutability: write_only }",
            ),
            "write_only field must be class secret",
        ),
        (
            "a changeable field no group carries",
            with("fields: [name, color, token]", "fields: [name, token]"),
            "no write group carries it",
        ),
        (
            "a create_only field in a write group",
            with(
                "fields: [name, color, token]",
                "fields: [name, color, token, size]",
            ),
            "cannot be written by an update",
        ),
        (
            "a field in two groups",
            with(
                "shape: partial }",
                "shape: partial }\n  - { op: widget.other, fields: [name], shape: partial }",
            ),
            "carried by more than one write group",
        ),
        (
            "a managed kind without create_identity",
            with("  create_identity: { from_response: /widgetId }\n", ""),
            "say how the new identity is learned",
        ),
        (
            "an unknown field type",
            with(
                "color: { type: text, nullable: true }",
                "color: { type: blorp(x), nullable: true }",
            ),
            "invalid type",
        ),
        (
            "an identity key that is not a field",
            with("key: name,", "key: nope,"),
            "`nope` is not a field",
        ),
        (
            "an address without a placeholder",
            with("\"widget.{key}\"", "\"widget\""),
            "{key} placeholder",
        ),
        (
            "an operation that is not resource.operation",
            with(
                "create: { op: widget.create }",
                "create: { op: createWidget }",
            ),
            "not a `resource.operation` id",
        ),
    ];
    for (label, source, expected) in cases {
        let found = issues(&source);
        assert!(
            found.iter().any(|issue| issue.contains(expected)),
            "{label}: expected an issue containing {expected:?}, got {found:#?}"
        );
    }
}

#[test]
fn a_union_must_be_written_by_an_operation_per_arm() {
    let source = r#"
kind: widget
scope: settings
section: widgets
title: Widget
identity: { key: name, collision: [name], address: "widget.{key}" }
api:
  id: widgetId
  create: { op: widget.create }
  remove: { op: widget.remove }
  read: { one: { op: widget.one, id_param: widgetId } }
  create_identity: none
fields:
  name: { type: text, mutability: create_only }
  source:
    type: union(type)
    arms:
      a: { x: { type: text } }
      b: { y: { type: text } }
write:
  - { by_variant: source, ops: { a: widget.saveA } }
"#;
    let found = issues(source);
    assert!(
        found
            .iter()
            .any(|i| i.contains("must name exactly the arms")),
        "{found:#?}"
    );
}

#[test]
fn the_ledger_fails_an_unclassified_request_field_for_full_coverage() {
    let mut document = openapi();
    document["components"]["schemas"]["WidgetUpdate"]["properties"]["surprise"] = json!({});
    let report = ledger(BASE, &document);
    assert!(
        report.issues.iter().any(|issue| issue
            .to_string()
            .contains("`surprise` is not mapped or classified")),
        "{:?}",
        report.issues
    );
}

#[test]
fn a_partial_spec_counts_unclassified_fields_without_failing() {
    let mut document = openapi();
    document["components"]["schemas"]["WidgetUpdate"]["properties"]["surprise"] = json!({});
    let partial = BASE.replacen("title: Widget", "title: Widget\ncoverage: partial", 1);
    let report = ledger(&partial, &document);
    assert!(report.is_clean(), "{:?}", report.issues);
    assert_eq!(report.kinds[0].coverage, Coverage::Partial);
    assert_eq!(
        report.kinds[0].unclassified,
        vec![("widget.update".to_owned(), "surprise".to_owned())]
    );
}

#[test]
fn the_ledger_fails_an_unknown_operation() {
    let mut document = openapi();
    document["paths"]
        .as_object_mut()
        .unwrap()
        .remove("/widget.remove");
    let report = ledger(BASE, &document);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.to_string().contains("`widget.remove` does not exist")),
        "{:?}",
        report.issues
    );
}

#[test]
fn the_ledger_fails_a_mapped_field_the_operation_no_longer_accepts() {
    let mut document = openapi();
    document["components"]["schemas"]["WidgetUpdate"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("color");
    let report = ledger(BASE, &document);
    assert!(
        report.issues.iter().any(|issue| issue
            .to_string()
            .contains("maps to `color`, which `widget.update` does not accept")),
        "{:?}",
        report.issues
    );
}

#[test]
fn a_create_only_field_must_be_accepted_by_create() {
    let mut document = openapi();
    document["paths"]["/widget.create"]["post"]["requestBody"]["content"]["application/json"]
        ["schema"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("size");
    let report = ledger(BASE, &document);
    assert!(
        report.issues.iter().any(|issue| issue
            .to_string()
            .contains("maps to `size`, which `widget.create` does not accept")),
        "{:?}",
        report.issues
    );
}

#[test]
fn a_field_cannot_be_both_mapped_and_classified() {
    let source = with("readonly: [createdAt]", "readonly: [createdAt, name]");
    let report = ledger(&source, &openapi());
    assert!(
        report.issues.iter().any(|issue| issue
            .to_string()
            .contains("`name` is both mapped and classified readonly")),
        "{:?}",
        report.issues
    );
}

#[test]
fn registry_relations_are_cross_checked() {
    let parent = r#"
kind: gadget
scope: project
section: gadgets
title: Gadget
identity: { key: name, collision: [name], address: "gadget.{key}" }
api:
  id: gadgetId
  create: { op: gadget.create }
  remove: { op: gadget.remove }
  read: { one: { op: gadget.one, id_param: gadgetId } }
  create_identity: none
fields:
  name: { type: text, mutability: create_only }
children:
  - { kind: widget, section: widgets }
"#;
    // widget declares scope settings and no parent: the parent lists a child that does not point back.
    let error = SpecRegistry::from_specs(vec![spec(parent), spec(BASE)], &BTreeSet::new())
        .expect_err("a child that does not name its parent is rejected");
    let text: Vec<String> = error.iter().map(ToString::to_string).collect();
    assert!(
        text.iter()
            .any(|t| t.contains("does not name it as parent")),
        "{text:#?}"
    );

    let orphan = BASE.replacen("scope: settings", "scope: settings\nparent: ghost", 1);
    let error = SpecRegistry::from_specs(vec![spec(&orphan)], &BTreeSet::new())
        .expect_err("an unknown parent is rejected");
    assert!(
        error
            .iter()
            .any(|i| i.to_string().contains("unknown kind `ghost`"))
    );
}

#[test]
fn unknown_keys_and_anchors_are_rejected() {
    let unknown = with("title: Widget", "title: Widget\nbogus: 1");
    assert!(parse_spec(&unknown).is_err());
    let anchored = with("title: Widget", "title: &t Widget");
    assert!(parse_spec(&anchored).is_err());
}
