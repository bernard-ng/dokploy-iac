use dokploy_config::{
    ApplicationDocument, ConfigDocument, DokployConfig, EnvironmentDocument, Field,
    LifecycleDocument, ScheduleDocument, ScheduleShellConfig, SecretSource, ValidationIssue,
    render,
};
use dokploy_state::{ResourceAddress, ResourceKind, ResourceName};

const SCHEDULE_BASE: &str = r#"
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

fn with_schedules(schedules: &str) -> String {
    SCHEDULE_BASE.replace(
        "  staging:",
        &format!("    schedules:\n{schedules}  staging:"),
    )
}

fn one_schedule(body: &str) -> String {
    with_schedules(&format!("      nightly:\n{body}"))
}

const APPLICATION_JOB: &str = "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: bash\n        enabled: false\n        command:\n          env: SCHEDULE_COMMAND\n";

fn application_job_with(extra: &str) -> String {
    one_schedule(&format!("{APPLICATION_JOB}{extra}"))
}

#[test]
fn parses_application_and_compose_schedules_with_descriptor_only_executables() {
    let config = DokployConfig::parse(&with_schedules(
        r#"      nightly:
        name: nightly
        target: application.api
        cron_expression: "0 3 * * *"
        shell_type: bash
        enabled: false
        description: Nightly cleanup
        timezone: Africa/Lubumbashi
        command:
          env: SCHEDULE_COMMAND
        script:
          file: .secrets/schedule-script
      worker:
        name: worker-job
        target: compose.stack
        service_name: worker
        cron_expression: "@daily"
        shell_type: sh
        enabled: true
        command:
          env: WORKER_COMMAND
"#,
    ))
    .expect("typed Schedule configuration is valid");

    let nightly = config
        .resource(&"schedule.nightly".parse::<ResourceAddress>().unwrap())
        .and_then(|resource| resource.as_schedule())
        .expect("Schedule exists");
    assert_eq!(nightly.name(), "nightly");
    assert_eq!(nightly.target().to_string(), "application.api");
    assert_eq!(nightly.service_name(), None);
    assert_eq!(nightly.cron_expression(), "0 3 * * *");
    assert_eq!(nightly.shell_type(), ScheduleShellConfig::Bash);
    assert!(!nightly.enabled());
    assert!(matches!(
        nightly.command(),
        Field::Set(SecretSource::Env(name)) if name == "SCHEDULE_COMMAND"
    ));
    assert!(matches!(
        nightly.script(),
        Field::Set(SecretSource::File(path)) if path == ".secrets/schedule-script"
    ));
    assert_eq!(
        config.parent_of(&"schedule.nightly".parse().unwrap()),
        Some(&"environment.production".parse().unwrap()),
        "a Schedule is contained by its environment, not by its target"
    );

    let worker = config
        .resource(&"schedule.worker".parse::<ResourceAddress>().unwrap())
        .and_then(|resource| resource.as_schedule())
        .unwrap();
    assert_eq!(worker.service_name(), Some("worker"));
    assert_eq!(worker.shell_type(), ScheduleShellConfig::Sh);
    assert!(worker.enabled());
    assert!(matches!(worker.script(), Field::Unmanaged));

    let resource = config
        .resource(&"schedule.nightly".parse::<ResourceAddress>().unwrap())
        .unwrap();
    for debug in [format!("{resource:?}"), format!("{nightly:?}")] {
        for sensitive in ["SCHEDULE_COMMAND", "schedule-script", "nightly", "cleanup"] {
            assert!(!debug.contains(sensitive), "{sensitive} leaked: {debug}");
        }
    }
}

