use dokploy_cli::desired::compile_desired;
use dokploy_config::{DokployConfig, SelectorKind};
use dokploy_core::{ConfigDigest, OwnedValue, PropertyPath};

const CANARY: &str = "backup-destination-name-canary";

fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).unwrap()
}

fn config(backups: &str) -> DokployConfig {
    DokployConfig::parse(&format!(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main: {{}}
    mysql:
      sql: {{}}
    backups:
{backups}
"#
    ))
    .expect("Backup configuration is valid")
}

fn json_value(value: serde_json::Value) -> OwnedValue {
    OwnedValue::Value(dokploy_core::ComparableValue::try_from_json(value).unwrap())
}

#[test]
fn compiles_typed_backup_ownership_dependency_and_environment_containment() {
    let config = config(
        r#"      nightly:
        target: postgres.main
        destination: { name: offsite }
        schedule: "0 3 * * *"
        prefix: nightly
        database: app
        enabled: false
        keep_latest: 7
        include_encryption_key: true
      cleared:
        target: mysql.sql
        destination: { name: offsite }
        schedule: "5 4 * * 1"
        prefix: weekly
        database: app
        keep_latest: null
      plain:
        target: mysql.sql
        destination: { name: offsite }
        schedule: "5 4 * * 2"
        prefix: other
        database: app
"#,
    );

    let compiled = compile_desired(&config, digest()).expect("offline Backup compilation succeeds");
    let resources = compiled.desired_state().resources();

    let nightly = &resources[&"backup.nightly".parse().unwrap()];
    assert_eq!(
        nightly.properties()[&PropertyPath::Target],
        json_value(serde_json::json!("postgres.main"))
    );
    assert_eq!(
        nightly.properties()[&PropertyPath::Destination],
        json_value(serde_json::json!({ "name": "offsite" })),
        "the destination is a stable name selector, never a physical identity"
    );
    for (path, expected) in [
        (PropertyPath::Schedule, serde_json::json!("0 3 * * *")),
        (PropertyPath::Prefix, serde_json::json!("nightly")),
        (PropertyPath::Database, serde_json::json!("app")),
        (PropertyPath::Enabled, serde_json::json!(false)),
        (PropertyPath::KeepLatest, serde_json::json!(7)),
        (PropertyPath::IncludeEncryptionKey, serde_json::json!(true)),
    ] {
        assert_eq!(nightly.properties()[&path], json_value(expected), "{path}");
    }
    assert_eq!(
        nightly.containment().unwrap().to_string(),
        "environment.production"
    );
    assert_eq!(
        nightly
            .dependencies()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["postgres.main"],
        "the target is an inferred dependency, not containment"
    );

    let cleared = &resources[&"backup.cleared".parse().unwrap()];
    assert_eq!(
        cleared.properties()[&PropertyPath::KeepLatest],
        OwnedValue::Null
    );
    let plain = &resources[&"backup.plain".parse().unwrap()];
    assert!(!plain.properties().contains_key(&PropertyPath::KeepLatest));
    assert_eq!(
        plain.properties()[&PropertyPath::Enabled],
        json_value(serde_json::json!(true)),
        "enabled defaults to true and is always owned"
    );
    assert_eq!(
        plain.properties()[&PropertyPath::IncludeEncryptionKey],
        json_value(serde_json::json!(false))
    );
}

#[test]
fn binds_the_destination_selector_and_the_collision_inputs_for_discovery() {
    let config = config(&format!(
        r#"      nightly:
        target: postgres.main
        destination: {{ name: {CANARY} }}
        schedule: "0 3 * * *"
        prefix: nightly
        database: app
"#
    ));
    let compiled = compile_desired(&config, digest()).unwrap();
    let address = "backup.nightly".parse().unwrap();

    let selectors = compiled
        .bindings()
        .external_selectors()
        .map(|(address, path, kind, selector)| {
            (
                address.to_string(),
                path.to_string(),
                kind,
                selector.name().map(str::to_owned),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selectors,
        vec![(
            "backup.nightly".to_owned(),
            "destination".to_owned(),
            SelectorKind::Destination,
            Some(CANARY.to_owned())
        )]
    );
    let (target, destination, prefix, database) = compiled
        .bindings()
        .backup(&address)
        .expect("Backup binding is retained for identity discovery");
    assert_eq!(target.to_string(), "postgres.main");
    assert_eq!(destination.name(), Some(CANARY));
    assert_eq!((prefix, database), ("nightly", "app"));
    assert!(
        compiled
            .bindings()
            .backup(&"mount.x".parse().unwrap())
            .is_none()
    );

    let rendered = format!("{compiled:?}");
    assert!(!rendered.contains(CANARY), "{rendered}");
}

#[test]
fn ignored_in_place_properties_are_recorded_for_the_planner() {
    let config = config(
        r#"      nightly:
        target: postgres.main
        destination: { name: offsite }
        schedule: "0 3 * * *"
        prefix: nightly
        database: app
        lifecycle: { ignore_changes: [schedule, enabled] }
"#,
    );

    let compiled = compile_desired(&config, digest()).unwrap();
    let resource = &compiled.desired_state().resources()[&"backup.nightly".parse().unwrap()];

    assert_eq!(
        resource.ignored_changes(),
        &[PropertyPath::Schedule, PropertyPath::Enabled]
    );
}
