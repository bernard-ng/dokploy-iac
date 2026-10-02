use dokploy_config::{
    ApplicationDocument, ConfigDocument, DokployConfig, EnvironmentDocument, Field,
    LifecycleDocument, MountDocument, MountSourceConfig, SecretSource, ValidationIssue, render,
};
use dokploy_state::{ResourceAddress, ResourceKind, ResourceName};

const MOUNT_BASE: &str = r#"
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
    staging:
      applications:
        preview: {}
"#;

/// Indents a YAML fragment two spaces so it nests under `project.environments`.
fn nest(fragment: &str) -> String {
    fragment.lines().map(|line| format!("  {line}\n")).collect()
}

fn with_mounts(mounts: &str) -> String {
    MOUNT_BASE.replace(
        "    staging:",
        &format!("      mounts:\n{}    staging:", nest(mounts)),
    )
}

fn one_mount(body: &str) -> String {
    with_mounts(&format!("      data:\n{body}"))
}

#[test]
fn parses_typed_mount_targets_and_sources_with_descriptor_only_content() {
    let config = DokployConfig::parse(&with_mounts(
        r#"      data:
        target: application.api
        mount_path: /data
        source:
          type: volume
          volume_name: api-data
      host:
        target: compose.stack
        mount_path: /host
        source:
          type: bind
          host_path: /srv/stack
      settings:
        target: postgres.main
        mount_path: /etc/settings.conf
        source:
          type: file
          file_path: conf.d/settings.conf
          content:
            env: MOUNT_CONTENT
"#,
    ))
    .expect("typed Mount configuration is valid");

    let data = config
        .resource(&"mount.data".parse::<ResourceAddress>().unwrap())
        .and_then(|resource| resource.as_mount())
        .expect("Mount exists");
    assert_eq!(data.target().to_string(), "application.api");
    assert_eq!(data.mount_path(), "/data");
    assert_eq!(data.source().type_name(), "volume");
    assert_eq!(
        config.parent_of(&"mount.data".parse().unwrap()),
        Some(&"environment.production".parse().unwrap()),
        "a Mount is contained by its environment, not by its target"
    );

    let settings = config
        .resource(&"mount.settings".parse::<ResourceAddress>().unwrap())
        .unwrap();
    let mount = settings.as_mount().unwrap();
    assert!(matches!(
        mount.source(),
        MountSourceConfig::File {
            content: Field::Set(SecretSource::Env(name)),
            ..
        } if name == "MOUNT_CONTENT"
    ));
    for debug in [
        format!("{settings:?}"),
        format!("{mount:?}"),
        format!("{:?}", mount.source()),
    ] {
        assert!(!debug.contains("MOUNT_CONTENT"));
        assert!(!debug.contains("settings.conf"));
    }
}

#[test]
fn rejects_mount_targets_outside_the_closed_same_environment_union() {
    for (target, expected) in [
        ("project.platform", ValidationIssue::InvalidMountTarget),
        (
            "environment.production",
            ValidationIssue::InvalidMountTarget,
        ),
        ("mount.data", ValidationIssue::InvalidMountTarget),
        ("application.missing", ValidationIssue::MissingReference),
        (
            "application.preview",
            ValidationIssue::CrossEnvironmentReference,
        ),
    ] {
        let error = DokployConfig::parse(&one_mount(&format!(
            "        target: {target}\n        mount_path: /data\n        source:\n          type: volume\n          volume_name: api-data\n"
        )))
        .expect_err("Mount target must be a same-environment supported service");
        assert!(error.issues().contains(&expected), "{target}: {error:?}");
    }
}

#[test]
fn rejects_duplicate_mount_path_per_target_but_allows_other_targets() {
    let error = DokployConfig::parse(&with_mounts(
        r#"      first:
        target: application.api
        mount_path: /data
        source: { type: volume, volume_name: one }
      second:
        target: application.api
        mount_path: /data
        source: { type: volume, volume_name: two }
"#,
    ))
    .expect_err("one target cannot mount two sources at one path");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::DuplicateMountCollision)
    );

    DokployConfig::parse(&with_mounts(
        r#"      first:
        target: application.api
        mount_path: /data
        source: { type: volume, volume_name: one }
      second:
        target: compose.stack
        mount_path: /data
        source: { type: volume, volume_name: one }
"#,
    ))
    .expect("mount path is scoped by target");
}