#[test]
fn rejects_schedule_targets_outside_the_closed_same_environment_union() {
    for (target, service, expected) in [
        (
            "project.platform",
            None,
            ValidationIssue::InvalidScheduleTarget,
        ),
        (
            "environment.production",
            None,
            ValidationIssue::InvalidScheduleTarget,
        ),
        (
            "postgres.main",
            None,
            ValidationIssue::InvalidScheduleTarget,
        ),
        (
            "schedule.nightly",
            None,
            ValidationIssue::InvalidScheduleTarget,
        ),
        (
            "compose.stack",
            None,
            ValidationIssue::InvalidScheduleTarget,
        ),
        (
            "application.api",
            Some("worker"),
            ValidationIssue::InvalidScheduleTarget,
        ),
        (
            "application.missing",
            None,
            ValidationIssue::MissingReference,
        ),
        (
            "application.preview",
            None,
            ValidationIssue::CrossEnvironmentReference,
        ),
    ] {
        let service = service.map_or_else(String::new, |service| {
            format!("        service_name: {service}\n")
        });
        let error = DokployConfig::parse(&one_schedule(&format!(
            "        name: nightly\n        target: {target}\n{service}        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: false\n        command:\n          env: SCHEDULE_COMMAND\n"
        )))
        .expect_err("Schedule target must be a same-environment application or Compose service");
        assert!(error.issues().contains(&expected), "{target}: {error:?}");
    }
}

#[test]
fn rejects_duplicate_names_per_target_but_allows_other_targets() {
    let error = DokployConfig::parse(&with_schedules(
        r#"      first:
        name: job
        target: application.api
        cron_expression: "0 3 * * *"
        shell_type: sh
        enabled: false
        command: { env: ONE }
      second:
        name: job
        target: application.api
        cron_expression: "0 4 * * *"
        shell_type: sh
        enabled: false
        command: { env: TWO }
"#,
    ))
    .expect_err("one target cannot hold two Schedules with one name");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::DuplicateScheduleCollision)
    );

    DokployConfig::parse(&with_schedules(
        r#"      first:
        name: job
        target: application.api
        cron_expression: "0 3 * * *"
        shell_type: sh
        enabled: false
        command: { env: ONE }
      second:
        name: job
        target: compose.stack
        service_name: worker
        cron_expression: "0 4 * * *"
        shell_type: sh
        enabled: false
        command: { env: TWO }
"#,
    ))
    .expect("Schedule names are scoped by target");

    DokployConfig::parse(&with_schedules(
        r#"      first:
        name: job
        target: application.api
        cron_expression: "0 3 * * *"
        shell_type: sh
        enabled: false
        command: { env: ONE }
      second:
        name: other
        target: application.api
        cron_expression: "0 4 * * *"
        shell_type: sh
        enabled: false
        command: { env: TWO }
"#,
    ))
    .expect("distinct names on one target are valid");
}

#[test]
fn schedules_on_one_compose_must_share_one_service_name() {
    let error = DokployConfig::parse(&with_schedules(
        r#"      first:
        name: job
        target: compose.stack
        service_name: worker
        cron_expression: "0 3 * * *"
        shell_type: sh
        enabled: false
        command: { env: ONE }
      second:
        name: job
        target: compose.stack
        service_name: web
        cron_expression: "0 4 * * *"
        shell_type: sh
        enabled: false
        command: { env: TWO }
"#,
    ))
    .expect_err("the SDK reads one Compose service collection per target");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::ScheduleComposeServiceMismatch)
    );
}

