use std::fs;

use dokploy_cli::desired::{CompileDesiredError, compile_desired, compile_desired_for_instance};
use dokploy_config::DokployConfig;
use dokploy_core::{ConfigDigest, OwnedValue, PropertyPath};
use dokploy_state::InstanceIdentity;

const COMMAND_CANARY: &str = "schedule-command-canary-never-leak";
const SCRIPT_CANARY: &str = "schedule-script-canary-never-leak";

fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).unwrap()
}

fn config(schedules: &str) -> DokployConfig {
    let schedules = schedules
        .lines()
        .map(|line| format!("  {line}\n"))
        .collect::<String>();
    DokployConfig::parse(&format!(
        r#"
version: 1
project:
  name: platform
  environments:
    production:
      applications:
        api: {{}}
      compose:
        stack:
          lifecycle: {{ protect: true }}
      schedules:
{schedules}
"#
    ))
    .expect("Schedule configuration is valid")
}

fn text(value: &str) -> OwnedValue {
    OwnedValue::Value(
        dokploy_core::ComparableValue::try_from_json(serde_json::json!(value)).unwrap(),
    )
}

fn boolean(value: bool) -> OwnedValue {
    OwnedValue::Value(
        dokploy_core::ComparableValue::try_from_json(serde_json::json!(value)).unwrap(),
    )
}

#[test]
fn compiles_typed_schedule_ownership_dependency_and_environment_containment() {
    let config = config(
        r#"      nightly:
        name: nightly
        target: application.api
        cron_expression: "0 3 * * *"
        shell_type: bash
        enabled: false
        description: Nightly cleanup
        timezone: UTC
      worker:
        name: worker-job
        target: compose.stack
        service_name: worker
        cron_expression: "@daily"
        shell_type: sh
        enabled: true
        lifecycle: { protect: true }
"#
        .replace(
            "        description: Nightly cleanup\n        timezone: UTC\n",
            "        description: Nightly cleanup\n        timezone: UTC\n        lifecycle: { protect: true }\n",
        )
        .as_str(),
    );

    let compiled =
        compile_desired(&config, digest()).expect("offline Schedule compilation succeeds");
    let resources = compiled.desired_state().resources();

    let nightly = &resources[&"schedule.nightly".parse().unwrap()];
    let properties = nightly.properties();
    assert_eq!(properties[&PropertyPath::Target], text("application.api"));
    assert_eq!(properties[&PropertyPath::Name], text("nightly"));
    assert_eq!(properties[&PropertyPath::CronExpression], text("0 3 * * *"));
    assert_eq!(properties[&PropertyPath::ShellType], text("bash"));
    assert_eq!(properties[&PropertyPath::Enabled], boolean(false));
    assert_eq!(
        properties[&PropertyPath::Description],
        text("Nightly cleanup")
    );
    assert_eq!(properties[&PropertyPath::Timezone], text("UTC"));
    assert!(!properties.contains_key(&PropertyPath::ServiceName));
    assert!(
        !properties.contains_key(&PropertyPath::Command),
        "an unmanaged command is not owned"
    );
    assert!(!properties.contains_key(&PropertyPath::Script));
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
        ["application.api"],
        "the target is an inferred dependency, not containment"
    );

    let worker = &resources[&"schedule.worker".parse().unwrap()];
    assert_eq!(
        worker.properties()[&PropertyPath::Target],
        text("compose.stack")
    );
    assert_eq!(
        worker.properties()[&PropertyPath::ServiceName],
        text("worker")
    );
    assert_eq!(worker.properties()[&PropertyPath::Enabled], boolean(true));
    assert_eq!(
        worker
            .dependencies()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["compose.stack"]
    );

    let (target, service, name) = compiled
        .bindings()
        .schedule(&"schedule.worker".parse().unwrap())
        .expect("Schedule binding is retained for identity discovery");
    assert_eq!(
        (target.to_string(), service, name),
        ("compose.stack".to_owned(), Some("worker"), "worker-job")
    );
    let (target, service, name) = compiled
        .bindings()
        .schedule(&"schedule.nightly".parse().unwrap())
        .unwrap();
    assert_eq!(
        (target.to_string(), service, name),
        ("application.api".to_owned(), None, "nightly")
    );
}

