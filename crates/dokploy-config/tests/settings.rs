//! Settings documents, the document discriminator, tags, and project tag lists.

use dokploy_config::{
    ConfigError, DokployConfig, ExternalSelector, Field, ValidationIssue, initialize_settings,
    peek_scope, render,
};
use dokploy_state::{ResourceAddress, StateScope};

const SETTINGS: &str = r##"
version: 1
settings:
  server:
    tags:
      prod:
        color: "#e11d48"
      staging:
        name: Staging Env
      plain: {}
"##;

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("test address is valid")
}

fn issues(source: &str) -> Vec<ValidationIssue> {
    match DokployConfig::parse(source) {
        Err(ConfigError::Invalid { issues, .. }) => issues,
        Ok(_) => panic!("the document must be rejected"),
        Err(error) => panic!("expected semantic issues, got {error}"),
    }
}

// ---------------------------------------------------------------------------
// Settings documents
// ---------------------------------------------------------------------------

#[test]
fn a_settings_document_declares_tags_without_containment() {
    let config = DokployConfig::parse(SETTINGS).expect("settings document parses");

    assert_eq!(config.scope(), StateScope::Settings);
    assert_eq!(config.resources().len(), 3);
    assert!(
        config.parents().is_empty(),
        "settings resources have no parent"
    );
    let prod = config
        .resource(&address("tag.prod"))
        .unwrap()
        .as_tag()
        .unwrap();
    assert_eq!(prod.color(), &Field::Set("#e11d48".to_owned()));
    assert_eq!(
        prod.name(),
        None,
        "the remote name defaults to the logical name"
    );
    let staging = config
        .resource(&address("tag.staging"))
        .unwrap()
        .as_tag()
        .unwrap();
    assert_eq!(staging.name(), Some("Staging Env"));
    assert_eq!(staging.color(), &Field::Unmanaged);
    assert!(config.resource(&address("tag.plain")).is_some());
}

#[test]
fn an_empty_settings_document_is_valid() {
    for source in [
        "version: 1\nsettings: {}\n",
        "version: 1\nsettings:\n  server: {}\n",
        "version: 1\nsettings:\n  server:\n    tags: {}\n",
    ] {
        let config = DokployConfig::parse(source).expect("an empty settings document parses");
        assert_eq!(config.scope(), StateScope::Settings);
        assert!(config.resources().is_empty());
    }
}

#[test]
fn project_documents_stay_project_scope() {
    let config = DokployConfig::parse("version: 1\nproject:\n  name: platform\n").unwrap();

    assert_eq!(config.scope(), StateScope::Project);
}

#[test]
fn a_document_cannot_declare_both_scopes() {
    let found =
        issues("version: 1\nproject:\n  name: platform\nsettings:\n  server:\n    tags: {}\n");

    assert_eq!(found, [ValidationIssue::AmbiguousDocumentScope]);
    assert_eq!(ValidationIssue::AmbiguousDocumentScope.code(), "DOKCFG070");
}

#[test]
fn a_document_with_neither_scope_reports_the_familiar_missing_project() {
    let error = DokployConfig::parse("version: 1\n").expect_err("a scope is required");

    assert!(matches!(
        error,
        ConfigError::Parse { .. } | ConfigError::ParseWithoutLocation
    ));
}

#[test]
fn unknown_settings_keys_fail_closed() {
    for source in [
        "version: 1\nsettings:\n  servers: {}\n",
        "version: 1\nsettings:\n  server:\n    ssh_keys: {}\n",
        "version: 1\nsettings:\n  server:\n    tags:\n      prod:\n        colour: red\n",
    ] {
        assert!(
            matches!(
                DokployConfig::parse(source),
                Err(ConfigError::Parse { .. } | ConfigError::ParseWithoutLocation)
            ),
            "{source}"
        );
    }
}