#[test]
fn rejects_malformed_names_cron_service_timezone_and_description() {
    for fields in [
        "name: \"\"",
        "name: \" lead\"",
        "name: \"trail \"",
        "name: \"bad/name\"",
        "name: \"bad\\nname\"",
        "cron_expression: \"\"",
        "cron_expression: \"* * * *\"",
        "cron_expression: \"* * * * * * *\"",
        "cron_expression: \"*  * * * *\"",
        "cron_expression: \"* * * * $(id)\"",
        "cron_expression: \"@sometimes\"",
        "timezone: \"Bad Zone\"",
        "timezone: \"\"",
        "description: \"\"",
        "description: \"line\\nbreak\"",
        "description: \" padded\"",
    ] {
        let key = fields.split(':').next().unwrap();
        let base = String::from(
            "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: bash\n        enabled: false\n        command:\n          env: SCHEDULE_COMMAND\n",
        );
        let line_prefix = format!("        {key}:");
        let body: String = base
            .lines()
            .filter(|line| !line.starts_with(&line_prefix))
            .map(|line| format!("{line}\n"))
            .collect::<String>()
            + &format!("        {fields}\n");
        let error = DokployConfig::parse(&one_schedule(&body))
            .expect_err("malformed Schedule value must be rejected");
        assert!(
            error
                .issues()
                .contains(&ValidationIssue::InvalidScheduleField),
            "{fields}: {error:?}"
        );
    }

    let error = DokployConfig::parse(&one_schedule(&format!(
        "{APPLICATION_JOB}        service_name: \"bad service\"\n"
    )))
    .expect_err("service names are validated");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::InvalidScheduleTarget)
            || error
                .issues()
                .contains(&ValidationIssue::InvalidScheduleField)
    );

    let error = DokployConfig::parse(&with_schedules(
        "      nightly:\n        name: nightly\n        target: compose.stack\n        service_name: \"bad service\"\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: false\n        command: { env: X }\n",
    ))
    .expect_err("Compose service names are validated");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::InvalidScheduleField)
    );
}

#[test]
fn accepts_conservative_cron_shapes_including_macros_and_seconds() {
    for cron in [
        "0 3 * * *",
        "*/5 0-6 1,15 * mon-fri",
        "0 0 3 * * *",
        "@hourly",
        "@midnight",
    ] {
        DokployConfig::parse(&one_schedule(&format!(
            "        name: nightly\n        target: application.api\n        cron_expression: \"{cron}\"\n        shell_type: sh\n        enabled: false\n        command: {{ env: X }}\n"
        )))
        .unwrap_or_else(|error| panic!("{cron} should be accepted: {error:?}"));
    }
}

#[test]
fn shell_type_and_enabled_are_required_and_strictly_typed() {
    let missing_enabled = DokployConfig::parse(&one_schedule(
        "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        command: { env: X }\n",
    ))
    .expect_err("enabled must be explicit so a Schedule never runs by default");
    assert!(!format!("{missing_enabled:?}").is_empty());

    for body in [
        "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        enabled: false\n        command: { env: X }\n",
        "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: zsh\n        enabled: false\n        command: { env: X }\n",
        "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: \"yes\"\n        command: { env: X }\n",
        "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: null\n        command: { env: X }\n",
        "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: false\n        server_id: x\n        command: { env: X }\n",
    ] {
        DokployConfig::parse(&one_schedule(body))
            .expect_err("strict parsing rejects missing, unknown, and mistyped fields");
    }
}

#[test]
fn command_and_script_are_descriptor_only_and_clear_or_unmanaged_is_restricted() {
    for field in ["command", "script"] {
        let source = if field == "command" {
            "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: false\n        command: null\n"
                .to_owned()
        } else {
            format!("{APPLICATION_JOB}        script: null\n")
        };
        let error = DokployConfig::parse(&one_schedule(&source))
            .expect_err("executable text cannot be cleared");
        assert!(
            error
                .issues()
                .contains(&ValidationIssue::ScheduleSecretCannotBeCleared),
            "{field}: {error:?}"
        );
    }

    let unprotected = DokployConfig::parse(&one_schedule(
        "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: false\n",
    ))
    .expect_err("an unmanaged command requires protection");
    assert!(
        unprotected
            .issues()
            .contains(&ValidationIssue::UnmanagedScheduleCommandRequiresProtection)
    );

    DokployConfig::parse(&one_schedule(
        "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: false\n        lifecycle: { protect: true }\n",
    ))
    .expect("a protected unmanaged command is the import shape");

    // An omitted script is accepted without protection: it means the Schedule has
    // no script, and the executor refuses any write that would need to resend one.
    DokployConfig::parse(&application_job_with("")).expect("script is optional");

    for literal in [
        "        command: literal-secret-canary\n",
        "        command: { value: literal-secret-canary }\n",
    ] {
        let source = one_schedule(&format!(
            "        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: false\n{literal}"
        ));
        let error =
            DokployConfig::parse(&source).expect_err("a command literal must fail strict parsing");
        assert!(!format!("{error:?}").contains("literal-secret-canary"));
        assert!(!error.to_string().contains("literal-secret-canary"));
    }

    let unsafe_file = DokployConfig::parse(&application_job_with(
        "        script: { file: ../secret }\n",
    ))
    .expect_err("script descriptor files are workspace-relative");
    assert!(
        unsafe_file
            .issues()
            .contains(&ValidationIssue::UnsafeSecretFile)
    );

    let unsafe_env = DokployConfig::parse(&application_job_with(
        "        script: { env: lowercase }\n",
    ))
    .expect_err("script descriptor environment names are validated");
    assert!(
        unsafe_env
            .issues()
            .contains(&ValidationIssue::InvalidSecretEnvironment)
    );
}

