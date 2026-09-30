use dokploy_config::{
    ConfigError, ConfigValue, DokployConfig, Field, SecretSourceKind, ValidationIssue,
};
use dokploy_state::ResourceAddress;

#[test]
fn parses_versioned_configuration_and_preserves_field_ownership() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
  description: null
  lifecycle:
    protect: false
environments:
  production:
    applications:
      api:
        description: ""
        replicas: 0
      worker: {}
"#,
    )
    .expect("valid configuration");

    assert_eq!(config.version(), 1);

    let project = config
        .resource(&"project.platform".parse::<ResourceAddress>().unwrap())
        .unwrap()
        .as_project()
        .unwrap();
    assert_eq!(project.description(), &Field::Clear);
    assert_eq!(project.lifecycle().protect(), &Field::Set(false));

    let api = config
        .resource(&"application.api".parse::<ResourceAddress>().unwrap())
        .unwrap()
        .as_application()
        .unwrap();
    assert_eq!(api.description(), &Field::Set(String::new()));
    assert_eq!(api.replicas(), &Field::Set(0));

    let worker = config
        .resource(&"application.worker".parse::<ResourceAddress>().unwrap())
        .unwrap()
        .as_application()
        .unwrap();
    assert_eq!(worker.description(), &Field::Unmanaged);
}

#[test]
fn parses_mysql_with_independent_secret_descriptors_and_strict_fields() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    mysql:
      primary:
        database: app
        username: app
        password:
          env: MYSQL_PASSWORD
        root_password:
          file: .secrets/mysql-root-password
        depends_on: [application.api]
        lifecycle:
          protect: true
    applications:
      api:
        environment:
          DATABASE_URL:
            from: mysql.primary.connection_url
"#,
    )
    .expect("valid MySQL configuration");

    let mysql = config
        .resource(&"mysql.primary".parse().unwrap())
        .expect("MySQL resource exists")
        .as_mysql()
        .expect("resource is MySQL");

    assert_eq!(mysql.database(), &Field::Set("app".to_owned()));
    assert_eq!(mysql.username(), &Field::Set("app".to_owned()));
    assert!(
        matches!(mysql.password(), Field::Set(source) if source.env_name() == Some("MYSQL_PASSWORD"))
    );
    assert!(
        matches!(mysql.root_password(), Field::Set(source) if source.file_path() == Some(".secrets/mysql-root-password"))
    );
    assert_eq!(
        config
            .parent_of(&"mysql.primary".parse().unwrap())
            .expect("MySQL has an environment parent")
            .to_string(),
        "environment.production"
    );

    let unknown = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    mysql:
      primary:
        root_password:
          env: MYSQL_ROOT_PASSWORD
        server_id: raw-server-id
"#,
    );
    assert!(matches!(
        unknown,
        Err(ConfigError::Parse { .. } | ConfigError::ParseWithoutLocation)
    ));
}

#[test]
fn rejects_unknown_fields_duplicate_keys_and_multiple_documents() {
    let cases = [
        r#"
version: 1
project:
  name: platform
  mystery: true
"#,
        r#"
version: 1
version: 1
project:
  name: platform
"#,
        r#"
version: 1
project:
  name: platform
---
version: 1
project:
  name: other
"#,
    ];

    for yaml in cases {
        assert!(matches!(
            DokployConfig::parse(yaml),
            Err(ConfigError::Parse { .. } | ConfigError::ParseWithoutLocation)
        ));
    }
}

#[test]
fn parser_errors_never_echo_untrusted_configuration_values() {
    let canary = "plaintext-secret-canary";
    let cases = [
        format!("version: 1\nproject:\n  name: platform\n  secret: {canary}\n"),
        format!(
            "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      api:\n        environment:\n          TOKEN: {canary}\n"
        ),
        format!("version: nope-{canary}\nproject:\n  name: platform\n"),
    ];

    for yaml in cases {
        let error = DokployConfig::parse(&yaml).expect_err("invalid configuration");
        assert!(!error.to_string().contains(canary));
        assert!(!format!("{error:?}").contains(canary));
    }
}

