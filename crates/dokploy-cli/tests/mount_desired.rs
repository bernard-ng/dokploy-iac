use std::fs;

use dokploy_cli::desired::{CompileDesiredError, compile_desired, compile_desired_for_instance};
use dokploy_config::DokployConfig;
use dokploy_core::{ConfigDigest, OwnedValue, PropertyPath};
use dokploy_state::InstanceIdentity;

const CONTENT_CANARY: &str = "mount-content-canary-never-leak";

fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).unwrap()
}

fn config(mounts: &str) -> DokployConfig {
    DokployConfig::parse(&format!(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api: {{}}
    mounts:
{mounts}
"#
    ))
    .expect("Mount configuration is valid")
}

fn text(value: &str) -> OwnedValue {
    OwnedValue::Value(
        dokploy_core::ComparableValue::try_from_json(serde_json::json!(value)).unwrap(),
    )
}

#[test]
fn compiles_typed_mount_ownership_dependency_and_environment_containment() {
    let config = config(
        r#"      data:
        target: application.api
        mount_path: /data
        source: { type: volume, volume_name: api-data }
      host:
        target: application.api
        mount_path: /host
        source: { type: bind, host_path: /srv/api }
      imported:
        target: application.api
        mount_path: /imported
        source: { type: file, file_path: imported.conf }
        lifecycle: { protect: true }
"#,
    );

    let compiled = compile_desired(&config, digest()).expect("offline Mount compilation succeeds");
    let resources = compiled.desired_state().resources();

    let data = &resources[&"mount.data".parse().unwrap()];
    assert_eq!(
        data.properties()[&PropertyPath::Target],
        text("application.api")
    );
    assert_eq!(data.properties()[&PropertyPath::MountType], text("volume"));
    assert_eq!(data.properties()[&PropertyPath::MountPath], text("/data"));
    assert_eq!(
        data.properties()[&PropertyPath::VolumeName],
        text("api-data")
    );
    assert_eq!(
        data.containment().unwrap().to_string(),
        "environment.production"
    );
    assert_eq!(
        data.dependencies()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["application.api"],
        "the target is an inferred dependency, not containment"
    );

    let host = &resources[&"mount.host".parse().unwrap()];
    assert_eq!(host.properties()[&PropertyPath::MountType], text("bind"));
    assert_eq!(host.properties()[&PropertyPath::HostPath], text("/srv/api"));

    let imported = &resources[&"mount.imported".parse().unwrap()];
    assert_eq!(
        imported.properties()[&PropertyPath::FilePath],
        text("imported.conf")
    );
    assert!(
        !imported
            .properties()
            .contains_key(&PropertyPath::FileContent),
        "unmanaged content is not owned"
    );

    let (target, mount_type, mount_path) = compiled
        .bindings()
        .mount(&"mount.host".parse().unwrap())
        .expect("Mount binding is retained for identity discovery");
    assert_eq!(
        (target.to_string(), mount_type, mount_path),
        ("application.api".to_owned(), "bind", "/host")
    );
}

#[test]
fn concrete_file_content_fails_closed_offline_and_never_enters_debug_output() {
    let config = config(
        r#"      settings:
        target: application.api
        mount_path: /etc/settings.conf
        source:
          type: file
          file_path: settings.conf
          content: { env: MOUNT_TEST_CONTENT }
"#,
    );

    let error = compile_desired(&config, digest())
        .expect_err("concrete sensitive content needs the instance-bound compiler");

    assert!(matches!(
        error,
        CompileDesiredError::SensitiveIntentUnsupported
    ));
}

#[test]
fn instance_compilation_resolves_file_content_once_as_a_fingerprinted_receipt() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("settings.conf"), CONTENT_CANARY).unwrap();
    let config = DokployConfig::parse(
        r#"
version: 1
project: { name: platform }
environments:
  production:
    applications:
      api: {}
    mounts:
      settings:
        target: application.api
        mount_path: /etc/settings.conf
        source:
          type: file
          file_path: settings.conf
          content: { file: settings.conf }
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
    let settings = "mount.settings".parse().unwrap();

    assert!(matches!(
        compiled.desired_state().resources()[&settings].properties()[&PropertyPath::FileContent],
        OwnedValue::Sensitive(_)
    ));
    let rendered = format!("{compiled:?} {:?}", compiled.desired_state());
    assert!(!rendered.contains(CONTENT_CANARY));

    let (bytes, receipt) = compiled
        .take_sensitive(&settings, &PropertyPath::FileContent)
        .expect("resolved content is available to the executor once")
        .into_parts();
    assert_eq!(bytes.as_slice(), CONTENT_CANARY.as_bytes());
    assert!(!format!("{receipt:?}").contains(CONTENT_CANARY));
    assert!(
        compiled
            .take_sensitive(&settings, &PropertyPath::FileContent)
            .is_none()
    );
}

#[test]
fn missing_content_file_is_a_redacted_compile_failure() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let config = config(
        r#"      settings:
        target: application.api
        mount_path: /etc/settings.conf
        source:
          type: file
          file_path: settings.conf
          content: { file: absent-content }
"#,
    );

    let error = compile_desired_for_instance(
        &config,
        digest(),
        InstanceIdentity::parse("https://dokploy.example.test").unwrap(),
        workspace.path(),
    )
    .expect_err("missing content source fails");

    assert!(matches!(
        error,
        CompileDesiredError::SensitiveFileUnavailable
    ));
    assert!(!format!("{error:?} {error}").contains("absent-content"));
}
