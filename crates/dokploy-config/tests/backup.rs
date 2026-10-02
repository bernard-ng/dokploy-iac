use dokploy_config::{
    ApplicationDocument, BackupDocument, ConfigDocument, DokployConfig, EnvironmentDocument,
    ExternalSelector, Field, LifecycleDocument, SelectorKind, ValidationIssue, render,
};
use dokploy_state::{ResourceAddress, ResourceKind, ResourceName};

const CANARY: &str = "backup-destination-canary-5b1e";

const BASE: &str = r#"
version: 1
project:
  name: platform
  environments:
    production:
      applications:
        api: {}
      compose:
        stack:
          document:
            env: STACK_DOCUMENT
      postgres:
        main: {}
      mysql:
        sql: {}
      mariadb:
        maria: {}
      mongo:
        docs: {}
      libsql:
        edge: {}
      redis:
        cache: {}
    staging:
      postgres:
        preview: {}
"#;

/// Indents a YAML fragment two spaces so it nests under `project.environments`.
fn nest(fragment: &str) -> String {
    fragment.lines().map(|line| format!("  {line}\n")).collect()
}

fn with_backups(backups: &str) -> String {
    BASE.replace(
        "    staging:",
        &format!("      backups:\n{}    staging:", nest(backups)),
    )
}

fn one_backup(body: &str) -> String {
    with_backups(&format!("      nightly:\n{body}"))
}

const VALID: &str = "        target: postgres.main\n        destination: { name: offsite }\n        schedule: \"0 3 * * *\"\n        prefix: nightly\n        database: app\n";

fn valid_with(extra: &str) -> String {
    one_backup(&format!("{VALID}{extra}"))
}

fn backup_address(name: &str) -> ResourceAddress {
    format!("backup.{name}").parse().unwrap()
}

#[test]
fn parses_every_supported_database_target_with_owned_fields_and_defaults() {
    let config = DokployConfig::parse(&with_backups(
        r#"      pg:
        target: postgres.main
        destination: { name: offsite }
        schedule: "0 3 * * *"
        prefix: pg
        database: app
        enabled: false
        keep_latest: 7
        include_encryption_key: true
      my:
        target: mysql.sql
        destination: { name: offsite }
        schedule: "*/5 * * * *"
        prefix: my
        database: app
      ma:
        target: mariadb.maria
        destination: { name: offsite }
        schedule: "0 0 * * 0"
        prefix: ma
        database: app
        keep_latest: null
      mo:
        target: mongo.docs
        destination: { name: other }
        schedule: "0 0 * * 0 0"
        prefix: mo
        database: app
      ls:
        target: libsql.edge
        destination: { name: offsite }
        schedule: "0 0 1 * *"
        prefix: ls
        database: app
"#,
    ))
    .expect("typed Backup configuration is valid");

    let pg = config
        .resource(&backup_address("pg"))
        .and_then(|resource| resource.as_backup())
        .expect("Backup exists");
    assert_eq!(pg.target().to_string(), "postgres.main");
    assert_eq!(pg.destination(), &ExternalSelector::named("offsite"));
    assert_eq!(pg.schedule(), "0 3 * * *");
    assert_eq!(pg.prefix(), "pg");
    assert_eq!(pg.database(), "app");
    assert!(!pg.enabled());
    assert_eq!(pg.keep_latest(), &Field::Set(7));
    assert!(pg.include_encryption_key());
    assert_eq!(
        config.parent_of(&backup_address("pg")),
        Some(&"environment.production".parse().unwrap()),
        "a Backup is contained by its environment, not by its target"
    );

    let my = config
        .resource(&backup_address("my"))
        .unwrap()
        .as_backup()
        .unwrap();
    assert!(my.enabled(), "enabled defaults to true");
    assert_eq!(my.keep_latest(), &Field::Unmanaged);
    assert!(!my.include_encryption_key());
    let ma = config
        .resource(&backup_address("ma"))
        .unwrap()
        .as_backup()
        .unwrap();
    assert_eq!(ma.keep_latest(), &Field::Clear);

    let selectors = config
        .resource(&backup_address("pg"))
        .unwrap()
        .external_selectors();
    assert_eq!(selectors.len(), 1);
    assert_eq!(selectors[0].0, "destination");
    assert_eq!(selectors[0].1, SelectorKind::Destination);
}

