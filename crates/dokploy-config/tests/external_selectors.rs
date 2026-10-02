use dokploy_config::{
    ApplicationDocument, ConfigDocument, ConfigError, DokployConfig, EnvironmentDocument,
    ExternalSelector, Field, SelectorKind, ValidationIssue, render,
};
use dokploy_state::{ResourceAddress, ResourceName};

const CANARY: &str = "edge-selector-canary-7c1d";

const FULL: &str = r#"
version: 1
project:
  name: platform
  environments:
    production:
      applications:
        api:
          server:
            name: edge-1
          build_server:
            name: builder
          registry:
            name: main
          build_registry: null
          rollback_registry:
            name: archive
        worker:
          server:
            local: true
        plain: {}
"#;

#[test]
fn parses_typed_local_named_and_cleared_selectors_with_field_ownership() {
    let config = DokployConfig::parse(FULL).expect("valid selectors");
    let api = application(&config, "application.api");

    assert_eq!(api.server(), &Field::Set(ExternalSelector::named("edge-1")));
    assert_eq!(
        api.build_server(),
        &Field::Set(ExternalSelector::named("builder"))
    );
    assert_eq!(api.registry(), &Field::Set(ExternalSelector::named("main")));
    assert_eq!(api.build_registry(), &Field::Clear);
    assert_eq!(
        api.rollback_registry(),
        &Field::Set(ExternalSelector::named("archive"))
    );

    let worker = application(&config, "application.worker");
    assert_eq!(worker.server(), &Field::Set(ExternalSelector::Local));
    assert_eq!(worker.registry(), &Field::Unmanaged);

    let plain = application(&config, "application.plain");
    assert_eq!(plain.server(), &Field::Unmanaged);
    assert_eq!(plain.build_server(), &Field::Unmanaged);
    assert_eq!(plain.rollback_registry(), &Field::Unmanaged);
}

#[test]
fn selector_vocabulary_distinguishes_local_from_a_record_named_local() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
  environments:
    production:
      applications:
        api:
          server:
            name: local
        worker:
          server:
            local: true
"#,
    )
    .expect("a record may literally be named local");

    let api = application(&config, "application.api");
    let worker = application(&config, "application.worker");
    assert_eq!(api.server().as_set().unwrap().name(), Some("local"));
    assert!(!api.server().as_set().unwrap().is_local());
    assert!(worker.server().as_set().unwrap().is_local());
    assert_eq!(worker.server().as_set().unwrap().name(), None);
    assert!(SelectorKind::Server.allows_local());
    assert!(!SelectorKind::Registry.allows_local());
    assert!(!SelectorKind::Destination.allows_local());
}

#[test]
fn malformed_selector_shapes_fail_with_location_only_parse_errors() {
    for body in [
        "server: edge-1",
        "server: {}",
        "server: { local: false }",
        "server: { local: null }",
        "server: { name: null }",
        "server: { local: true, name: edge-1 }",
        "server: { id: raw-external-id }",
        "server: { name: edge-1, extra: true }",
        "registry: [main]",
        "registry: 7",
        "build_server: edge",
    ] {
        let source = format!(
            "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api:\n          {body}\n"
        );
        let error = DokployConfig::parse(&source).expect_err("malformed selector");
        assert!(
            matches!(
                error,
                ConfigError::Parse { .. } | ConfigError::ParseWithoutLocation
            ),
            "{body}"
        );
    }
}

#[test]
fn semantic_selector_errors_have_stable_dokcfg_codes_and_never_echo_values() {
    let cases: [(&str, ValidationIssue, &str); 8] = [
        (
            "registry: { local: true }",
            ValidationIssue::LocalSelectorUnsupported,
            "DOKCFG030",
        ),
        (
            "build_server: { local: true }",
            ValidationIssue::LocalSelectorUnsupported,
            "DOKCFG030",
        ),
        (
            "build_registry: { local: true }",
            ValidationIssue::LocalSelectorUnsupported,
            "DOKCFG030",
        ),
        (
            "server: null",
            ValidationIssue::ServerPlacementCannotBeCleared,
            "DOKCFG031",
        ),
        (
            "server: { name: '' }",
            ValidationIssue::InvalidExternalSelectorName,
            "DOKCFG029",
        ),
        (
            "registry: { name: ' padded ' }",
            ValidationIssue::InvalidExternalSelectorName,
            "DOKCFG029",
        ),
        (
            "rollback_registry: { name: \"tab\\there\" }",
            ValidationIssue::InvalidExternalSelectorName,
            "DOKCFG029",
        ),
        (
            "build_server: { name: \"line\\nbreak\" }",
            ValidationIssue::InvalidExternalSelectorName,
            "DOKCFG029",
        ),
    ];

    for (body, issue, code) in cases {
        let source = format!(
            "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api:\n          {body}\n"
        );
        let error = DokployConfig::parse(&source).expect_err(body);
        assert!(error.issues().contains(&issue), "{body}");
        assert_eq!(issue.code(), code);
        assert!(!issue.message().contains(CANARY));
    }

    let over_long = format!(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api:\n          server: {{ name: \"{}\" }}\n",
        "x".repeat(257)
    );
    let error = DokployConfig::parse(&over_long).expect_err("over-long name");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::InvalidExternalSelectorName)
    );

    let named = format!(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api:\n          registry: {{ local: true }}\n          server: {{ name: {CANARY} }}\n"
    );
    let error = DokployConfig::parse(&named).expect_err("local registry");
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains(CANARY));
}