#[test]
fn tag_validation_rejects_bad_fields_collisions_and_null_color() {
    let cases: [(&str, ValidationIssue, &str); 5] = [
        (
            "an untrimmed remote name",
            ValidationIssue::InvalidTagField,
            "tag:\n      prod:\n        name: \" padded \"\n",
        ),
        (
            "an empty color",
            ValidationIssue::InvalidTagField,
            "tag:\n      prod:\n        color: \"\"\n",
        ),
        (
            "a null color",
            ValidationIssue::TagFieldCannotBeCleared,
            "tag:\n      prod:\n        color: null\n",
        ),
        (
            "two tags with one remote name",
            ValidationIssue::DuplicateTagCollision,
            "tag:\n      one:\n        name: Same\n      two:\n        name: Same\n",
        ),
        (
            "an explicit name equal to another logical name",
            ValidationIssue::DuplicateTagCollision,
            "tag:\n      one: {}\n      two:\n        name: one\n",
        ),
    ];
    for (name, expected, body) in cases {
        let source = format!(
            "version: 1\nsettings:\n  server:\n    {}",
            body.replacen("tag:", "tags:", 1)
        );
        let found = issues(&source);
        assert!(found.contains(&expected), "{name}: {found:?}");
    }
    assert_eq!(ValidationIssue::InvalidTagField.code(), "DOKCFG071");
    assert_eq!(ValidationIssue::DuplicateTagCollision.code(), "DOKCFG072");
    assert_eq!(ValidationIssue::TagFieldCannotBeCleared.code(), "DOKCFG073");
}

#[test]
fn tags_support_moves_and_removals_like_any_resource() {
    let config = DokployConfig::parse(
        r#"
version: 1
settings:
  server:
    tags:
      renamed: {}
moves:
  - from: tag.original
    to: tag.renamed
removed:
  - from: tag.gone
    destroy: true
"#,
    )
    .expect("move and removal declarations parse");

    assert_eq!(config.moves().len(), 1);
    assert_eq!(config.removed().len(), 1);
}

#[test]
fn a_settings_document_round_trips_through_the_canonical_writer() {
    let config = DokployConfig::parse(SETTINGS).unwrap();

    let rendered = render(&config).expect("settings render");
    let reparsed = DokployConfig::parse(&rendered).expect("rendered settings parse");

    assert_eq!(reparsed, config);
    assert_eq!(render(&reparsed).unwrap(), rendered, "rendering is stable");
    assert!(
        rendered.contains("settings:\n  server:\n    tags:\n"),
        "{rendered}"
    );

    let empty = DokployConfig::parse("version: 1\nsettings: {}\n").unwrap();
    assert_eq!(render(&empty).unwrap(), "version: 1\nsettings: {}\n");
}

// ---------------------------------------------------------------------------
// Project tags
// ---------------------------------------------------------------------------

fn project_with(tags: &str) -> String {
    format!("version: 1\nproject:\n  name: platform\n{tags}")
}

fn project_tags(source: &str) -> Field<Vec<ExternalSelector>> {
    DokployConfig::parse(source)
        .expect("project parses")
        .resource(&address("project.platform"))
        .unwrap()
        .as_project()
        .unwrap()
        .tags()
        .clone()
}

#[test]
fn project_tags_keep_omitted_empty_and_listed_apart() {
    assert_eq!(project_tags(&project_with("")), Field::Unmanaged);
    assert_eq!(
        project_tags(&project_with("  tags: []\n")),
        Field::Set(Vec::new())
    );
    assert_eq!(
        project_tags(&project_with(
            "  tags:\n    - name: prod\n    - name: staging\n"
        )),
        Field::Set(vec![
            ExternalSelector::named("prod"),
            ExternalSelector::named("staging"),
        ])
    );
}