#[test]
fn rejects_aliases_merge_keys_tags_and_excessive_nesting() {
    let cases = vec![
        "version: &version 1\nproject:\n  name: platform\n".to_owned(),
        "version: 1\nproject:\n  <<: {name: platform}\n".to_owned(),
        "version: 1\nproject:\n  name: !custom platform\n".to_owned(),
        format!(
            "version: 1\nproject:\n  name: platform\nenvironments: {}\n",
            "[".repeat(40) + &"]".repeat(40)
        ),
    ];

    for yaml in cases {
        assert!(DokployConfig::parse(&yaml).is_err());
    }
}

#[test]
fn rejects_unsupported_versions() {
    let error = DokployConfig::parse("version: 2\nproject:\n  name: platform\n")
        .expect_err("unsupported version");

    assert!(matches!(
        error,
        ConfigError::UnsupportedVersion { found: 2 }
    ));
}

#[test]
fn parses_references_secrets_dependencies_lifecycle_moves_and_removals() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main:
        database: app
        password:
          env: DATABASE_PASSWORD
        lifecycle:
          protect: true
    redis:
      cache: {}
    applications:
      api:
        source:
          type: github
          repository: legalterlaw/platform
          branch: main
        environment:
          DATABASE_URL:
            from: postgres.main.connection_url
          SECRET_KEY:
            secret:
              file: .secrets/api-key
        depends_on:
          - redis.cache
          - postgres.main
        lifecycle:
          ignore_changes:
            - deployment.status
            - replicas
    domains:
      public:
        host: api.example.test
        application: application.api
moves:
  - from: application.backend
    to: application.api
removed:
  - from: redis.legacy
    destroy: false
"#,
    )
    .expect("valid configuration");

    let api = config
        .resource(&"application.api".parse().unwrap())
        .unwrap()
        .as_application()
        .unwrap();
    assert_eq!(
        api.depends_on()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["postgres.main", "redis.cache"]
    );
    assert_eq!(
        api.lifecycle()
            .ignore_changes()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["deployment.status", "replicas"]
    );

    let environment = api.environment().as_set().unwrap();
    let Field::Set(ConfigValue::Reference(reference)) = environment.get("DATABASE_URL").unwrap()
    else {
        panic!("expected typed reference");
    };
    assert_eq!(reference.address().to_string(), "postgres.main");
    assert_eq!(reference.property().to_string(), "connection_url");

    let Field::Set(ConfigValue::Secret(secret)) = environment.get("SECRET_KEY").unwrap() else {
        panic!("expected secret descriptor");
    };
    assert_eq!(secret.kind(), SecretSourceKind::File);
    assert_eq!(config.moves().len(), 1);
    assert_eq!(config.removed().len(), 1);
    assert!(!config.removed()[0].destroy());
}

#[test]
fn rejects_missing_targets_unsafe_secret_files_and_cross_environment_collisions() {
    let error = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        environment:
          DATABASE_URL:
            from: postgres.missing.connection_url
          SECRET_KEY:
            secret:
              file: ../outside
        depends_on:
          - redis.missing
  staging:
    applications:
      api: {}
"#,
    )
    .expect_err("semantic validation must fail");

    assert!(
        error
            .issues()
            .contains(&ValidationIssue::DuplicateResourceAddress {
                kind: dokploy_state::ResourceKind::Application,
            })
    );
    assert!(error.issues().contains(&ValidationIssue::MissingReference));
    assert!(error.issues().contains(&ValidationIssue::MissingDependency));
    assert!(error.issues().contains(&ValidationIssue::UnsafeSecretFile));
}

#[test]
fn rejects_duplicate_or_conflicting_moves_removals_and_lifecycle_entries() {
    let error = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        depends_on: [project.platform, project.platform]
        lifecycle:
          ignore_changes: [replicas, replicas]
moves:
  - from: application.old
    to: application.api
  - from: application.old
    to: application.api
removed:
  - from: application.old
  - from: application.old
"#,
    )
    .expect_err("duplicate declarations must fail");

    assert!(
        error
            .issues()
            .contains(&ValidationIssue::DuplicateDependency)
    );
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::DuplicateIgnoredChange)
    );
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::DuplicateMoveSource)
    );
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::DuplicateMoveTarget)
    );
    assert!(error.issues().contains(&ValidationIssue::DuplicateRemoval));
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::MoveRemovalConflict)
    );
}