#[test]
fn debug_output_redacts_selector_names() {
    let config = DokployConfig::parse(&format!(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      applications:\n        api:\n          server: {{ name: {CANARY} }}\n"
    ))
    .expect("valid");
    let api = application(&config, "application.api");

    let rendered = format!("{config:?} {api:?} {:?}", api.server());
    assert!(!rendered.contains(CANARY));
    assert!(format!("{:?}", ExternalSelector::Local).contains("Local"));
}

#[test]
fn selector_properties_can_be_ignored_by_lifecycle_rules() {
    DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
  environments:
    production:
      applications:
        api:
          server:
            name: edge-1
          registry:
            name: main
          lifecycle:
            ignore_changes: [server, registry, build_server, build_registry, rollback_registry]
"#,
    )
    .expect("selector paths are ignorable");
}

#[test]
fn external_selectors_are_listed_with_their_kinds() {
    let config = DokployConfig::parse(FULL).expect("valid selectors");
    let address: ResourceAddress = "application.api".parse().unwrap();
    let selectors = config.resource(&address).unwrap().external_selectors();

    let kinds: Vec<_> = selectors
        .iter()
        .map(|(name, kind, _)| (*name, *kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("server", SelectorKind::Server),
            ("build_server", SelectorKind::Server),
            ("registry", SelectorKind::Registry),
            ("rollback_registry", SelectorKind::Registry),
        ]
    );
}

#[test]
fn canonical_writer_round_trips_selectors_deterministically() {
    let config = DokployConfig::parse(FULL).expect("valid selectors");
    let first = render(&config).expect("renders");
    let reparsed = DokployConfig::parse(&first).expect("reparses");

    assert_eq!(reparsed, config);
    assert_eq!(render(&reparsed).expect("renders again"), first);
    assert!(first.contains("server:\n"));
    assert!(first.contains("local: true"));
    assert!(first.contains("build_registry: null"));
}

#[test]
fn typed_import_document_emits_selectors_and_rejects_invalid_ones_through_the_parser() {
    let mut document = ConfigDocument::new("platform".parse::<ResourceName>().unwrap());
    let mut production = EnvironmentDocument::default();
    production
        .add_application(
            "api".parse::<ResourceName>().unwrap(),
            ApplicationDocument {
                server: Field::Set(ExternalSelector::named("edge-1")),
                registry: Field::Set(ExternalSelector::named("main")),
                build_registry: Field::Clear,
                ..ApplicationDocument::default()
            },
        )
        .expect("unique");
    document
        .add_environment("production".parse::<ResourceName>().unwrap(), production)
        .expect("unique");
    let rendered = document.render().expect("valid document renders");
    let reparsed = DokployConfig::parse(&rendered).expect("round trips");
    let api = application(&reparsed, "application.api");
    assert_eq!(api.server(), &Field::Set(ExternalSelector::named("edge-1")));
    assert_eq!(api.build_registry(), &Field::Clear);

    let mut invalid = ConfigDocument::new("platform".parse::<ResourceName>().unwrap());
    let mut production = EnvironmentDocument::default();
    production
        .add_application(
            "api".parse::<ResourceName>().unwrap(),
            ApplicationDocument {
                registry: Field::Set(ExternalSelector::Local),
                ..ApplicationDocument::default()
            },
        )
        .expect("unique");
    invalid
        .add_environment("production".parse::<ResourceName>().unwrap(), production)
        .expect("unique");
    assert!(invalid.render().is_err(), "writer revalidates selectors");
}

#[test]
fn generated_schema_models_closed_selector_objects() {
    let schema = serde_json::to_value(DokployConfig::json_schema()).unwrap();
    let application = &schema["$defs"]["RawApplication"]["properties"];

    for field in [
        "server",
        "build_server",
        "registry",
        "build_registry",
        "rollback_registry",
    ] {
        assert!(application.get(field).is_some(), "{field}");
    }
    let selector = &schema["$defs"]["ExternalSelector"]["oneOf"];
    assert_eq!(selector.as_array().unwrap().len(), 2);
    assert_eq!(selector[0]["properties"]["local"]["const"], true);
    assert_eq!(selector[0]["additionalProperties"], false);
    assert_eq!(selector[1]["properties"]["name"]["minLength"], 1);
    assert_eq!(selector[1]["properties"]["name"]["maxLength"], 256);
    assert_eq!(selector[1]["additionalProperties"], false);
}

fn application<'a>(
    config: &'a DokployConfig,
    address: &str,
) -> &'a dokploy_config::ApplicationConfig {
    config
        .resource(&address.parse::<ResourceAddress>().unwrap())
        .unwrap()
        .as_application()
        .unwrap()
}