#[test]
fn rejects_targets_outside_the_closed_same_environment_database_union() {
    for (target, expected) in [
        ("compose.stack", ValidationIssue::InvalidBackupTarget),
        ("application.api", ValidationIssue::InvalidBackupTarget),
        ("redis.cache", ValidationIssue::InvalidBackupTarget),
        ("project.platform", ValidationIssue::InvalidBackupTarget),
        ("backup.nightly", ValidationIssue::InvalidBackupTarget),
        ("postgres.missing", ValidationIssue::MissingReference),
        (
            "postgres.preview",
            ValidationIssue::CrossEnvironmentReference,
        ),
    ] {
        let error = DokployConfig::parse(&one_backup(&VALID.replace("postgres.main", target)))
            .expect_err("Backup target must be a same-environment supported database");
        assert!(error.issues().contains(&expected), "{target}: {error:?}");
    }
}

#[test]
fn destination_is_a_required_name_selector_and_local_is_rejected() {
    let error = DokployConfig::parse(&one_backup(
        &VALID.replace("{ name: offsite }", "{ local: true }"),
    ))
    .expect_err("local is not a backup destination");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::LocalSelectorUnsupported)
    );

    for destination in [
        "{ name: \"\" }",
        "{ name: \" padded\" }",
        "{ name: \"trailing \" }",
    ] {
        let error = DokployConfig::parse(&one_backup(
            &VALID.replace("{ name: offsite }", destination),
        ))
        .expect_err("destination names are validated");
        assert!(
            error
                .issues()
                .contains(&ValidationIssue::InvalidExternalSelectorName),
            "{destination}"
        );
    }

    // Shape errors are strict parse errors and never echo the offending value.
    for destination in [
        "null",
        "offsite",
        "{}",
        "{ name: a, local: true }",
        "{ id: raw-identity }",
        "{ name: null }",
        &format!("{{ name: {CANARY}, extra: 1 }}"),
    ] {
        let error = DokployConfig::parse(&one_backup(
            &VALID.replace("{ name: offsite }", destination),
        ))
        .expect_err("destination shape is closed");
        assert!(matches!(error, dokploy_config::ConfigError::Parse { .. }));
        assert!(!format!("{error:?}{error}").contains(CANARY));
    }

    let missing = one_backup(
        "        target: postgres.main\n        schedule: \"0 3 * * *\"\n        prefix: nightly\n        database: app\n",
    );
    assert!(DokployConfig::parse(&missing).is_err());
}

#[test]
fn rejects_unsafe_schedules_prefixes_databases_and_retention() {
    for (field, value) in [
        ("schedule", "\"\""),
        ("schedule", "\"* * * *\""),
        ("schedule", "\"* * * * * * *\""),
        ("schedule", "\"@daily\""),
        ("schedule", "\"0  3 * * *\""),
        ("schedule", "\"0 3 * * *\\n\""),
        ("schedule", "\"0 3 * * $(x)\""),
        ("prefix", "\"\""),
        ("prefix", "\"a\\u0000b\""),
        ("database", "\"\""),
        ("database", "\"a\\nb\""),
    ] {
        let body = VALID
            .lines()
            .map(|line| {
                if line.trim_start().starts_with(&format!("{field}:")) {
                    format!("        {field}: {value}\n")
                } else {
                    format!("{line}\n")
                }
            })
            .collect::<String>();
        let error = DokployConfig::parse(&one_backup(&body))
            .expect_err("unsafe Backup fields are rejected");
        assert!(
            error
                .issues()
                .contains(&ValidationIssue::InvalidBackupField),
            "{field}={value}: {error:?}"
        );
    }

    let error = DokployConfig::parse(&valid_with("        keep_latest: 0\n"))
        .expect_err("a retention count of zero is invalid");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::InvalidBackupField)
    );
    for body in [
        "        keep_latest: -1\n",
        "        keep_latest: many\n",
        "        enabled: null\n",
        "        enabled: maybe\n",
        "        include_encryption_key: null\n",
        "        extra: true\n",
    ] {
        assert!(DokployConfig::parse(&valid_with(body)).is_err(), "{body}");
    }
}