#[test]
fn rejects_unsafe_mount_paths_names_and_unknown_sources() {
    for source in [
        "mount_path: relative\n        source: { type: volume, volume_name: ok }",
        "mount_path: /\n        source: { type: volume, volume_name: ok }",
        "mount_path: \"/data\\n\"\n        source: { type: volume, volume_name: ok }",
        "mount_path: /data\n        source: { type: bind, host_path: relative }",
        "mount_path: /data\n        source: { type: volume, volume_name: \"-bad\" }",
        "mount_path: /data\n        source: { type: volume, volume_name: \"bad name\" }",
        "mount_path: /data\n        source: { type: file, file_path: /absolute, content: { env: MOUNT_CONTENT } }",
        "mount_path: /data\n        source: { type: file, file_path: ../escape, content: { env: MOUNT_CONTENT } }",
        "mount_path: /data\n        source: { type: file, file_path: a//b, content: { env: MOUNT_CONTENT } }",
    ] {
        let error = DokployConfig::parse(&one_mount(&format!(
            "        target: application.api\n        {source}\n"
        )))
        .expect_err("unsafe Mount value must be rejected");
        assert!(
            error.issues().contains(&ValidationIssue::InvalidMountField),
            "{source}: {error:?}"
        );
    }

    for source in [
        "source: { type: tmpfs, volume_name: ok }",
        "source: { type: volume }",
        "source: { type: volume, volume_name: ok, host_path: /x }",
        "source: { type: file, file_path: a, content: { value: literal-secret-canary } }",
        "source: { type: file, file_path: a, content: literal-secret-canary }",
    ] {
        let error = DokployConfig::parse(&one_mount(&format!(
            "        target: application.api\n        mount_path: /data\n        {source}\n"
        )))
        .expect_err("malformed Mount source must fail strict parsing");
        assert!(!format!("{error:?}").contains("literal-secret-canary"));
    }
}

#[test]
fn file_mount_content_is_descriptor_only_and_clear_or_unmanaged_is_restricted() {
    let cleared = DokployConfig::parse(&one_mount(
        "        target: application.api\n        mount_path: /data\n        source: { type: file, file_path: a, content: null }\n",
    ))
    .expect_err("file content cannot be cleared");
    assert!(
        cleared
            .issues()
            .contains(&ValidationIssue::MountContentCannotBeCleared)
    );

    let unprotected = DokployConfig::parse(&one_mount(
        "        target: application.api\n        mount_path: /data\n        source: { type: file, file_path: a }\n",
    ))
    .expect_err("unmanaged content requires protection");
    assert!(
        unprotected
            .issues()
            .contains(&ValidationIssue::UnmanagedMountContentRequiresProtection)
    );

    DokployConfig::parse(&one_mount(
        "        target: application.api\n        mount_path: /data\n        source: { type: file, file_path: a }\n        lifecycle: { protect: true }\n",
    ))
    .expect("protected unmanaged content is the import shape");

    let unsafe_file = DokployConfig::parse(&one_mount(
        "        target: application.api\n        mount_path: /data\n        source: { type: file, file_path: a, content: { file: ../secret } }\n",
    ))
    .expect_err("content descriptor files are workspace-relative");
    assert!(
        unsafe_file
            .issues()
            .contains(&ValidationIssue::UnsafeSecretFile)
    );
}

#[test]
fn mount_ignored_changes_are_limited_to_in_place_properties() {
    DokployConfig::parse(&one_mount(
        "        target: application.api\n        mount_path: /data\n        source: { type: volume, volume_name: ok }\n        lifecycle: { ignore_changes: [mount_path, volume_name] }\n",
    ))
    .expect("in-place Mount properties can be ignored");

    for path in ["target", "mount_type", "content"] {
        let error = DokployConfig::parse(&one_mount(&format!(
            "        target: application.api\n        mount_path: /data\n        source: {{ type: volume, volume_name: ok }}\n        lifecycle: {{ ignore_changes: [{path}] }}\n"
        )))
        .expect_err("replacement and sensitive paths cannot be ignored");
        assert!(
            error
                .issues()
                .contains(&ValidationIssue::InvalidIgnoredChange)
        );
    }
}

#[test]
fn mount_issue_codes_are_stable_and_message_safe() {
    for (issue, code) in [
        (ValidationIssue::InvalidMountTarget, "DOKCFG040"),
        (ValidationIssue::InvalidMountField, "DOKCFG041"),
        (ValidationIssue::DuplicateMountCollision, "DOKCFG042"),
        (ValidationIssue::MountContentCannotBeCleared, "DOKCFG043"),
        (
            ValidationIssue::UnmanagedMountContentRequiresProtection,
            "DOKCFG044",
        ),
    ] {
        assert_eq!(issue.code(), code);
        assert!(issue.to_string().starts_with(code));
    }
}