#[test]
fn description_and_timezone_cannot_be_cleared() {
    for field in ["description", "timezone"] {
        let error =
            DokployConfig::parse(&application_job_with(&format!("        {field}: null\n")))
                .expect_err("Dokploy has no proven clear for this field");
        assert!(
            error
                .issues()
                .contains(&ValidationIssue::ScheduleFieldCannotBeCleared),
            "{field}: {error:?}"
        );
    }
}

#[test]
fn schedule_ignored_changes_are_limited_to_safe_non_sensitive_in_place_properties() {
    DokployConfig::parse(&application_job_with(
        "        lifecycle: { ignore_changes: [name, cron_expression, shell_type, enabled, description, timezone] }\n",
    ))
    .expect("in-place non-sensitive Schedule properties can be ignored");

    for path in ["target", "service_name", "command", "script", "content"] {
        let error = DokployConfig::parse(&application_job_with(&format!(
            "        lifecycle: {{ ignore_changes: [{path}] }}\n"
        )))
        .expect_err("replacement and sensitive paths cannot be ignored");
        assert!(
            error
                .issues()
                .contains(&ValidationIssue::InvalidIgnoredChange),
            "{path}: {error:?}"
        );
    }
}

#[test]
fn schedules_cannot_be_referenced_as_outputs() {
    let source = with_schedules(
        "      nightly:\n        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: false\n        command: { env: X }\n",
    )
    .replace(
        "      api: {}\n",
        "      api:\n        environment:\n          VALUE:\n            from: schedule.nightly.name\n",
    );
    let error =
        DokployConfig::parse(&source).expect_err("Schedules expose no referenceable output");
    assert!(
        error
            .issues()
            .contains(&ValidationIssue::UnsupportedReferenceProperty)
    );
}

#[test]
fn schedule_issue_codes_are_stable_and_message_safe() {
    for (issue, code) in [
        (ValidationIssue::InvalidScheduleTarget, "DOKCFG050"),
        (ValidationIssue::InvalidScheduleField, "DOKCFG051"),
        (ValidationIssue::DuplicateScheduleCollision, "DOKCFG052"),
        (ValidationIssue::ScheduleSecretCannotBeCleared, "DOKCFG053"),
        (
            ValidationIssue::UnmanagedScheduleCommandRequiresProtection,
            "DOKCFG054",
        ),
        (ValidationIssue::ScheduleComposeServiceMismatch, "DOKCFG055"),
        (ValidationIssue::ScheduleFieldCannotBeCleared, "DOKCFG056"),
    ] {
        assert_eq!(issue.code(), code);
        assert!(issue.to_string().starts_with(code));
    }
}