#[test]
fn generated_schema_is_strict_and_models_nullable_owned_fields() {
    let schema = serde_json::to_value(DokployConfig::json_schema()).unwrap();
    let rendered = serde_json::to_string(&schema).unwrap();

    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["version"]["const"], 1);
    assert!(rendered.contains("description"));
    assert!(rendered.contains("null"));
    assert!(rendered.contains("ignore_changes"));
    assert!(rendered.contains("moves"));
    assert!(rendered.contains("removed"));
    assert!(rendered.contains("mysql"));
    assert!(rendered.contains("root_password"));
    assert_eq!(
        schema["$defs"]["ConfigValue"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        schema["$defs"]["SecretSource"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        schema["$defs"]["RawProject"]["properties"]["description"]["type"],
        serde_json::json!(["string", "null"])
    );
    assert_eq!(
        schema["$defs"]["RawProject"]["required"],
        serde_json::json!(["name"])
    );
}

#[test]
fn semantic_errors_and_config_debug_never_echo_source_canaries() {
    let canary = "plaintext-secret-canary";
    let error = DokployConfig::parse(&format!(
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      api:\n        environment:\n          SECRET:\n            secret:\n              file: ../{canary}\n"
    ))
    .expect_err("unsafe file descriptor");
    assert!(!error.to_string().contains(canary));
    assert!(!format!("{error:?}").contains(canary));

    let config = DokployConfig::parse(&format!(
        "version: 1\nproject:\n  name: platform\n  description: {canary}\n"
    ))
    .unwrap();
    assert!(!format!("{config:?}").contains(canary));
}

#[test]
fn environment_collection_and_entries_preserve_ownership_semantics() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      omitted: {}
      cleared:
        environment: null
      empty:
        environment: {}
      configured:
        environment:
          CLEARED: null
          EMPTY:
            value: ""
"#,
    )
    .unwrap();

    let application = |name: &str| {
        config
            .resource(&format!("application.{name}").parse().unwrap())
            .unwrap()
            .as_application()
            .unwrap()
    };
    assert!(matches!(
        application("omitted").environment(),
        Field::Unmanaged
    ));
    assert!(matches!(application("cleared").environment(), Field::Clear));
    assert!(matches!(application("empty").environment(), Field::Set(values) if values.is_empty()));

    let Field::Set(values) = application("configured").environment() else {
        panic!("environment map should be managed");
    };
    assert!(matches!(values["CLEARED"], Field::Clear));
    assert!(
        matches!(values["EMPTY"], Field::Set(ConfigValue::Literal(ref value)) if value.is_empty())
    );
}

#[test]
fn preserves_parent_relationships_and_rejects_cross_environment_references() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      primary: {}
    applications:
      api: {}
  staging:
    postgres:
      preview: {}
    applications:
      preview: {}
"#,
    )
    .unwrap();

    assert_eq!(
        config
            .parent_of(&"environment.production".parse().unwrap())
            .unwrap()
            .to_string(),
        "project.platform"
    );
    assert_eq!(
        config
            .parent_of(&"application.preview".parse().unwrap())
            .unwrap()
            .to_string(),
        "environment.staging"
    );

    let error = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      primary: {}
  staging:
    applications:
      preview:
        environment:
          DATABASE_URL:
            from: postgres.primary.connection_url
"#,
    )
    .expect_err("cross-environment reference");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::CrossEnvironmentReference)
    );
}

#[test]
fn rejects_inline_secrets_unsafe_cross_platform_paths_and_unsupported_outputs_without_leaks() {
    let canary = "secret-canary-value";
    let inline = DokployConfig::parse(&format!(
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      api:\n        environment:\n          TOKEN:\n            secret: {canary}\n"
    ))
    .expect_err("inline secret values are forbidden");
    assert!(!inline.to_string().contains(canary));
    assert!(!format!("{inline:?}").contains(canary));

    for path in [r"C:\\secrets\\token", r"server\\share\\token", "~/.token"] {
        let error = DokployConfig::parse(&format!(
            "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      api:\n        environment:\n          TOKEN:\n            secret:\n              file: '{path}'\n"
        ))
        .expect_err("unsafe path");
        assert!(error.issues().contains(&ValidationIssue::UnsafeSecretFile));
    }

    let unsupported = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main: {}
    applications:
      api:
        environment:
          VALUE:
            from: postgres.main.not_an_output
"#,
    )
    .expect_err("unsupported output");
    assert!(
        unsupported
            .issues()
            .contains(&ValidationIssue::UnsupportedReferenceProperty)
    );
}