#[test]
fn generated_schema_models_the_typed_mount_union() {
    let schema = serde_json::to_value(DokployConfig::json_schema()).unwrap();

    assert_eq!(schema["$defs"]["RawMount"]["additionalProperties"], false);
    assert_eq!(
        schema["$defs"]["RawMount"]["required"],
        serde_json::json!(["target", "mount_path", "source"])
    );
    let variants = schema["$defs"]["RawMountSource"]["oneOf"]
        .as_array()
        .expect("Mount source is a tagged union");
    let names = variants
        .iter()
        .map(|variant| variant["properties"]["type"]["const"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            serde_json::json!("bind"),
            serde_json::json!("volume"),
            serde_json::json!("file")
        ]
    );
    assert_eq!(
        variants[2]["properties"]["content"],
        schema["$defs"]["RawCompose"]["properties"]["document"]
    );
    assert!(
        schema["$defs"]["RawMount"]["properties"]["target"]["pattern"]
            .as_str()
            .unwrap()
            .contains("application|compose|postgres|mysql|mariadb|mongo|libsql|redis")
    );
}

#[test]
fn canonical_writer_round_trips_every_mount_source_without_content_bytes() {
    let source = with_mounts(
        r#"      data:
        target: application.api
        mount_path: /data
        source:
          type: volume
          volume_name: api-data
        depends_on:
          - postgres.main
      host:
        target: compose.stack
        mount_path: /host
        source:
          type: bind
          host_path: /srv/stack
      settings:
        target: postgres.main
        mount_path: /etc/settings.conf
        source:
          type: file
          file_path: conf.d/settings.conf
          content:
            file: .secrets/settings
        lifecycle:
          protect: true
          ignore_changes: [mount_path]
      imported:
        target: application.api
        mount_path: /imported
        source:
          type: file
          file_path: imported.conf
        lifecycle:
          protect: true
"#,
    );
    let config = DokployConfig::parse(&source).expect("fixture is valid");

    let rendered = render(&config).expect("Mount configuration renders");

    assert_eq!(DokployConfig::parse(&rendered).unwrap(), config);
    assert!(
        rendered.contains("      mounts:\n        data:\n          target: \"application.api\"\n")
    );
    assert!(
        rendered.contains("            type: \"volume\"\n            volume_name: \"api-data\"\n")
    );
    assert!(rendered.contains("              file: \".secrets/settings\"\n"));
    assert_eq!(
        ConfigDocument::from_config(&config)
            .unwrap()
            .render()
            .unwrap(),
        rendered
    );
}

#[test]
fn typed_document_writes_imported_mount_with_unmanaged_content() {
    let name = |value: &str| value.parse::<ResourceName>().unwrap();
    let mut environment = EnvironmentDocument::default();
    environment
        .add_application(name("api"), ApplicationDocument::default())
        .unwrap();
    environment
        .add_mount(
            name("settings"),
            MountDocument {
                target: ResourceAddress::new(ResourceKind::Application, name("api")),
                mount_path: "/etc/settings.conf".to_owned(),
                source: MountSourceConfig::File {
                    file_path: "settings.conf".to_owned(),
                    content: Field::Unmanaged,
                },
                depends_on: Vec::new(),
                lifecycle: LifecycleDocument {
                    protect: Field::Set(true),
                    ..LifecycleDocument::default()
                },
            },
        )
        .unwrap();
    assert!(
        environment
            .add_mount(
                name("settings"),
                MountDocument {
                    target: ResourceAddress::new(ResourceKind::Application, name("api")),
                    mount_path: "/other".to_owned(),
                    source: MountSourceConfig::Volume {
                        volume_name: "v".to_owned(),
                    },
                    depends_on: Vec::new(),
                    lifecycle: LifecycleDocument::default(),
                },
            )
            .is_err(),
        "duplicate Mount names are rejected"
    );
    let mut document = ConfigDocument::new(name("platform"));
    document
        .add_environment(name("production"), environment)
        .unwrap();

    let rendered = document.render().expect("imported Mount renders");

    assert!(
        rendered.contains("            type: \"file\"\n            file_path: \"settings.conf\"\n")
    );
    assert!(!rendered.contains("content"));
    assert!(rendered.contains("          lifecycle:\n            protect: true\n"));
}
