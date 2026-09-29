use dokploy_cli::desired::{CompileDesiredError, compile_desired, compile_desired_for_instance};
use dokploy_config::DokployConfig;
use dokploy_core::{ComparableValue, ConfigDigest, OwnedValue, PropertyPath, ProtectionIntent};
use dokploy_state::InstanceIdentity;

fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).expect("valid test digest")
}

fn value(value: serde_json::Value) -> OwnedValue {
    OwnedValue::Value(ComparableValue::try_from_json(value).expect("non-null test value"))
}

#[test]
fn instance_bound_compilation_without_sensitive_inputs_preserves_the_source_digest() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
"#,
    )
    .expect("valid configuration");
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();

    let compiled = compile_desired_for_instance(&config, digest(), instance, workspace.path())
        .expect("configuration without sensitive inputs compiles without external access");

    assert_eq!(compiled.desired_state().digest().as_str(), "a".repeat(64));
}

#[test]
fn rejects_null_protection_instead_of_coercing_it() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
  lifecycle:
    protect: null
"#,
    )
    .expect("configuration syntax is valid");

    let error = compile_desired(&config, digest()).expect_err("null protection is ambiguous");

    assert!(matches!(
        error,
        CompileDesiredError::ProtectionCannotBeCleared
    ));
    assert_eq!(error.code(), "DOKCMP001");
    assert_eq!(
        error.to_string(),
        "DOKCMP001: lifecycle protection cannot be null"
    );
}

#[test]
fn compiles_project_owned_fields_through_one_public_seam() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
  description: null
  lifecycle:
    protect: true
"#,
    )
    .expect("valid configuration");

    let compiled = compile_desired(&config, digest()).expect("configuration compiles");
    let project = compiled
        .desired_state()
        .resources()
        .get(&"project.platform".parse().unwrap())
        .expect("project is desired");

    assert_eq!(
        project.properties().get(&PropertyPath::Description),
        Some(&OwnedValue::Null)
    );
    assert_eq!(project.protection(), ProtectionIntent::Set(true));
    assert_eq!(compiled.desired_state().digest().as_str(), "a".repeat(64));
}

#[test]
fn compiles_all_mvp_resource_shapes_and_field_states() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    description: Production
    applications:
      api:
        description: API
        replicas: 2
        source:
          type: github
          repository: legalterlaw/platform
          branch: null
        environment:
          OMITTED: null
      cleared:
        source: null
        environment: null
      empty:
        environment: {}
    postgres:
      main:
        database: app
        username: null
        password: null
      unmanaged: {}
    redis:
      cache:
        password: null
      unmanaged: {}
    domains:
      public:
        host: api.example.test
        application: application.api
"#,
    )
    .expect("valid configuration");

    let compiled = compile_desired(&config, digest()).expect("configuration compiles");
    let resources = compiled.desired_state().resources();

    assert_eq!(resources.len(), 10);
    assert_eq!(
        resources[&"environment.production".parse().unwrap()]
            .properties()
            .get(&PropertyPath::Description),
        Some(&value(serde_json::json!("Production")))
    );

    let api = &resources[&"application.api".parse().unwrap()];
    assert_eq!(
        api.properties().get(&PropertyPath::Description),
        Some(&value(serde_json::json!("API")))
    );
    assert_eq!(
        api.properties().get(&PropertyPath::Replicas),
        Some(&value(serde_json::json!(2)))
    );
    assert_eq!(
        api.properties().get(&PropertyPath::SourceRepository),
        Some(&value(serde_json::json!("legalterlaw/platform")))
    );
    assert_eq!(
        api.properties().get(&PropertyPath::SourceBranch),
        Some(&OwnedValue::Null)
    );
    assert_eq!(
        api.properties()
            .get(&PropertyPath::environment_variable("OMITTED").unwrap()),
        Some(&OwnedValue::Null)
    );

    let cleared = &resources[&"application.cleared".parse().unwrap()];
    assert_eq!(
        cleared.properties().get(&PropertyPath::Source),
        Some(&OwnedValue::Null)
    );
    assert_eq!(
        cleared.properties().get(&PropertyPath::Environment),
        Some(&OwnedValue::Null)
    );
    assert_eq!(
        resources[&"application.empty".parse().unwrap()]
            .properties()
            .get(&PropertyPath::Environment),
        Some(&OwnedValue::EmptyCollection)
    );

    let postgres = &resources[&"postgres.main".parse().unwrap()];
    assert_eq!(
        postgres.properties().get(&PropertyPath::Database),
        Some(&value(serde_json::json!("app")))
    );
    assert_eq!(
        postgres.properties().get(&PropertyPath::Username),
        Some(&OwnedValue::Null)
    );
    assert_eq!(
        postgres.properties().get(&PropertyPath::Password),
        Some(&OwnedValue::Null)
    );
    assert!(
        !resources[&"postgres.unmanaged".parse().unwrap()]
            .properties()
            .contains_key(&PropertyPath::Password)
    );
    assert_eq!(
        resources[&"redis.cache".parse().unwrap()]
            .properties()
            .get(&PropertyPath::Password),
        Some(&OwnedValue::Null)
    );
    assert!(
        !resources[&"redis.unmanaged".parse().unwrap()]
            .properties()
            .contains_key(&PropertyPath::Password)
    );

    let domain = &resources[&"domain.public".parse().unwrap()];
    assert_eq!(
        domain.properties().get(&PropertyPath::Host),
        Some(&value(serde_json::json!("api.example.test")))
    );
    assert_eq!(
        domain.properties().get(&PropertyPath::Application),
        Some(&value(serde_json::json!("application.api")))
    );
}