#[test]
fn concrete_executables_fail_closed_offline_and_never_enter_debug_output() {
    for field in ["command", "script"] {
        let (command, script) = if field == "command" {
            ("        command: { env: SCHEDULE_TEST_COMMAND }\n", "")
        } else {
            (
                "        command: { env: SCHEDULE_TEST_COMMAND }\n",
                "        script: { env: SCHEDULE_TEST_SCRIPT }\n",
            )
        };
        let config = config(&format!(
            "      nightly:\n        name: nightly\n        target: application.api\n        cron_expression: \"0 3 * * *\"\n        shell_type: sh\n        enabled: false\n{command}{script}"
        ));

        let error = compile_desired(&config, digest())
            .expect_err("concrete executable text needs the instance-bound compiler");

        assert!(
            matches!(error, CompileDesiredError::SensitiveIntentUnsupported),
            "{field}"
        );
    }
}

#[test]
fn instance_compilation_resolves_command_and_script_once_as_fingerprinted_receipts() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("command.sh"), COMMAND_CANARY).unwrap();
    fs::write(workspace.path().join("script.sh"), SCRIPT_CANARY).unwrap();
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
  environments:
    production:
      applications:
        api: {}
      schedules:
        nightly:
          name: nightly
          target: application.api
          cron_expression: "0 3 * * *"
          shell_type: bash
          enabled: false
          command: { file: command.sh }
          script: { file: script.sh }
"#,
    )
    .expect("configuration is valid");

    let mut compiled = compile_desired_for_instance(
        &config,
        digest(),
        InstanceIdentity::parse("https://dokploy.example.test").unwrap(),
        workspace.path(),
    )
    .expect("instance-bound compilation succeeds");
    let nightly = "schedule.nightly".parse().unwrap();

    for path in [PropertyPath::Command, PropertyPath::Script] {
        assert!(matches!(
            compiled.desired_state().resources()[&nightly].properties()[&path],
            OwnedValue::Sensitive(_)
        ));
    }
    let rendered = format!("{compiled:?} {:?}", compiled.desired_state());
    assert!(!rendered.contains(COMMAND_CANARY));
    assert!(!rendered.contains(SCRIPT_CANARY));

    for (path, canary) in [
        (PropertyPath::Command, COMMAND_CANARY),
        (PropertyPath::Script, SCRIPT_CANARY),
    ] {
        let value = compiled
            .take_sensitive(&nightly, &path)
            .expect("resolved text is available to the executor once");
        assert!(!format!("{value:?}").contains(canary));
        let (bytes, receipt) = value.into_parts();
        assert_eq!(bytes.as_slice(), canary.as_bytes());
        assert!(!format!("{receipt:?}").contains(canary));
        assert!(compiled.take_sensitive(&nightly, &path).is_none());
    }
}

#[test]
fn distinct_command_and_script_bytes_produce_distinct_receipts_and_stable_repeats() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("one.sh"), "first").unwrap();
    fs::write(workspace.path().join("two.sh"), "second").unwrap();
    let compile = |file: &str| {
        let config = DokployConfig::parse(&format!(
            r#"
version: 1
project:
  name: platform
  environments:
    production:
      applications:
        api: {{}}
      schedules:
        nightly:
          name: nightly
          target: application.api
          cron_expression: "0 3 * * *"
          shell_type: bash
          enabled: false
          command: {{ file: {file} }}
"#
        ))
        .unwrap();
        let mut compiled = compile_desired_for_instance(
            &config,
            digest(),
            InstanceIdentity::parse("https://dokploy.example.test").unwrap(),
            workspace.path(),
        )
        .unwrap();
        let (_, receipt) = compiled
            .take_sensitive(&"schedule.nightly".parse().unwrap(), &PropertyPath::Command)
            .unwrap()
            .into_parts();
        receipt
    };

    let first = compile("one.sh");
    assert_eq!(first, compile("one.sh"));
    assert_ne!(first, compile("two.sh"));
}

#[test]
fn missing_command_file_is_a_redacted_compile_failure() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let config = config(
        r#"      nightly:
        name: nightly
        target: application.api
        cron_expression: "0 3 * * *"
        shell_type: bash
        enabled: false
        command: { file: absent-command }
"#,
    );

    let error = compile_desired_for_instance(
        &config,
        digest(),
        InstanceIdentity::parse("https://dokploy.example.test").unwrap(),
        workspace.path(),
    )
    .expect_err("missing command source fails");

    assert!(matches!(
        error,
        CompileDesiredError::SensitiveFileUnavailable
    ));
    assert!(!format!("{error:?} {error}").contains("absent-command"));
}