#[test]
fn parsing_is_bounded_by_bytes_nodes_booleans_and_aliases() {
    let oversized = "#".repeat(1024 * 1024 + 1);
    assert!(matches!(
        DokployConfig::parse(&oversized),
        Err(ConfigError::InputTooLarge { .. })
    ));

    let mut node_heavy = String::from("version: 1\nproject:\n  name: platform\nenvironments:\n");
    for index in 0..11_000 {
        node_heavy.push_str(&format!("  env{index}: {{}}\n"));
    }
    assert!(matches!(
        DokployConfig::parse(&node_heavy),
        Err(ConfigError::Parse { .. } | ConfigError::ParseWithoutLocation)
    ));

    assert!(
        DokployConfig::parse(
            "version: 1\nproject:\n  name: platform\n  lifecycle:\n    protect: yes\n"
        )
        .is_err()
    );
}

#[test]
fn semantic_issue_order_is_independent_of_yaml_map_order() {
    let first = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      beta:
        depends_on: [redis.missing]
      alpha:
        environment:
          VALUE:
            from: postgres.missing.connection_url
"#,
    )
    .unwrap_err();
    let second = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      alpha:
        environment:
          VALUE:
            from: postgres.missing.connection_url
      beta:
        depends_on: [redis.missing]
"#,
    )
    .unwrap_err();

    let mut first_issues = first.issues().to_vec();
    let mut second_issues = second.issues().to_vec();
    first_issues.sort();
    second_issues.sort();
    assert_eq!(first_issues, second_issues);
}

#[test]
fn rejects_unknown_or_secret_ignore_change_paths() {
    let error = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main:
        lifecycle:
          ignore_changes:
            - password
            - made_up
"#,
    )
    .expect_err("secret and unknown paths cannot be ignored");

    assert!(
        error
            .issues()
            .contains(&ValidationIssue::InvalidIgnoredChange)
    );
}

#[test]
fn selector_nulls_cannot_bypass_exactly_one_rules() {
    let cases = [
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        environment:
          TOKEN:
            secret:
              env: null
              file: ./secrets/token
"#,
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main: {}
    applications:
      api:
        environment:
          DATABASE_URL:
            value: null
            from: postgres.main.connection_url
"#,
    ];

    for yaml in cases {
        assert!(matches!(
            DokployConfig::parse(yaml),
            Err(ConfigError::Parse { .. } | ConfigError::ParseWithoutLocation)
        ));
    }
}

#[test]
fn accepts_a_leading_current_directory_in_secret_file_descriptors() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        environment:
          TOKEN:
            secret:
              file: ./secrets/token
"#,
    )
    .expect("safe relative descriptor");

    let api = config
        .resource(&"application.api".parse().unwrap())
        .unwrap()
        .as_application()
        .unwrap();
    let Field::Set(values) = api.environment() else {
        panic!("managed environment");
    };
    let Field::Set(ConfigValue::Secret(secret)) = &values["TOKEN"] else {
        panic!("secret descriptor");
    };
    assert_eq!(secret.file_path(), Some("./secrets/token"));
}

#[test]
fn semantic_diagnostics_retain_safe_resource_locations() {
    let error = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        depends_on: [redis.missing]
      worker:
        environment:
          DATABASE_URL:
            from: postgres.missing.connection_url
"#,
    )
    .expect_err("two semantic issues");

    assert_eq!(error.diagnostics().len(), 2);
    assert!(
        error
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.location().line() > 0)
    );
    assert_ne!(
        error.diagnostics()[0].location(),
        error.diagnostics()[1].location()
    );
    assert!(
        error
            .diagnostics()
            .windows(2)
            .all(|pair| pair[0].location() <= pair[1].location())
    );
}

#[test]
fn normalized_config_equality_ignores_source_order_and_locations() {
    let first = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    redis:
      cache: {}
    applications:
      worker: {}
      api: {}
"#,
    )
    .unwrap();
    let second = DokployConfig::parse(
        r#"

version: 1

project: { name: platform }
environments:
  production:
    applications:
      api: {}
      worker: {}
    redis:
      cache: {}
"#,
    )
    .unwrap();

    assert_eq!(first, second);
    assert_ne!(
        first.location_of(&"project.platform".parse().unwrap()),
        second.location_of(&"project.platform".parse().unwrap())
    );
}