#[test]
fn project_tag_validation_rejects_null_local_duplicate_and_invalid_names() {
    let cases: [(&str, &str, ValidationIssue); 4] = [
        (
            "null",
            "  tags: null\n",
            ValidationIssue::TagFieldCannotBeCleared,
        ),
        (
            "local",
            "  tags:\n    - local: true\n",
            ValidationIssue::LocalSelectorUnsupported,
        ),
        (
            "duplicate",
            "  tags:\n    - name: prod\n    - name: prod\n",
            ValidationIssue::DuplicateProjectTag,
        ),
        (
            "padded",
            "  tags:\n    - name: \" prod \"\n",
            ValidationIssue::InvalidExternalSelectorName,
        ),
    ];
    for (name, body, expected) in cases {
        let found = issues(&project_with(body));
        assert!(found.contains(&expected), "{name}: {found:?}");
    }
    assert_eq!(ValidationIssue::DuplicateProjectTag.code(), "DOKCFG074");
}

#[test]
fn project_tags_survive_the_canonical_writer() {
    for tags in [
        "",
        "  tags: []\n",
        "  tags:\n    - name: prod\n    - name: \"two words\"\n",
    ] {
        let config = DokployConfig::parse(&project_with(tags)).unwrap();

        let rendered = render(&config).expect("project renders");

        assert_eq!(DokployConfig::parse(&rendered).unwrap(), config, "{tags:?}");
    }
}

#[test]
fn a_project_tag_name_is_not_echoed_in_debug_output() {
    let config =
        DokployConfig::parse(&project_with("  tags:\n    - name: confidential-tag\n")).unwrap();

    assert!(!format!("{config:?}").contains("confidential-tag"));
    assert!(!format!("{:?}", config.resources()).contains("confidential-tag"));
}

// ---------------------------------------------------------------------------
// Scope detection, the starter, and schemas
// ---------------------------------------------------------------------------

#[test]
fn peek_scope_classifies_files_without_validating_them() {
    let directory = tempfile::tempdir().unwrap();
    let write = |name: &str, text: &str| {
        let path = directory.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    };

    assert_eq!(
        peek_scope(write("settings.yaml", SETTINGS)).unwrap(),
        StateScope::Settings
    );
    // A semantically invalid settings document is still a settings document.
    assert_eq!(
        peek_scope(write(
            "bad.yaml",
            "version: 1\nsettings:\n  server:\n    tags:\n      a:\n        color: null\n"
        ))
        .unwrap(),
        StateScope::Settings
    );
    assert_eq!(
        peek_scope(write(
            "project.yaml",
            "version: 1\nproject:\n  name: platform\n"
        ))
        .unwrap(),
        StateScope::Project
    );
    assert_eq!(
        peek_scope(write(
            "both.yaml",
            "version: 1\nproject:\n  name: a\nsettings: {}\n"
        ))
        .unwrap(),
        StateScope::Project,
        "an ambiguous document is left to the strict parser"
    );
    assert_eq!(
        peek_scope(write("garbage.yaml", ": : :\n\t")).unwrap(),
        StateScope::Project
    );
    assert_eq!(
        peek_scope(directory.path().join("missing.yaml")).unwrap(),
        StateScope::Project
    );
}

#[test]
fn the_settings_starter_is_a_valid_empty_settings_document() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dokploy.settings.yaml");

    initialize_settings(&path).expect("starter is written");
    let config = dokploy_config::load(&path).expect("starter is valid");

    assert_eq!(config.scope(), StateScope::Settings);
    assert!(config.resources().is_empty());
    assert!(
        initialize_settings(&path).is_err(),
        "an existing file is never replaced"
    );
}

#[test]
fn the_two_json_schemas_describe_their_own_documents() {
    let project = serde_json::to_string(&DokployConfig::json_schema()).unwrap();
    let settings = serde_json::to_string(&DokployConfig::settings_json_schema()).unwrap();

    assert!(project.contains("\"project\"") && !project.contains("\"settings\""));
    assert!(project.contains("\"tags\""), "project tags are documented");
    assert!(settings.contains("\"settings\"") && !settings.contains("\"environments\""));
    assert!(settings.contains("\"tags\""));
}