#[test]
fn generated_schema_models_the_typed_schedule() {
    let schema = serde_json::to_value(DokployConfig::json_schema()).unwrap();
    let raw = &schema["$defs"]["RawSchedule"];

    assert_eq!(raw["additionalProperties"], false);
    assert_eq!(
        raw["required"],
        serde_json::json!(["name", "target", "cron_expression", "shell_type", "enabled"])
    );
    assert_eq!(
        raw["properties"]["command"],
        schema["$defs"]["RawCompose"]["properties"]["document"]
    );
    assert_eq!(raw["properties"]["script"], raw["properties"]["command"]);
    assert!(
        raw["properties"]["target"]["pattern"]
            .as_str()
            .unwrap()
            .contains("application|compose")
    );
    assert!(
        !raw["properties"]["target"]["pattern"]
            .as_str()
            .unwrap()
            .contains("postgres")
    );
    let shells = schema["$defs"]["ScheduleShellConfig"]["oneOf"]
        .as_array()
        .expect("shell type is a closed enumeration")
        .iter()
        .map(|variant| variant["const"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        shells,
        vec![serde_json::json!("bash"), serde_json::json!("sh")]
    );
    assert_eq!(raw["properties"]["timezone"]["maxLength"], 64);
    assert_eq!(raw["properties"]["description"]["maxLength"], 1024);
}

#[test]
fn canonical_writer_round_trips_schedules_without_executable_bytes() {
    let source = with_schedules(
        r#"      nightly:
        name: nightly
        target: application.api
        cron_expression: "0 3 * * *"
        shell_type: bash
        enabled: false
        description: Nightly cleanup
        timezone: UTC
        command:
          env: SCHEDULE_COMMAND
        script:
          file: .secrets/schedule-script
        depends_on:
          - postgres.main
      worker:
        name: worker-job
        target: compose.stack
        service_name: worker
        cron_expression: "@daily"
        shell_type: sh
        enabled: true
        command:
          env: WORKER_COMMAND
        lifecycle:
          protect: true
          ignore_changes: [cron_expression]
      imported:
        name: imported-job
        target: application.api
        cron_expression: "5 4 * * 1"
        shell_type: sh
        enabled: false
        lifecycle:
          protect: true
"#,
    );
    let config = DokployConfig::parse(&source).expect("fixture is valid");

    let rendered = render(&config).expect("Schedule configuration renders");

    assert_eq!(DokployConfig::parse(&rendered).unwrap(), config);
    assert!(rendered.contains("    schedules:\n      imported:\n        name: \"imported-job\"\n"));
    assert!(rendered.contains("        service_name: \"worker\"\n"));
    assert!(rendered.contains("        command:\n          env: \"SCHEDULE_COMMAND\"\n"));
    assert!(rendered.contains("          file: \".secrets/schedule-script\"\n"));
    assert_eq!(
        ConfigDocument::from_config(&config)
            .unwrap()
            .render()
            .unwrap(),
        rendered
    );
}

#[test]
fn typed_document_writes_imported_schedule_with_unmanaged_executables() {
    let name = |value: &str| value.parse::<ResourceName>().unwrap();
    let mut environment = EnvironmentDocument::default();
    environment
        .add_application(name("api"), ApplicationDocument::default())
        .unwrap();
    let document_for = |service_name: Option<&str>| ScheduleDocument {
        name: "nightly".to_owned(),
        target: ResourceAddress::new(ResourceKind::Application, name("api")),
        service_name: service_name.map(str::to_owned),
        cron_expression: "0 3 * * *".to_owned(),
        shell_type: ScheduleShellConfig::Bash,
        enabled: false,
        description: Field::Unmanaged,
        timezone: Field::Unmanaged,
        command: Field::Unmanaged,
        script: Field::Unmanaged,
        depends_on: Vec::new(),
        lifecycle: LifecycleDocument {
            protect: Field::Set(true),
            ..LifecycleDocument::default()
        },
    };
    environment
        .add_schedule(name("nightly"), document_for(None))
        .unwrap();
    assert!(
        environment
            .add_schedule(name("nightly"), document_for(None))
            .is_err(),
        "duplicate Schedule names are rejected"
    );
    let mut document = ConfigDocument::new(name("platform"));
    document
        .add_environment(name("production"), environment)
        .unwrap();

    let rendered = document.render().expect("imported Schedule renders");

    assert!(rendered.contains("        shell_type: \"bash\"\n        enabled: false\n"));
    assert!(!rendered.contains("command"));
    assert!(!rendered.contains("script"));
    assert!(rendered.contains("        lifecycle:\n          protect: true\n"));
}