#[test]
fn rejects_duplicate_collision_keys_but_allows_distinguishing_components() {
    let error = DokployConfig::parse(&with_backups(
        r#"      first:
        target: postgres.main
        destination: { name: offsite }
        schedule: "0 3 * * *"
        prefix: nightly
        database: app
      second:
        target: postgres.main
        destination: { name: offsite }
        schedule: "0 4 * * *"
        prefix: " /nightly/ "
        database: app
        enabled: false
"#,
    ))
    .expect_err("the runtime-normalized prefix is part of the collision key");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::DuplicateBackupCollision)
    );

    DokployConfig::parse(&with_backups(
        r#"      base:
        target: postgres.main
        destination: { name: offsite }
        schedule: "0 3 * * *"
        prefix: nightly
        database: app
      other-destination:
        target: postgres.main
        destination: { name: other }
        schedule: "0 3 * * *"
        prefix: nightly
        database: app
      other-prefix:
        target: postgres.main
        destination: { name: offsite }
        schedule: "0 3 * * *"
        prefix: weekly
        database: app
      other-database:
        target: postgres.main
        destination: { name: offsite }
        schedule: "0 3 * * *"
        prefix: nightly
        database: other
      other-target:
        target: mysql.sql
        destination: { name: offsite }
        schedule: "0 3 * * *"
        prefix: nightly
        database: app
"#,
    ))
    .expect("each differing collision component yields a distinct backup");
}

#[test]
fn backup_ignored_changes_are_limited_to_safe_in_place_properties() {
    DokployConfig::parse(&valid_with(
        "        lifecycle: { ignore_changes: [schedule, enabled, keep_latest, include_encryption_key] }\n",
    ))
    .expect("in-place non-collision properties can be ignored");

    for path in ["target", "destination", "prefix", "database", "password"] {
        let error = DokployConfig::parse(&valid_with(&format!(
            "        lifecycle: {{ ignore_changes: [{path}] }}\n"
        )))
        .expect_err("replacement and collision-key paths cannot be ignored");
        assert!(
            error
                .issues()
                .contains(&ValidationIssue::InvalidIgnoredChange),
            "{path}"
        );
    }
}

#[test]
fn backup_issue_codes_are_stable_and_message_safe() {
    for (issue, code) in [
        (ValidationIssue::InvalidBackupTarget, "DOKCFG060"),
        (ValidationIssue::InvalidBackupField, "DOKCFG061"),
        (ValidationIssue::DuplicateBackupCollision, "DOKCFG062"),
    ] {
        assert_eq!(issue.code(), code);
        assert!(issue.to_string().starts_with(code));
    }
}

#[test]
fn debug_output_redacts_backup_values() {
    let config = DokployConfig::parse(&one_backup(
        &VALID
            .replace("offsite", CANARY)
            .replace("nightly", "prefix-canary")
            .replace("app", "database-canary"),
    ))
    .unwrap();
    let resource = config.resource(&backup_address("nightly")).unwrap();
    let backup = resource.as_backup().unwrap();

    for debug in [
        format!("{resource:?}"),
        format!("{backup:?}"),
        format!("{:?}", backup.destination()),
    ] {
        for canary in [CANARY, "prefix-canary", "database-canary"] {
            assert!(!debug.contains(canary), "{debug}");
        }
    }
}

#[test]
fn generated_schema_models_the_closed_backup_shape() {
    let schema = serde_json::to_value(DokployConfig::json_schema()).unwrap();
    let backup = &schema["$defs"]["RawBackup"];

    assert_eq!(backup["additionalProperties"], false);
    assert_eq!(
        backup["required"],
        serde_json::json!(["target", "destination", "schedule", "prefix", "database"])
    );
    let target = backup["properties"]["target"]["pattern"].as_str().unwrap();
    assert!(target.contains("postgres|mysql|mariadb|mongo|libsql"));
    assert!(!target.contains("compose") && !target.contains("redis"));
    assert!(
        schema["$defs"]["ExternalSelector"]["oneOf"].is_array(),
        "the destination reuses the closed selector schema"
    );
    assert_eq!(
        backup["properties"]["destination"]["$ref"],
        "#/$defs/ExternalSelector"
    );
    assert!(
        schema["$defs"]["RawEnvironment"]["properties"]["backups"].is_object(),
        "backups are an environment collection"
    );
}

