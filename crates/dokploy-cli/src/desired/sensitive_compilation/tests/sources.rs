use super::*;

#[test]
fn unsupported_reference_fails_preflight_before_keyring_or_any_source_access() {
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
          A_LITERAL:
            value: literal-that-must-not-resolve
          B_ENV:
            secret:
              env: MUST_NOT_READ
          C_FILE:
            secret:
              file: secrets/must-not-read
          D_REFERENCE:
            from: postgres.main.connection_url
    postgres:
      main: {}
"#,
    )
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let error = compile_for_instance_with(
        &config,
        digest('a'),
        instance(),
        workspace.path(),
        &PanicFingerprinterLoader,
        &PanicSourceResolver,
    )
    .expect_err("references remain unsupported");

    assert_eq!(error.code(), "DOKCMP004");
}

#[test]
fn clear_and_unmanaged_sensitive_fields_remain_offline() {
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
          CLEARED: null
    postgres:
      main:
        password: null
    redis:
      cache: {}
"#,
    )
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let compiled = compile_for_instance_with(
        &config,
        digest('a'),
        instance(),
        workspace.path(),
        &PanicFingerprinterLoader,
        &PanicSourceResolver,
    )
    .expect("clear and unmanaged fields require no external access");

    assert_eq!(compiled.desired_state().digest(), &digest('a'));
}

#[test]
fn remaining_database_secrets_are_fingerprinted_and_bound_once() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    mariadb:
      main:
        password:
          env: MARIADB_PASSWORD
        root_password:
          env: MARIADB_ROOT_PASSWORD
    mongo:
      documents:
        password:
          env: MONGO_PASSWORD
    libsql:
      edge:
        password:
          env: LIBSQL_PASSWORD
"#,
    )
    .unwrap();
    let resolver = RecordingSourceResolver {
        environment: BTreeMap::from([
            ("MARIADB_PASSWORD".to_owned(), Ok(b"mariadb-user".to_vec())),
            (
                "MARIADB_ROOT_PASSWORD".to_owned(),
                Ok(b"mariadb-root".to_vec()),
            ),
            ("MONGO_PASSWORD".to_owned(), Ok(b"mongo-password".to_vec())),
            (
                "LIBSQL_PASSWORD".to_owned(),
                Ok(b"libsql-password".to_vec()),
            ),
        ]),
        ..RecordingSourceResolver::default()
    };
    let loader = existing_fingerprinter_loader();
    let workspace = tempfile::tempdir().unwrap();
    let mut compiled = compile_for_instance_with(
        &config,
        digest('a'),
        instance(),
        workspace.path(),
        &loader,
        &resolver,
    )
    .unwrap();

    for (address, path, expected) in [
        (
            "mariadb.main",
            PropertyPath::Password,
            b"mariadb-user".as_slice(),
        ),
        (
            "mariadb.main",
            PropertyPath::RootPassword,
            b"mariadb-root".as_slice(),
        ),
        (
            "mongo.documents",
            PropertyPath::Password,
            b"mongo-password".as_slice(),
        ),
        (
            "libsql.edge",
            PropertyPath::Password,
            b"libsql-password".as_slice(),
        ),
    ] {
        let address: ResourceAddress = address.parse().unwrap();
        assert!(matches!(
            compiled.desired_state().resources()[&address]
                .properties()
                .get(&path),
            Some(OwnedValue::Sensitive(_))
        ));
        let value = compiled
            .take_sensitive(&address, &path)
            .expect("configured secret has one execution binding");
        assert_eq!(value.into_parts().0.as_slice(), expected);
        assert!(compiled.take_sensitive(&address, &path).is_none());
    }

    let debug = format!("{compiled:?}");
    for canary in [
        "mariadb-user",
        "mariadb-root",
        "mongo-password",
        "libsql-password",
    ] {
        assert!(!debug.contains(canary));
    }
}

