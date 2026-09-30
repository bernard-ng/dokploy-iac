use std::fs;

use dokploy_config::{
    ApplicationDocument, ConfigDocument, ConfigDocumentError, ConfigWriteError, DokployConfig,
    DomainDocument, EnvironmentDocument, Field, PostgresDocument, RedisDocument, SourceDocument,
    render, write,
};
use dokploy_state::{ResourceKind, ResourceName};

const COMPLETE_CONFIG: &str = r#"
version: 1
project:
  name: platform
  description: "LegalterLaw platform"
  lifecycle:
    protect: true
environments:
  production:
    description: Production
    postgres:
      main:
        database: app
        username: app
        password:
          env: DATABASE_PASSWORD
    redis:
      cache:
        password:
          file: .secrets/redis-password
    applications:
      api:
        description: "API: production"
        replicas: 2
        source:
          type: github
          repository: legalterlaw/platform
          branch: main
        environment:
          DATABASE_URL:
            from: postgres.main.connection_url
          FEATURE_FLAG:
            value: "on"
          OPTIONAL: null
          TOKEN:
            secret:
              file: .secrets/api-token
        depends_on: [redis.cache, postgres.main]
        lifecycle:
          ignore_changes: [replicas, deployment.status]
    domains:
      public:
        host: api.example.test
        application: application.api
moves:
  - from: application.backend
    to: application.api
removed:
  - from: redis.legacy
    destroy: true
"#;

#[test]
fn renders_the_complete_mvp_model_as_deterministic_nested_yaml() {
    let config = DokployConfig::parse(COMPLETE_CONFIG).expect("fixture is valid");

    let first = render(&config).expect("configuration renders");
    let second = render(&config).expect("configuration renders deterministically");
    let reparsed = DokployConfig::parse(&first).expect("rendered YAML passes the strict parser");

    assert_eq!(first, second);
    assert_eq!(reparsed, config);
    assert!(first.starts_with("version: 1\nproject:\n"));
    assert!(first.contains("\nenvironments:\n  production:\n"));
    assert!(first.contains("\n    applications:\n      api:\n"));
    assert!(!first.contains("resources:"));
}

#[test]
fn rendering_secret_descriptors_never_reads_plaintext_secret_files() {
    let directory = tempfile::tempdir().expect("temporary directory is available");
    let secret = "plaintext-secret-canary";
    fs::create_dir(directory.path().join(".secrets"))
        .expect("secret fixture directory is writable");
    fs::write(directory.path().join(".secrets/api-token"), secret)
        .expect("secret fixture is writable");
    let config = DokployConfig::parse(COMPLETE_CONFIG).expect("fixture is valid");

    let rendered = render(&config).expect("configuration renders");

    assert!(!rendered.contains(secret));
    assert!(rendered.contains("file: \".secrets/api-token\""));
}

#[test]
fn renders_an_empty_environment_as_an_empty_mapping() {
    let config = DokployConfig::parse(
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production: {}\n",
    )
    .expect("fixture is valid");

    let rendered = render(&config).expect("empty environment renders");

    assert!(rendered.contains("environments:\n  production: {}\n"));
    assert_eq!(
        DokployConfig::parse(&rendered).expect("rendered YAML passes the strict parser"),
        config
    );
}

#[test]
fn preserves_clear_empty_and_unmanaged_field_ownership() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
  description: null
environments:
  production:
    description: null
    lifecycle:
      protect: null
    applications:
      unmanaged: {}
      cleared:
        description: null
        replicas: null
        source: null
        environment: null
      empty:
        environment: {}
    postgres:
      main:
        database: null
        username: ""
        password: null
    redis:
      cache:
        password: null
    domains:
      public:
        host: null
        application: null
"#,
    )
    .expect("fixture is valid");

    let rendered = render(&config).expect("ownership-aware fields render");

    assert!(rendered.contains("      unmanaged: {}\n"));
    assert!(rendered.contains("        environment: {}\n"));
    assert!(rendered.contains("        source: null\n"));
    assert_eq!(
        DokployConfig::parse(&rendered).expect("rendered YAML passes the strict parser"),
        config
    );
}

#[test]
fn quotes_strings_that_yaml_could_otherwise_reinterpret() {
    let config = DokployConfig::parse(
        "version: 1\nproject:\n  name: platform\n  description: \"null\\n\\\"quoted\\\"\\\\path\"\n",
    )
    .expect("fixture is valid");

    let rendered = render(&config).expect("special string renders");

    assert!(rendered.contains("description: \"null\\n\\\"quoted\\\"\\\\path\""));
    assert_eq!(
        DokployConfig::parse(&rendered).expect("rendered YAML passes the strict parser"),
        config
    );
}

