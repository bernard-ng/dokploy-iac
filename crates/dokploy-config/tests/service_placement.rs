use dokploy_config::{
    ComposeDocument, ConfigDocument, ConfigError, DokployConfig, EnvironmentDocument,
    ExternalSelector, Field, LibSqlDocument, MariaDbDocument, MongoDocument, MySqlDocument,
    PostgresDocument, RedisDocument, SecretSource, SelectorKind, ValidationIssue, render,
};
use dokploy_state::{ResourceAddress, ResourceName};

const CANARY: &str = "edge-placement-canary-3b9e";

/// Every service kind that Dokploy v0.30.6 lets a create call place on a server.
const SECTIONS: [(&str, &str); 7] = [
    ("compose", "compose"),
    ("postgres", "postgres"),
    ("mysql", "mysql"),
    ("mariadb", "mariadb"),
    ("mongo", "mongo"),
    ("libsql", "libsql"),
    ("redis", "redis"),
];

fn source(section: &str, body: &str) -> String {
    format!(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      {section}:\n        main:\n          lifecycle:\n            protect: true\n          {body}\n"
    )
}

fn address(section: &str) -> ResourceAddress {
    format!("{section}.main").parse().unwrap()
}

#[test]
fn every_supported_service_kind_parses_local_named_and_unmanaged_server_placement() {
    for (section, prefix) in SECTIONS {
        let named =
            DokployConfig::parse(&source(section, "server: { name: edge-1 }")).expect(section);
        let resource = named.resource(&address(prefix)).unwrap();
        assert_eq!(
            resource.server_placement(),
            Some(&Field::Set(ExternalSelector::named("edge-1"))),
            "{section}"
        );

        let local =
            DokployConfig::parse(&source(section, "server: { local: true }")).expect(section);
        assert_eq!(
            local.resource(&address(prefix)).unwrap().server_placement(),
            Some(&Field::Set(ExternalSelector::Local)),
            "{section}"
        );

        let literal =
            DokployConfig::parse(&source(section, "server: { name: local }")).expect(section);
        let selector = literal
            .resource(&address(prefix))
            .unwrap()
            .server_placement()
            .and_then(Field::as_set)
            .unwrap();
        assert!(!selector.is_local());
        assert_eq!(selector.name(), Some("local"));

        let omitted = DokployConfig::parse(&source(section, "depends_on: []")).expect(section);
        assert_eq!(
            omitted
                .resource(&address(prefix))
                .unwrap()
                .server_placement(),
            Some(&Field::Unmanaged),
            "{section}"
        );
    }
}

#[test]
fn typed_accessors_expose_the_same_field_for_each_kind() {
    let text = "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      compose:\n        main:\n          server: { name: edge-1 }\n          lifecycle: { protect: true }\n      postgres:\n        main:\n          server: { name: edge-1 }\n      mysql:\n        main:\n          server: { name: edge-1 }\n      mariadb:\n        main:\n          server: { name: edge-1 }\n      mongo:\n        main:\n          server: { name: edge-1 }\n      libsql:\n        main:\n          server: { name: edge-1 }\n      redis:\n        main:\n          server: { name: edge-1 }\n";
    let config = DokployConfig::parse(text).expect("valid");
    let expected = Field::Set(ExternalSelector::named("edge-1"));
    let get = |section: &str| config.resource(&address(section)).unwrap();
    assert_eq!(get("compose").as_compose().unwrap().server(), &expected);
    assert_eq!(get("postgres").as_postgres().unwrap().server(), &expected);
    assert_eq!(get("mysql").as_mysql().unwrap().server(), &expected);
    assert_eq!(get("mariadb").as_mariadb().unwrap().server(), &expected);
    assert_eq!(get("mongo").as_mongo().unwrap().server(), &expected);
    assert_eq!(get("libsql").as_libsql().unwrap().server(), &expected);
    assert_eq!(get("redis").as_redis().unwrap().server(), &expected);
}

#[test]
fn malformed_service_selector_shapes_fail_with_location_only_parse_errors() {
    for (section, _) in SECTIONS {
        for body in [
            "server: edge-1",
            "server: {}",
            "server: { local: false }",
            "server: { name: null }",
            "server: { local: true, name: edge-1 }",
            "server: { id: raw-external-id }",
            "server: [edge-1]",
        ] {
            let error = DokployConfig::parse(&source(section, body)).expect_err(body);
            assert!(
                matches!(
                    error,
                    ConfigError::Parse { .. } | ConfigError::ParseWithoutLocation
                ),
                "{section}: {body}"
            );
        }
    }
}

