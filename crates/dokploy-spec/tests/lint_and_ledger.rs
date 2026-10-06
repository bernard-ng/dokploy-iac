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
fn a_settings_singleton_uses_one_fixed_key_and_one_parameterless_read() {
    let source = r#"
kind: organization
scope: settings
section: organization
singleton: active
title: Organization
class: adopt_only
identity: { key: null, collision: [], address: "organization.{key}" }
api:
  id: id
  update: { op: organization.update, id_param: organizationId }
  read: { singleton: { op: organization.active } }
fields:
  name: { type: text }
write:
  - { op: organization.update, fields: [name], shape: full }
"#;

    assert_eq!(issues(source), Vec::<String>::new());

    let collection = source.replace(
        "read: { singleton: { op: organization.active } }",
        "read: { list: { op: organization.all } }",
    );
    assert!(
        issues(&collection)
            .iter()
            .any(|issue| issue.contains("a singleton kind needs a singleton read"))
    );

    let nested = source.replace("scope: settings", "scope: project");
    assert!(
        issues(&nested)
            .iter()
            .any(|issue| issue.contains("only valid for a top-level settings kind"))
    );
}

#[test]
fn lint_rejects_each_structural_mistake() {
    let cases: [(&str, String, &str); 11] = [
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
        (
            "a key selector on a scalar field",
            with(
                "color: { type: text, nullable: true }",
                "color: { type: text, nullable: true, key_selector: project }",
            ),
            "applies to a top-level map of selector sets with key granularity",
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
fn collection_and_fresh_read_mappings_are_checked() {
    let valid = BASE
        .replace("update: { op: widget.update }", "update: { op: widget.update, from_read: { physicalId: /physicalId } }")
        .replace(
            "one: { op: widget.one, id_param: widgetId, agree: true }",
            "one: { op: widget.one, id_param: physicalId, id_from: /physical/physicalId, agree: true }",
        )
        .replace("name: { type: text }", "name: { type: text, list_api: displayName }");
    assert_eq!(issues(&valid), Vec::<String>::new());

    let without_list = valid.replace("    list: { op: widget.all }\n", "");
    let found = issues(&without_list);
    assert!(
        found
            .iter()
            .any(|issue| issue.contains("collection pointer needs a collection read")),
        "{found:#?}"
    );
    assert!(
        found
            .iter()
            .any(|issue| issue.contains("collection mapping needs a collection read")),
        "{found:#?}"
    );

    let bad_pointers = valid
        .replace("physicalId: /physicalId", "physicalId: physicalId")
        .replace("id_from: /physical/physicalId", "id_from: physicalId");
    let found = issues(&bad_pointers);
    assert_eq!(
        found
            .iter()
            .filter(|issue| issue.contains("must be a JSON pointer"))
            .count(),
        2,
        "{found:#?}"
    );
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
fn a_union_can_select_create_and_update_operations_by_arm() {
    let source = r#"
kind: notification
scope: settings
section: notifications
title: Notification
identity: { key: name, collision: [name], address: "notification.{key}" }
api:
  id: notificationId
  create:
    by_variant: config
    ops: { slack: notification.createSlack, teams: notification.createTeams }
  remove: { op: notification.remove }
  read: { list: { op: notification.all }, one: { op: notification.one, id_param: notificationId } }
  create_identity: { diff_collection: { key: [name] } }
fields:
  name: { type: text, default: key }
  config:
    api: notificationType
    type: union(type)
    arms:
      slack:
        webhook_url: { api: /slack/webhookUrl, request: webhookUrl, type: text, class: secret, mutability: write_only }
      teams:
        webhook_url: { api: /teams/webhookUrl, request: webhookUrl, type: text, class: secret, mutability: write_only }
write:
  - by_variant: config
    fields: [name]
    ops: { slack: notification.updateSlack, teams: notification.updateTeams }
"#;

    assert_eq!(issues(source), Vec::<String>::new());
    let parsed = spec(source);
    let webhook = parsed.property("config.slack.webhook_url").unwrap();
    assert_eq!(webhook.api, "/slack/webhookUrl");
    assert_eq!(webhook.request_key(), "webhookUrl");

    let incomplete = source.replace(
        "ops: { slack: notification.createSlack, teams: notification.createTeams }",
        "ops: { slack: notification.createSlack }",
    );
    assert!(
        issues(&incomplete)
            .iter()
            .any(|issue| issue.contains("must name exactly the arms"))
    );
}

#[test]
fn json_decoding_is_limited_to_write_only_secret_text() {
    let valid = BASE.replace(
        "token: { type: text, class: secret, mutability: write_only }",
        "token: { type: text, class: secret, mutability: write_only, json: true }",
    );
    assert_eq!(issues(&valid), Vec::<String>::new());

    for invalid in [
        valid.replace(
            "token: { type: text, class: secret, mutability: write_only, json: true }",
            "token: { type: bool, class: secret, mutability: write_only, json: true }",
        ),
        valid.replace(
            "token: { type: text, class: secret, mutability: write_only, json: true }",
            "token: { type: text, class: public, mutability: write_only, json: true }",
        ),
        valid.replace(
            "token: { type: text, class: secret, mutability: write_only, json: true }",
            "token: { type: text, class: secret, mutability: create_only, json: true }",
        ),
    ] {
        assert!(
            issues(&invalid)
                .iter()
                .any(|issue| issue.contains("JSON decoding applies only")),
            "{:?}",
            issues(&invalid)
        );
    }
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

#[test]
fn a_public_field_cannot_carry_a_secret_looking_name() {
    for name in [
        "api_key",
        "refresh_token",
        "build_secrets",
        "script",
        "compose_file",
        "content",
    ] {
        let broken = with(
            "  color: { type: text, nullable: true }\n",
            &format!(
                "  color: {{ type: text, nullable: true }}\n  {name}: {{ type: text, nullable: true }}\n"
            ),
        );
        let found = issues(&broken);
        assert!(
            found
                .iter()
                .any(|issue| issue.contains(&format!("fields.{name}.class"))
                    && issue.contains("class: secret")),
            "{name}: {found:?}"
        );
    }

    // Declaring the class is the fix, and a field with an ordinary name is untouched.
    let fixed = with(
        "  color: { type: text, nullable: true }\n",
        "  color: { type: text, nullable: true }\n  script: { type: text, class: content, nullable: true }\n  command: { type: text, nullable: true }\n",
    );
    assert!(
        !issues(&fixed)
            .iter()
            .any(|issue| issue.contains("name suggests")),
        "{:?}",
        issues(&fixed)
    );
}

#[test]
fn a_file_key_names_a_free_document_key_of_a_top_level_env_field() {
    let valid = with(
        "  size: { type: int, mutability: create_only }",
        "  size: { type: int, mutability: create_only }\n  environment: { api: env, type: env, file_key: env_file, mutability: create_only }",
    );
    assert!(issues(&valid).is_empty(), "{:?}", issues(&valid));

    let not_env = with(
        "  color: { type: text, nullable: true }",
        "  color: { type: text, nullable: true, file_key: color_file }",
    );
    assert!(
        issues(&not_env)
            .iter()
            .any(|issue| issue.contains("top-level env field only")),
        "{:?}",
        issues(&not_env)
    );

    for taken in ["name", "lifecycle", "depends_on"] {
        let clash = with(
            "  size: { type: int, mutability: create_only }",
            &format!(
                "  size: {{ type: int, mutability: create_only }}\n  environment: {{ api: env, type: env, mutability: create_only, file_key: {taken} }}"
            ),
        );
        assert!(
            issues(&clash)
                .iter()
                .any(|issue| issue.contains("already a key of the document")),
            "{taken}: {:?}",
            issues(&clash)
        );
    }

    let twice = with(
        "  size: { type: int, mutability: create_only }",
        "  size: { type: int, mutability: create_only }\n  environment: { api: env, type: env, mutability: create_only, file_key: env_file }\n  build_args: { api: buildArgs, type: env, mutability: create_only, file_key: env_file }",
    );
    assert!(
        issues(&twice)
            .iter()
            .any(|issue| issue.contains("file key of another field")),
        "{:?}",
        issues(&twice)
    );
}

#[test]
fn a_kind_made_with_its_parent_names_a_computed_bool_marker_and_has_one_parent() {
    let nowhere = with(
        "ledger:\n  readonly: [createdAt]",
        "made_with_parent: { name: default, marker: nothing }\nledger:\n  readonly: [createdAt]",
    );
    let found = issues(&nowhere);
    for fragment in ["exactly one parent", "`nothing` is not a field"] {
        assert!(
            found.iter().any(|issue| issue.contains(fragment)),
            "{fragment}: {found:?}"
        );
    }

    let not_computed = with(
        "ledger:\n  readonly: [createdAt]",
        "made_with_parent: { name: default, marker: color }\nledger:\n  readonly: [createdAt]",
    );
    assert!(
        issues(&not_computed)
            .iter()
            .any(|issue| issue.contains("a computed bool field")),
        "{:?}",
        issues(&not_computed)
    );
}