#[test]
fn writes_atomically_without_replacing_an_existing_configuration() {
    let directory = tempfile::tempdir().expect("temporary directory is available");
    let path = directory.path().join("dokploy.yaml");
    let config = DokployConfig::parse(COMPLETE_CONFIG).expect("fixture is valid");

    write(&path, &config).expect("new configuration is written");
    let first = fs::read_to_string(&path).expect("configuration is readable");

    let error = write(&path, &config).expect_err("existing configuration is not replaced");

    assert_eq!(
        fs::read_to_string(path).expect("configuration remains readable"),
        first
    );
    assert_eq!(error.to_string(), "the configuration file already exists");
}

#[test]
fn typed_import_document_builds_all_mvp_resources_with_secrets_unmanaged_by_default() {
    let mut document = ConfigDocument::new(name("platform"));
    document.project_mut().description = Field::Set("Platform".to_owned());
    document.project_mut().lifecycle.protect = Field::Set(true);

    let mut production = EnvironmentDocument::default();
    production.description = Field::Set("Production".to_owned());
    production
        .add_application(
            name("api"),
            ApplicationDocument {
                description: Field::Set("API".to_owned()),
                replicas: Field::Set(2),
                source: Field::Set(SourceDocument::GitHub {
                    repository: "legalterlaw/platform".to_owned(),
                    branch: Field::Set("main".to_owned()),
                }),
                depends_on: vec!["postgres.main".parse().unwrap()],
                ..ApplicationDocument::default()
            },
        )
        .expect("application is unique");
    production
        .add_postgres(
            name("main"),
            PostgresDocument {
                database: Field::Set("app".to_owned()),
                username: Field::Set("app".to_owned()),
                ..PostgresDocument::default()
            },
        )
        .expect("postgres is unique");
    production
        .add_redis(name("cache"), RedisDocument::default())
        .expect("redis is unique");
    production
        .add_domain(
            name("public"),
            DomainDocument {
                host: Field::Set("api.example.test".to_owned()),
                application: Field::Set("application.api".parse().unwrap()),
                ..DomainDocument::default()
            },
        )
        .expect("domain is unique");
    document
        .add_environment(name("production"), production)
        .expect("environment is unique");
    document.add_move(
        "application.backend".parse().unwrap(),
        "application.api".parse().unwrap(),
    );
    document.add_removed("redis.legacy".parse().unwrap(), false);

    let rendered = document.render().expect("typed document is valid");
    let config = document
        .to_config()
        .expect("typed document builds a config");

    assert_eq!(DokployConfig::parse(&rendered).unwrap(), config);
    assert!(rendered.contains("    applications:\n      api:\n"));
    assert!(rendered.contains("    postgres:\n      main:\n"));
    assert!(rendered.contains("    redis:\n      cache: {}\n"));
    assert!(rendered.contains("    domains:\n      public:\n"));
    assert!(!rendered.contains("password:"));
    assert!(!rendered.contains("environment:"));
}

#[test]
fn typed_import_document_rejects_duplicates_without_replacing_the_first_resource() {
    let mut document = ConfigDocument::new(name("platform"));
    let mut first = EnvironmentDocument::default();
    first.description = Field::Set("first".to_owned());
    let mut replacement = EnvironmentDocument::default();
    replacement.description = Field::Set("replacement-canary".to_owned());
    document
        .add_environment(name("production"), first)
        .expect("first environment is accepted");

    let error = document
        .add_environment(name("production"), replacement)
        .expect_err("duplicate environment is rejected");
    let rendered = document.render().expect("first environment remains valid");

    assert_eq!(
        error,
        ConfigDocumentError::DuplicateResource {
            kind: ResourceKind::Environment
        }
    );
    assert!(rendered.contains("description: \"first\""));
    assert!(!rendered.contains("replacement-canary"));
}

#[test]
fn typed_import_document_uses_strict_semantic_validation() {
    let mut document = ConfigDocument::new(name("platform"));
    let mut production = EnvironmentDocument::default();
    production
        .add_domain(
            name("public"),
            DomainDocument {
                application: Field::Set("application.missing".parse().unwrap()),
                ..DomainDocument::default()
            },
        )
        .expect("domain is unique");
    document
        .add_environment(name("production"), production)
        .expect("environment is unique");

    let error = document
        .to_config()
        .expect_err("missing reference is rejected by the strict parser");

    assert!(matches!(error, ConfigWriteError::GeneratedConfig(_)));
}

fn name(value: &str) -> ResourceName {
    value.parse().expect("fixture resource name is valid")
}
