use std::path::Path;

use dokploy_spec::{
    FieldType, KindSpec, Mutability, PathError, PathShape, SpecRegistry, ValueClass, load_dir,
    parse_spec,
};

fn registry() -> SpecRegistry {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    load_dir(&root).expect("repository specs are valid")
}

/// A spec that exercises every path shape the repository specs do not.
fn rich() -> KindSpec {
    parse_spec(
        r#"
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
fields:
  name: { type: text, min_len: 1, default: key }
  mode: { type: "enum[a, b]" }
  labels: { type: "map<text, text>", granularity: key }
  token: { type: text, class: secret, mutability: write_only }
  owner: { type: selector(server), nullable: true }
  limits:
    type: struct
    granularity: field
    mutability: create_only
    members:
      cpu: { type: int, min: 1, max: 64 }
      note: { type: text, nullable: true }
  tuning:
    type: struct
    members:
      level: { type: int }
  source:
    type: union(type)
    arms:
      one: { path: { type: text } }
  tags: { type: "set<text>" }
write:
  - { op: widget.update, fields: [name, mode, labels, token, owner, limits, tuning, source, tags], shape: partial }
"#,
    )
    .expect("the test spec parses")
}

#[test]
fn a_scalar_field_is_one_atomic_property() {
    let info = registry().property("redirect", "regex").unwrap();
    assert_eq!(info.kind, "redirect");
    assert_eq!(info.path, "regex");
    assert_eq!(info.shape, PathShape::Atomic);
    assert_eq!(info.ty, FieldType::Text);
    assert_eq!(info.rules.min_len, Some(1));
    assert!(info.is_required_on_create());
    assert!(!info.is_sensitive() && !info.is_lifecycle_only() && !info.is_selector());
}

#[test]
fn flags_follow_the_class_mutability_and_type() {
    let registry = registry();
    let password = registry.property("registry", "password").unwrap();
    assert!(password.is_sensitive());
    assert_eq!(password.mutability, Mutability::WriteOnly);
    assert!(password.is_required_on_create());

    let name = registry.property("registry", "name").unwrap();
    assert!(
        !name.is_required_on_create(),
        "a defaulted field is optional"
    );

    let server = registry.property("registry", "server").unwrap();
    assert_eq!(server.selector.as_deref(), Some("server"));
    assert!(server.is_selector() && server.nullable && !server.is_required_on_create());

    let app_name = registry.property("application", "app_name").unwrap();
    assert_eq!(app_name.mutability, Mutability::CreateOnly);
    assert!(!app_name.is_required_on_create(), "nullable");
}

#[test]
fn an_env_field_is_a_collection_with_entries_named_by_the_document() {
    let registry = registry();
    let root = registry.property("application", "environment").unwrap();
    assert_eq!(root.shape, PathShape::CollectionRoot);
    assert!(root.nullable && !root.is_required_on_create());

    let entry = registry
        .property("application", "environment.LOG_LEVEL")
        .unwrap();
    assert_eq!(entry.shape, PathShape::CollectionEntry);
    assert_eq!(entry.root.as_deref(), Some("environment"));
    assert_eq!(entry.ty, FieldType::Env);
    assert!(
        entry.is_sensitive(),
        "environment values are secret by default"
    );
    assert!(entry.nullable);

    for bad in [
        "environment.",
        "environment.1ABC",
        "environment.A-B",
        "environment. X",
    ] {
        assert!(
            matches!(
                registry.property("application", bad),
                Err(PathError::InvalidKey { .. } | PathError::Malformed(_))
            ),
            "{bad} should be rejected"
        );
    }
    let long = format!("environment.{}", "A".repeat(257));
    assert!(matches!(
        registry.property("application", &long),
        Err(PathError::InvalidKey { .. })
    ));
}

#[test]
fn a_union_is_one_value_and_its_arms_are_not_properties() {
    let registry = registry();
    let source = registry.property("application", "source").unwrap();
    assert_eq!(source.shape, PathShape::Atomic);
    assert!(matches!(source.ty, FieldType::Union { .. }));
    assert!(matches!(
        registry.property("application", "source.github.repository"),
        Err(PathError::UnionMember { .. })
    ));
}

#[test]
fn every_way_a_path_can_be_wrong_has_its_own_error() {
    let registry = registry();
    assert_eq!(
        registry.property("nope", "x"),
        Err(PathError::UnknownKind("nope".into()))
    );
    assert!(matches!(
        registry.property("redirect", "missing"),
        Err(PathError::UnknownField { .. })
    ));
    assert!(matches!(
        registry.property("redirect", "regex.deeper"),
        Err(PathError::NotComposite { .. })
    ));
    for malformed in ["", ".regex", "regex.", "a..b"] {
        assert!(
            matches!(
                registry.property("redirect", malformed),
                Err(PathError::Malformed(_))
            ),
            "`{malformed}` should be malformed"
        );
    }
}

#[test]
fn maps_structs_and_inherited_mutability() {
    let spec = rich();

    let entry = spec.property("labels.team.a").unwrap();
    assert_eq!(entry.shape, PathShape::CollectionEntry);
    assert_eq!(entry.ty, FieldType::Text);
    assert!(!entry.is_sensitive(), "map entries follow the field class");
    assert_eq!(entry.root.as_deref(), Some("labels"));

    assert_eq!(
        spec.property("limits"),
        Err(PathError::NeedsMember {
            prefix: "limits".into()
        })
    );
    let cpu = spec.property("limits.cpu").unwrap();
    assert_eq!(
        cpu.mutability,
        Mutability::CreateOnly,
        "inherited from the struct"
    );
    assert_eq!(cpu.rules.max, Some(64.0));
    assert!(matches!(
        spec.property("limits.gone"),
        Err(PathError::UnknownMember { .. })
    ));
    assert!(matches!(
        spec.property("limits.cpu.deeper"),
        Err(PathError::NotComposite { .. })
    ));

    // A struct without per-member granularity is one atomic value.
    assert_eq!(spec.property("tuning").unwrap().shape, PathShape::Atomic);
    assert!(matches!(
        spec.property("tuning.level"),
        Err(PathError::NotComposite { .. })
    ));
}

#[test]
fn properties_lists_every_addressable_path_without_keys() {
    let paths: Vec<String> = rich().properties().into_iter().map(|p| p.path).collect();
    assert_eq!(
        paths,
        [
            "labels",
            "limits.cpu",
            "limits.note",
            "mode",
            "name",
            "owner",
            "source",
            "tags",
            "token",
            "tuning"
        ]
    );

    let registry = registry();
    let redirect: Vec<String> = registry
        .get("redirect")
        .unwrap()
        .properties()
        .into_iter()
        .map(|p| p.path)
        .collect();
    assert_eq!(redirect, ["permanent", "regex", "replacement"]);

    let token = rich().property("token").unwrap();
    assert_eq!(token.class, ValueClass::Secret);
}

#[test]
fn every_listed_property_resolves_to_itself() {
    let registry = registry();
    for spec in registry.kinds() {
        for info in spec.properties() {
            assert_eq!(
                spec.property(&info.path).as_ref(),
                Ok(&info),
                "{}",
                info.path
            );
        }
    }
}