#[test]
fn service_server_diagnostics_reuse_the_selector_codes_and_never_echo_values() {
    for (section, _) in SECTIONS {
        let null = DokployConfig::parse(&source(section, "server: null")).expect_err(section);
        assert!(
            null.issues()
                .contains(&ValidationIssue::ServerPlacementCannotBeCleared),
            "{section}"
        );
        for (body, issue) in [
            (
                "server: { name: '' }",
                ValidationIssue::InvalidExternalSelectorName,
            ),
            (
                "server: { name: ' padded ' }",
                ValidationIssue::InvalidExternalSelectorName,
            ),
            (
                "server: { name: \"tab\\there\" }",
                ValidationIssue::InvalidExternalSelectorName,
            ),
        ] {
            let error = DokployConfig::parse(&source(section, body)).expect_err(body);
            assert!(error.issues().contains(&issue), "{section}: {body}");
        }
        let long = format!("server: {{ name: \"{}\" }}", "x".repeat(257));
        let error = DokployConfig::parse(&source(section, &long)).expect_err("long");
        assert!(
            error
                .issues()
                .contains(&ValidationIssue::InvalidExternalSelectorName)
        );

        let bad = format!("server: {{ name: \" {CANARY} \" }}");
        let error = DokployConfig::parse(&source(section, &bad)).expect_err("padded");
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains(CANARY), "{section}");
    }
    assert_eq!(
        ValidationIssue::ServerPlacementCannotBeCleared.code(),
        "DOKCFG031"
    );
    assert_eq!(
        ValidationIssue::InvalidExternalSelectorName.code(),
        "DOKCFG029"
    );
}

#[test]
fn service_server_can_be_ignored_by_lifecycle_rules() {
    for (section, _) in SECTIONS {
        let text = format!(
            "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      {section}:\n        main:\n          server: {{ name: edge-1 }}\n          lifecycle:\n            protect: true\n            ignore_changes: [server]\n"
        );
        DokployConfig::parse(&text).unwrap_or_else(|error| panic!("{section}: {error:?}"));
    }
}

#[test]
fn external_selectors_list_the_service_server_with_its_kind() {
    for (section, _) in SECTIONS {
        let config =
            DokployConfig::parse(&source(section, "server: { name: edge-1 }")).expect(section);
        let selectors = config
            .resource(&address(section))
            .unwrap()
            .external_selectors();
        assert_eq!(selectors.len(), 1, "{section}");
        assert_eq!(selectors[0].0, "server");
        assert_eq!(selectors[0].1, SelectorKind::Server);
        assert_eq!(selectors[0].2.name(), Some("edge-1"));

        let local =
            DokployConfig::parse(&source(section, "server: { local: true }")).expect(section);
        let selectors = local
            .resource(&address(section))
            .unwrap()
            .external_selectors();
        assert_eq!(selectors.len(), 1, "{section}");
        assert!(selectors[0].2.is_local());
    }
}

#[test]
fn debug_output_redacts_service_selector_names() {
    for (section, _) in SECTIONS {
        let text = source(section, &format!("server: {{ name: {CANARY} }}"));
        let config = DokployConfig::parse(&text).expect(section);
        let resource = config.resource(&address(section)).unwrap();
        let rendered = format!("{config:?} {resource:?} {:?}", resource.server_placement());
        assert!(!rendered.contains(CANARY), "{section}");
    }
}

#[test]
fn canonical_writer_round_trips_service_selectors_deterministically() {
    let text = "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      compose:\n        app:\n          server: { name: edge-1 }\n          lifecycle: { protect: true }\n      postgres:\n        main:\n          server: { local: true }\n      mysql:\n        main:\n          server: { name: edge-2 }\n      mariadb:\n        main:\n          server: { name: edge-3 }\n      mongo:\n        main:\n          server: { name: edge-4 }\n      libsql:\n        main:\n          server: { name: edge-5 }\n      redis:\n        main:\n          server: { name: edge-6 }\n";
    let config = DokployConfig::parse(text).expect("valid");
    let first = render(&config).expect("renders");
    let reparsed = DokployConfig::parse(&first).expect("reparses");
    assert_eq!(reparsed, config);
    assert_eq!(render(&reparsed).expect("renders again"), first);
    assert!(first.contains("local: true"));
    assert_eq!(first.matches("server:\n").count(), 7);
}

