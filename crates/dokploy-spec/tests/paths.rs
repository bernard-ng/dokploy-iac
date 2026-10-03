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
fn a_union_is_planned_per_member_by_its_tag_and_the_members_of_each_arm() {
    let registry = registry();

    let tag = registry.property("application", "source.type").unwrap();
    assert!(tag.union_tag && tag.arm.is_none());
    assert_eq!(tag.shape, PathShape::Atomic);
    assert!(matches!(&tag.ty, FieldType::Enum(arms) if arms.iter().any(|arm| arm == "github")));
    assert!(
        !tag.is_required_on_create(),
        "the create operation does not need it"
    );

    let owner = registry
        .property("application", "source.github.owner")
        .unwrap();
    assert_eq!(owner.arm.as_deref(), Some("github"));
    assert_eq!(owner.api, "owner");
    assert!(
        !owner.is_required_on_create(),
        "a member is needed only by its arm"
    );
    let password = registry
        .property("application", "source.docker.password")
        .unwrap();
    assert!(password.is_sensitive());

    // The union itself is not a property, and nothing outside its arms is a member.
    assert!(matches!(
        registry.property("application", "source"),
        Err(PathError::UnionMember { .. })
    ));
    assert!(matches!(
        registry.property("application", "source.github.nope"),
        Err(PathError::UnknownMember { .. })
    ));
    assert!(matches!(
        registry.property("application", "source.nope.owner"),
        Err(PathError::UnknownMember { .. })
    ));

    // Listing the properties gives the tag and every member of every arm.
    let spec = registry.get("application").unwrap();
    let paths: Vec<String> = spec.properties().into_iter().map(|p| p.path).collect();
    for expected in ["source.type", "source.github.owner", "source.docker.image"] {
        assert!(paths.iter().any(|path| path == expected), "{paths:?}");
    }
    assert!(!paths.iter().any(|path| path == "source"));
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
            "source.one.path",
            "source.type",
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

#[test]
fn a_set_of_selectors_is_a_collection_whose_entries_are_selectors_named_by_the_document() {
    let registry = registry();

    let root = registry.property("application", "networks").unwrap();
    assert_eq!(root.shape, PathShape::CollectionRoot);
    assert!(root.nullable, "`null` clears every attachment");
    assert_eq!(root.api, "networkIds");
    assert_eq!(root.ty.selector_set_kind(), Some("network"));

    let entry = registry
        .property("application", "networks.backend")
        .unwrap();
    assert_eq!(entry.shape, PathShape::CollectionEntry);
    assert_eq!(entry.root.as_deref(), Some("networks"));
    assert_eq!(entry.selector.as_deref(), Some("network"));
    assert_eq!(entry.ty, FieldType::Selector("network".into()));
    assert!(!entry.is_sensitive(), "a network name is not a secret");

    // A name may hold dots: everything after the field is the key.
    let dotted = registry.property("application", "networks.my.net").unwrap();
    assert_eq!(dotted.root.as_deref(), Some("networks"));
}

#[test]
fn a_relation_reads_the_ids_inside_the_objects_of_an_array() {
    let tags = registry().property("project", "tags").unwrap();

    assert_eq!(tags.shape, PathShape::CollectionRoot);
    assert_eq!(tags.api, "/projectTags/*/tagId");
    assert_eq!(tags.request_key(), "projectTags");
    let entry = registry().property("project", "tags.prod").unwrap();
    assert_eq!(entry.selector.as_deref(), Some("tag"));
}

#[test]
fn an_entry_of_a_map_of_sets_names_its_key_and_its_member_and_addresses_one_element() {
    let registry = registry();

    let root = registry.property("compose", "service_networks").unwrap();
    assert_eq!(root.shape, PathShape::CollectionRoot);
    assert!(root.ty.is_map_of_selector_sets());
    assert_eq!(root.request_key(), "serviceNetworks");

    let entry = registry
        .property("compose", "service_networks.api.backend")
        .unwrap();
    assert_eq!(entry.shape, PathShape::CollectionEntry);
    assert_eq!(entry.selector.as_deref(), Some("network"));
    assert_eq!(
        entry.api, "/serviceNetworks/*[serviceName=api]/networkIds",
        "the key of the entry is the element it reads and writes"
    );

    assert!(
        matches!(
            registry.property("compose", "service_networks.api"),
            Err(PathError::InvalidKey { .. })
        ),
        "a key alone names no entry"
    );
    for wrong in ["service_networks.api.", "service_networks..backend"] {
        assert!(
            registry.property("compose", wrong).is_err(),
            "`{wrong}` names no entry"
        );
    }
}

#[test]
fn a_relation_belongs_to_no_write_group_and_a_set_of_selectors_needs_a_key_granularity() {
    let broken = |fields: &str, write: &str| {
        let text = format!(
            r#"
kind: widget
scope: settings
section: widgets
title: Widget
identity: {{ key: name, collision: [name], address: "widget.{{key}}" }}
api:
  id: widgetId
  create: {{ op: tag.create }}
  update: {{ op: tag.update }}
  remove: {{ op: tag.remove }}
fields:
  name: {{ type: text, min_len: 1, default: key }}
{fields}
write:
  - {{ op: tag.update, fields: [{write}], shape: partial }}
"#
        );
        let spec = parse_spec(&text).expect("the test spec parses");
        dokploy_spec::validate_spec(&spec, &std::collections::BTreeSet::new())
    };

    // `granularity: key` only on a set of selectors, a map, an env, or a struct.
    let issues = broken(
        "  peers: { type: \"set<text>\", granularity: key }",
        "name, peers",
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue.to_string().contains("granularity")),
        "{issues:?}"
    );

    // A relation is changed by its own operations, so a write group may not carry it.
    let issues = broken(
        "  peers:\n    api: /peers/*/id\n    type: \"set<selector(tag)>\"\n    granularity: key\n    nullable: true\n    membership:\n      add: { op: tag.create, member: tagId }\n      remove: { op: tag.remove, member: tagId }",
        "name, peers",
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue.to_string().contains("no write group")),
        "{issues:?}"
    );
}