#[test]
fn derives_canonical_dependencies_and_preserves_containment_parents() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    depends_on: [project.platform]
    postgres:
      main: {}
    redis:
      cache: {}
    applications:
      api:
        depends_on:
          - environment.production
          - postgres.main
          - redis.cache
    domains:
      public:
        application: application.api
        depends_on:
          - environment.production
          - application.api
"#,
    )
    .expect("valid configuration");

    let compiled = compile_desired(&config, digest()).expect("configuration compiles");
    let resources = compiled.desired_state().resources();
    let addresses = |values: &[&str]| {
        values
            .iter()
            .map(|value| value.parse().unwrap())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        resources[&"environment.production".parse().unwrap()].dependencies(),
        addresses(&["project.platform"])
    );
    assert_eq!(
        resources[&"application.api".parse().unwrap()].dependencies(),
        addresses(&["environment.production", "postgres.main", "redis.cache"])
    );
    assert_eq!(
        resources[&"domain.public".parse().unwrap()].dependencies(),
        addresses(&["application.api", "environment.production"])
    );
    assert_eq!(
        compiled
            .bindings()
            .parent_of(&"environment.production".parse().unwrap())
            .map(ToString::to_string),
        Some("project.platform".to_owned())
    );
    assert_eq!(
        compiled
            .bindings()
            .parent_of(&"application.api".parse().unwrap())
            .map(ToString::to_string),
        Some("environment.production".to_owned())
    );
}

#[test]
fn preserves_logical_bindings_without_leaking_non_sensitive_values_in_debug_output() {
    let host_canary = "host-canary.example.test";
    let config = DokployConfig::parse(&format!(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api: {{}}
    domains:
      public:
        host: {host_canary}
        application: application.api
"#
    ))
    .expect("valid configuration");

    let compiled = compile_desired(&config, digest()).expect("configuration compiles");
    let bindings = compiled.bindings();
    assert_eq!(
        bindings
            .domain_application(&"domain.public".parse().unwrap())
            .map(ToString::to_string),
        Some("application.api".to_owned())
    );

    let debug = format!("{compiled:?} {bindings:?}");
    assert!(!debug.contains(host_canary));
}

#[test]
fn compiles_lifecycle_paths_moves_and_removals() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        lifecycle:
          ignore_changes:
            - deployment.status
            - replicas
moves:
  - from: application.backend
    to: application.api
removed:
  - from: redis.legacy
    destroy: true
"#,
    )
    .expect("valid configuration");

    let compiled = compile_desired(&config, digest()).expect("configuration compiles");
    let api = &compiled.desired_state().resources()[&"application.api".parse().unwrap()];

    assert_eq!(
        api.ignored_changes(),
        &[PropertyPath::Replicas, PropertyPath::DeploymentStatus]
    );
    assert_eq!(compiled.desired_state().moves().len(), 1);
    assert_eq!(
        compiled.desired_state().moves()[0].from().to_string(),
        "application.backend"
    );
    assert_eq!(
        compiled.desired_state().moves()[0].to().to_string(),
        "application.api"
    );
    assert_eq!(compiled.desired_state().removals().len(), 1);
    assert_eq!(
        compiled.desired_state().removals()[0].address().to_string(),
        "redis.legacy"
    );
    assert!(compiled.desired_state().removals()[0].destroy());
}

#[test]
fn rejects_every_concrete_sensitive_intent_before_planning() {
    let canary = "SENSITIVE_INTENT_CANARY";
    let cases = [
        format!(
            r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        environment:
          VALUE:
            value: {canary}
"#
        ),
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
            from: postgres.main.connection_url
    postgres:
      main: {}
"#
        .to_owned(),
        format!(
            r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        environment:
          SECRET:
            secret:
              env: {canary}
"#
        ),
        format!(
            r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main:
        password:
          env: {canary}
"#
        ),
        format!(
            r#"
version: 1
project:
  name: platform
environments:
  production:
    redis:
      cache:
        password:
          env: {canary}
"#
        ),
    ];

    for yaml in cases {
        let config = DokployConfig::parse(&yaml).expect("valid configuration");
        let error = compile_desired(&config, digest())
            .expect_err("sensitive intent needs a convergence fingerprint");

        assert!(matches!(
            error,
            CompileDesiredError::SensitiveIntentUnsupported
        ));
        assert_eq!(error.code(), "DOKCMP004");
        assert_eq!(
            error.to_string(),
            "DOKCMP004: sensitive desired values are unsupported"
        );
        assert!(!format!("{error:?} {error}").contains(canary));
    }
}