#[test]
fn typed_import_documents_emit_service_selectors_and_revalidate_through_the_parser() {
    let selector = Field::Set(ExternalSelector::named("edge-1"));
    let mut document = ConfigDocument::new("platform".parse::<ResourceName>().unwrap());
    let mut production = EnvironmentDocument::default();
    let name = |value: &str| value.parse::<ResourceName>().unwrap();
    production
        .add_compose(
            name("app"),
            ComposeDocument {
                server: selector.clone(),
                document: Field::Set(SecretSource::Env("COMPOSE_DOCUMENT".to_owned())),
                ..ComposeDocument::default()
            },
        )
        .unwrap();
    production
        .add_postgres(
            name("pg"),
            PostgresDocument {
                server: selector.clone(),
                ..PostgresDocument::default()
            },
        )
        .unwrap();
    production
        .add_mysql(
            name("my"),
            MySqlDocument {
                server: selector.clone(),
                ..MySqlDocument::default()
            },
        )
        .unwrap();
    production
        .add_mariadb(
            name("maria"),
            MariaDbDocument {
                server: selector.clone(),
                ..MariaDbDocument::default()
            },
        )
        .unwrap();
    production
        .add_mongo(
            name("mongo"),
            MongoDocument {
                server: selector.clone(),
                ..MongoDocument::default()
            },
        )
        .unwrap();
    production
        .add_libsql(
            name("lib"),
            LibSqlDocument {
                server: Field::Set(ExternalSelector::Local),
                ..LibSqlDocument::default()
            },
        )
        .unwrap();
    production
        .add_redis(
            name("cache"),
            RedisDocument {
                server: selector.clone(),
                ..RedisDocument::default()
            },
        )
        .unwrap();
    document
        .add_environment(name("production"), production)
        .unwrap();

    let rendered = document.render().expect("valid document renders");
    let reparsed = DokployConfig::parse(&rendered).expect("round trips");
    for (address, expected) in [
        ("compose.app", Some("edge-1")),
        ("postgres.pg", Some("edge-1")),
        ("mysql.my", Some("edge-1")),
        ("mariadb.maria", Some("edge-1")),
        ("mongo.mongo", Some("edge-1")),
        ("libsql.lib", None),
        ("redis.cache", Some("edge-1")),
    ] {
        let resource = reparsed.resource(&address.parse().unwrap()).unwrap();
        let selector = resource.server_placement().and_then(Field::as_set).unwrap();
        assert_eq!(selector.name(), expected, "{address}");
    }

    let mut invalid = ConfigDocument::new("platform".parse::<ResourceName>().unwrap());
    let mut production = EnvironmentDocument::default();
    production
        .add_redis(
            name("cache"),
            RedisDocument {
                server: Field::Set(ExternalSelector::named(" padded ")),
                ..RedisDocument::default()
            },
        )
        .unwrap();
    invalid
        .add_environment(name("production"), production)
        .unwrap();
    assert!(invalid.render().is_err(), "writer revalidates selectors");

    let mut cleared = ConfigDocument::new("platform".parse::<ResourceName>().unwrap());
    let mut production = EnvironmentDocument::default();
    production
        .add_redis(
            name("cache"),
            RedisDocument {
                server: Field::Clear,
                ..RedisDocument::default()
            },
        )
        .unwrap();
    cleared
        .add_environment(name("production"), production)
        .unwrap();
    assert!(cleared.render().is_err(), "null placement is rejected");
}

#[test]
fn generated_schema_models_the_server_field_on_every_service_kind() {
    let schema = serde_json::to_value(DokployConfig::json_schema()).unwrap();
    for definition in [
        "RawCompose",
        "RawPostgres",
        "RawMySql",
        "RawMariaDb",
        "RawMongo",
        "RawLibSql",
        "RawRedis",
    ] {
        let property = &schema["$defs"][definition]["properties"]["server"];
        assert!(
            property.get("$ref").is_some()
                || property.get("oneOf").is_some()
                || property.get("anyOf").is_some(),
            "{definition}: {property}"
        );
    }
    assert_eq!(
        schema["$defs"]["ExternalSelector"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