#[test]
fn canonical_writer_round_trips_every_backup_field() {
    let source = with_backups(
        r#"      pg:
        target: postgres.main
        destination: { name: offsite }
        schedule: "0 3 * * *"
        prefix: pg
        database: app
        enabled: false
        keep_latest: 7
        include_encryption_key: true
        depends_on:
          - mysql.sql
        lifecycle:
          protect: true
          ignore_changes: [schedule]
      cleared:
        target: mongo.docs
        destination: { name: other }
        schedule: "0 0 * * 0"
        prefix: mo
        database: app
        keep_latest: null
      plain:
        target: libsql.edge
        destination: { name: offsite }
        schedule: "0 0 1 * *"
        prefix: ls
        database: app
"#,
    );
    let config = DokployConfig::parse(&source).expect("fixture is valid");

    let rendered = render(&config).expect("Backup configuration renders");

    assert_eq!(DokployConfig::parse(&rendered).unwrap(), config);
    assert!(rendered.contains("      backups:\n        cleared:\n"));
    assert!(rendered.contains("          destination:\n            name: \"offsite\"\n"));
    assert!(rendered.contains("          keep_latest: null\n"));
    assert!(rendered.contains("          enabled: false\n"));
    assert!(rendered.contains("          include_encryption_key: true\n"));
    assert_eq!(
        ConfigDocument::from_config(&config)
            .unwrap()
            .render()
            .unwrap(),
        rendered
    );
}

#[test]
fn typed_document_writes_imported_backup_with_a_name_selector() {
    let name = |value: &str| value.parse::<ResourceName>().unwrap();
    let mut environment = EnvironmentDocument::default();
    environment
        .add_application(name("api"), ApplicationDocument::default())
        .unwrap();
    let backup = || BackupDocument {
        target: ResourceAddress::new(ResourceKind::Postgres, name("main")),
        destination: ExternalSelector::named("offsite"),
        schedule: "0 3 * * *".to_owned(),
        prefix: "nightly".to_owned(),
        database: "app".to_owned(),
        enabled: false,
        keep_latest: Field::Set(3),
        include_encryption_key: false,
        depends_on: Vec::new(),
        lifecycle: LifecycleDocument {
            protect: Field::Set(true),
            ..LifecycleDocument::default()
        },
    };
    environment.add_backup(name("nightly"), backup()).unwrap();
    assert!(
        environment.add_backup(name("nightly"), backup()).is_err(),
        "duplicate Backup names are rejected"
    );
    let mut document = ConfigDocument::new(name("platform"));
    document
        .add_environment(name("production"), environment)
        .unwrap();

    let error = document.render().expect_err("target must exist");
    assert!(!error.to_string().contains("offsite"));

    let mut environment = EnvironmentDocument::default();
    environment
        .add_postgres(name("main"), Default::default())
        .unwrap();
    environment.add_backup(name("nightly"), backup()).unwrap();
    let mut document = ConfigDocument::new(name("platform"));
    document
        .add_environment(name("production"), environment)
        .unwrap();

    let rendered = document.render().expect("imported Backup renders");

    assert!(rendered.contains("          destination:\n            name: \"offsite\"\n"));
    assert!(rendered.contains("          keep_latest: 3\n"));
    assert!(rendered.contains("          lifecycle:\n            protect: true\n"));
    DokployConfig::parse(&rendered).expect("rendered import document parses strictly");
}

#[test]
fn writer_rejects_a_destination_the_parser_would_reject() {
    let name = |value: &str| value.parse::<ResourceName>().unwrap();
    let mut environment = EnvironmentDocument::default();
    environment
        .add_postgres(name("main"), Default::default())
        .unwrap();
    environment
        .add_backup(
            name("nightly"),
            BackupDocument {
                target: ResourceAddress::new(ResourceKind::Postgres, name("main")),
                destination: ExternalSelector::named(format!(" {CANARY}")),
                schedule: "0 3 * * *".to_owned(),
                prefix: "nightly".to_owned(),
                database: "app".to_owned(),
                enabled: true,
                keep_latest: Field::Unmanaged,
                include_encryption_key: false,
                depends_on: Vec::new(),
                lifecycle: LifecycleDocument::default(),
            },
        )
        .unwrap();
    let mut document = ConfigDocument::new(name("platform"));
    document
        .add_environment(name("production"), environment)
        .unwrap();

    let error = document.render().expect_err("padded names are invalid");

    assert!(!format!("{error:?}{error}").contains(CANARY));
}