#[test]
fn literal_environment_and_file_sources_resolve_once_with_exact_untrimmed_bytes() {
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
          A_ENV:
            secret:
              env: TEST_SECRET
          B_FILE:
            secret:
              file: secrets/token
          C_LITERAL:
            value: " literal\n"
"#,
    )
    .unwrap();
    let resolver = RecordingSourceResolver {
        environment: BTreeMap::from([("TEST_SECRET".to_owned(), Ok(b"environment\n".to_vec()))]),
        files: BTreeMap::from([("secrets/token".to_owned(), Ok(b"file\n".to_vec()))]),
        ..RecordingSourceResolver::default()
    };
    let loader = existing_fingerprinter_loader();
    let workspace = tempfile::tempdir().unwrap();

    let mut compiled = compile_for_instance_with(
        &config,
        digest('a'),
        instance(),
        workspace.path(),
        &loader,
        &resolver,
    )
    .unwrap();
    let address: ResourceAddress = "application.api".parse().unwrap();
    let take = |compiled: &mut CompiledDesired, name: &str| {
        compiled
            .take_sensitive(&address, &PropertyPath::environment_variable(name).unwrap())
            .unwrap()
            .into_parts()
            .0
    };

    assert_eq!(take(&mut compiled, "A_ENV").as_slice(), b"environment\n");
    assert_eq!(take(&mut compiled, "B_FILE").as_slice(), b"file\n");
    assert_eq!(take(&mut compiled, "C_LITERAL").as_slice(), b" literal\n");
    assert_eq!(
        resolver.calls.borrow().as_slice(),
        ["env:TEST_SECRET", "file:secrets/token"]
    );
    assert_eq!(loader.loads.get(), 1);
    assert!(
        compiled
            .take_sensitive(
                &address,
                &PropertyPath::environment_variable("A_ENV").unwrap(),
            )
            .is_none()
    );
}

#[test]
fn source_failures_have_stable_redacted_diagnostics() {
    let environment_config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    redis:
      cache:
        password:
          env: SECRET_NAME_CANARY
"#,
    )
    .unwrap();
    let file_config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    postgres:
      main:
        password:
          file: secrets/path-canary
"#,
    )
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let compile_environment = |resolver: &RecordingSourceResolver| {
        compile_for_instance_with(
            &environment_config,
            digest('a'),
            instance(),
            workspace.path(),
            &existing_fingerprinter_loader(),
            resolver,
        )
        .expect_err("source must fail")
    };
    let compile_file = |resolver: &RecordingSourceResolver| {
        compile_for_instance_with(
            &file_config,
            digest('a'),
            instance(),
            workspace.path(),
            &existing_fingerprinter_loader(),
            resolver,
        )
        .expect_err("source must fail")
    };

    let missing_environment = compile_environment(&RecordingSourceResolver::default());
    let invalid_environment = compile_environment(&RecordingSourceResolver {
        environment: BTreeMap::from([("SECRET_NAME_CANARY".to_owned(), Err(()))]),
        ..RecordingSourceResolver::default()
    });
    let missing_file = compile_file(&RecordingSourceResolver::default());
    let oversized_file = compile_file(&RecordingSourceResolver {
        files: BTreeMap::from([(
            "secrets/path-canary".to_owned(),
            Err(SensitiveFileReadError::TooLarge),
        )]),
        ..RecordingSourceResolver::default()
    });
    let invalid_file = compile_file(&RecordingSourceResolver {
        files: BTreeMap::from([("secrets/path-canary".to_owned(), Ok(vec![0xff, b'x']))]),
        ..RecordingSourceResolver::default()
    });

    for (error, code) in [
        (missing_environment, "DOKCMP006"),
        (invalid_environment, "DOKCMP007"),
        (missing_file, "DOKCMP008"),
        (oversized_file, "DOKCMP009"),
        (invalid_file, "DOKCMP010"),
    ] {
        assert_eq!(error.code(), code);
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("SECRET_NAME_CANARY"));
        assert!(!rendered.contains("path-canary"));
    }
}

#[cfg(unix)]
#[test]
fn unix_environment_adapter_preserves_exact_utf8_and_rejects_non_utf8() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let exact = unix_environment_source(OsString::from_vec(b" value\r\n".to_vec()));
    let invalid = unix_environment_source(OsString::from_vec(vec![0xff, b'x']));

    match exact {
        EnvironmentSource::Value(value) => assert_eq!(value.as_slice(), b" value\r\n"),
        EnvironmentSource::Missing | EnvironmentSource::NotUtf8 => {
            panic!("exact UTF-8 bytes must be retained")
        }
    }
    assert!(matches!(invalid, EnvironmentSource::NotUtf8));
}

#[test]
fn concrete_sensitive_input_never_bypasses_key_storage() {
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
            value: secret-value-canary
"#,
    )
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let error = compile_for_instance_with(
        &config,
        digest('a'),
        instance(),
        workspace.path(),
        &FailingFingerprinterLoader,
        &PanicSourceResolver,
    )
    .expect_err("key storage must be mandatory");

    assert_eq!(error.code(), "DOKCMP005");
    assert!(!format!("{error:?} {error}").contains("secret-value-canary"));
}
