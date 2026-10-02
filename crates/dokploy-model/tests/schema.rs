mod common;

use common::generate::document;
use common::{repository, widget};
use dokploy_model::json_schema;
use dokploy_spec::Scope;
use regex::Regex;
use serde_json::Value;

/// A validator for the subset of JSON Schema the generator emits, so the schema is held to
/// what it claims without a validator dependency.
fn check(schema: &Value, value: &Value, root: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.strip_prefix("#/$defs/").expect("local reference");
        check(&root["$defs"][name], value, root, path, errors);
        return;
    }
    if let Some(expected) = schema.get("const")
        && expected != value
    {
        errors.push(format!("{path}: not the constant {expected}"));
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        errors.push(format!("{path}: not one of {allowed:?}"));
    }
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        let ok = match kind {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "null" => value.is_null(),
            other => panic!("unsupported type {other}"),
        };
        if !ok {
            errors.push(format!("{path}: not {kind}"));
            return;
        }
    }
    if let Some(text) = value.as_str() {
        if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
            && (text.chars().count() as u64) < min
        {
            errors.push(format!("{path}: shorter than {min}"));
        }
        if let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
            && (text.chars().count() as u64) > max
        {
            errors.push(format!("{path}: longer than {max}"));
        }
        if let Some(pattern) = schema.get("pattern").and_then(Value::as_str)
            && !Regex::new(pattern).unwrap().is_match(text)
        {
            errors.push(format!("{path}: does not match {pattern}"));
        }
    }
    if let Some(number) = value.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && number < min
        {
            errors.push(format!("{path}: below {min}"));
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && number > max
        {
            errors.push(format!("{path}: above {max}"));
        }
    }
    if let Some(items) = value.as_array() {
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                check(item_schema, item, root, &format!("{path}[{index}]"), errors);
            }
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
            for (index, item) in items.iter().enumerate() {
                if items[..index].contains(item) {
                    errors.push(format!("{path}[{index}]: duplicate"));
                }
            }
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = required.as_str().unwrap();
            if !object.contains_key(name) {
                errors.push(format!("{path}: missing {name}"));
            }
        }
        if let Some(naming) = schema
            .get("propertyNames")
            .and_then(|n| n.get("pattern"))
            .and_then(Value::as_str)
        {
            let regex = Regex::new(naming).unwrap();
            for name in object.keys() {
                if !regex.is_match(name) {
                    errors.push(format!("{path}: key {name:?} does not match {naming}"));
                }
            }
        }
        for (name, member) in object {
            let member_path = format!("{path}.{name}");
            match properties.and_then(|p| p.get(name)) {
                Some(member_schema) => check(member_schema, member, root, &member_path, errors),
                None => match schema.get("additionalProperties") {
                    Some(Value::Bool(false)) => errors.push(format!("{member_path}: not allowed")),
                    Some(extra) if extra.is_object() => {
                        check(extra, member, root, &member_path, errors);
                    }
                    _ => {}
                },
            }
        }
    }
    for (keyword, need_exactly_one) in [("oneOf", true), ("anyOf", false)] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            let passing = branches
                .iter()
                .filter(|branch| {
                    let mut found = Vec::new();
                    check(branch, value, root, path, &mut found);
                    found.is_empty()
                })
                .count();
            if (need_exactly_one && passing != 1) || (!need_exactly_one && passing == 0) {
                errors.push(format!(
                    "{path}: matched {passing} of the {keyword} alternatives"
                ));
            }
        }
    }
}

fn errors_of(schema: &Value, yaml: &str) -> Vec<String> {
    let value: Value = serde_saphyr::from_str(yaml).expect("the sample is YAML");
    let mut errors = Vec::new();
    check(schema, &value, schema, "$", &mut errors);
    errors
}

#[test]
fn the_schema_accepts_every_document_the_generator_makes() {
    for (registry, scope, label) in [
        (repository(), Scope::Settings, "settings"),
        (repository(), Scope::Project, "project"),
        (widget(), Scope::Settings, "widget"),
    ] {
        let schema = json_schema(registry, scope);
        for seed in 0..200 {
            let text = document(seed, registry, scope).render();
            let errors = errors_of(&schema, &text);
            assert!(errors.is_empty(), "{label} seed {seed}: {errors:?}\n{text}");
        }
    }
}

#[test]
fn the_schema_rejects_what_the_parser_rejects_where_json_can_say_so() {
    let settings = json_schema(repository(), Scope::Settings);
    let project = json_schema(repository(), Scope::Project);
    let widget_schema = json_schema(widget(), Scope::Settings);
    let cases: [(&Value, &str); 14] = [
        (&settings, "settings: {}\n"),
        (&settings, "version: 1\nsettings: {}\n"),
        (&settings, "version: 2\nsettings: {}\nproject: {slug: a}\n"),
        (&settings, "version: 2\nsettings:\n  gadgets: {}\n"),
        (
            &settings,
            "version: 2\nsettings:\n  registries:\n    ghcr:\n      urll: x\n",
        ),
        (
            &settings,
            "version: 2\nsettings:\n  registries:\n    Bad_Key: {}\n",
        ),
        (
            &settings,
            "version: 2\nsettings:\n  registries:\n    ghcr:\n      password: hunter2\n",
        ),
        (
            &settings,
            "version: 2\nsettings:\n  registries:\n    ghcr:\n      type: nope\n",
        ),
        (
            &settings,
            "version: 2\nsettings:\n  registries:\n    ghcr:\n      username: ''\n",
        ),
        (&project, "version: 2\nproject:\n  name: no slug\n"),
        (
            &project,
            "version: 2\nproject:\n  slug: a\n  environments:\n    e:\n      applications:\n        x:\n          replicas: -1\n",
        ),
        (
            &widget_schema,
            "version: 2\nsettings:\n  widgets:\n    w:\n      tags: [a, a]\n",
        ),
        (
            &widget_schema,
            "version: 2\nsettings:\n  widgets:\n    w:\n      owner: {local: false}\n",
        ),
        (
            &widget_schema,
            "version: 2\nsettings:\n  widgets:\n    w:\n      source: {type: svn}\n",
        ),
    ];
    for (schema, source) in cases {
        assert!(
            !errors_of(schema, source).is_empty(),
            "the schema accepted:\n{source}"
        );
    }
}

#[test]
fn the_schema_names_the_document_it_describes() {
    let settings = json_schema(repository(), Scope::Settings);
    assert_eq!(
        settings["required"],
        serde_json::json!(["version", "settings"])
    );
    assert_eq!(settings["properties"]["version"]["const"], 2);
    assert!(settings["$defs"]["registry"].is_object());
    assert!(
        settings["$defs"].get("application").is_none(),
        "other scopes stay out"
    );

    let project = json_schema(repository(), Scope::Project);
    assert_eq!(
        project["properties"]["project"]["required"],
        serde_json::json!(["slug"])
    );
    assert!(project["$defs"]["redirect"].is_object());
}
