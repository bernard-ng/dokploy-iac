mod common;

use std::collections::BTreeMap;

use common::{Canned, engine, key, specs};
use dokploy_core::PropertyPath;
use dokploy_engine::{
    CompileError, FingerprintKey, Fingerprinter, SecretError, SecretReader, WorkspaceSecrets,
};
use dokploy_model::Source;
use dokploy_state::{InstanceIdentity, ResourceAddress, SensitivePropertyPath};

const DOCUMENT: &str = "\
version: 2
settings:
  registries:
    ghcr:
      type: cloud
      url: ghcr.io
      username: ci
      password:
        file: secrets/ghcr.token
";

fn compile_in(
    root: &std::path::Path,
    text: &str,
    environment: &[(&str, &str)],
) -> Result<dokploy_engine::Compiled, CompileError> {
    let engine = engine(Canned::new());
    let document = engine.parse(text).expect("valid");
    let environment: BTreeMap<String, String> = environment
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect();
    let secrets = WorkspaceSecrets::new(root, move |name: &str| environment.get(name).cloned());
    engine.compile(&document, &engine.fingerprinter(key()), &secrets)
}

#[test]
fn the_fingerprint_matches_an_independently_computed_hmac_vector() {
    // Pinned against a separate HMAC-SHA-256 over the documented framing (the domain string,
    // then length-prefixed `instance`, `resource`, `property`, `value`), so a change to the
    // receipt format cannot pass by agreeing with itself. A receipt that changes format
    // would make every stored secret look rotated.
    let _kinds = engine(Canned::new());
    let fingerprinter = Fingerprinter::new(
        InstanceIdentity::parse("https://deploy.example.test").unwrap(),
        FingerprintKey::parse_explicit(
            "0199a0c8-2351-7c31-8899-2c8f81983ea5:0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
        )
        .unwrap(),
    );
    let address: ResourceAddress = "registry.cache".parse().unwrap();
    let path = SensitivePropertyPath::parse("password").unwrap();

    let receipt =
        serde_json::to_value(fingerprinter.fingerprint(&address, &path, b"sensitive-value"))
            .unwrap();

    assert_eq!(
        receipt["mac"],
        "cb951ce5c24b48cd949f094ad014a23e69358bc95fdbffa83ce810727dc3f8b9"
    );
}

#[test]
fn every_dimension_of_the_domain_changes_the_receipt() {
    let _kinds = engine(Canned::new());
    let key = || FingerprintKey::parse_explicit(common::KEY).unwrap();
    let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
    let other = InstanceIdentity::parse("https://other.example.test").unwrap();
    let address: ResourceAddress = "registry.api".parse().unwrap();
    let other_address: ResourceAddress = "registry.worker".parse().unwrap();
    let path = SensitivePropertyPath::parse("password").unwrap();
    let other_path = SensitivePropertyPath::parse("username").unwrap();
    let mac = |instance: &InstanceIdentity,
               address: &ResourceAddress,
               path: &SensitivePropertyPath,
               value: &[u8]| {
        serde_json::to_value(
            Fingerprinter::new(instance.clone(), key()).fingerprint(address, path, value),
        )
        .unwrap()["mac"]
            .clone()
    };
    let baseline = mac(&instance, &address, &path, b"v");
    assert_eq!(
        baseline,
        mac(&instance, &address, &path, b"v"),
        "deterministic"
    );
    for changed in [
        mac(&other, &address, &path, b"v"),
        mac(&instance, &other_address, &path, b"v"),
        mac(&instance, &address, &other_path, b"v"),
        mac(&instance, &address, &path, b"w"),
    ] {
        assert_ne!(baseline, changed);
    }
    assert!(FingerprintKey::parse_explicit("not-a-key").is_err());
    assert!(
        format!("{:?}", FingerprintKey::parse_explicit(common::KEY).unwrap()).contains("REDACTED")
    );
}

#[test]
fn a_compiled_document_declares_its_addresses_and_holds_no_secret_value() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("secrets")).unwrap();
    std::fs::write(
        directory.path().join("secrets/ghcr.token"),
        "file-token-canary",
    )
    .unwrap();

    let compiled = compile_in(directory.path(), DOCUMENT, &[]).expect("compiles");

    let addresses: Vec<String> = compiled.addresses().map(ToString::to_string).collect();
    assert_eq!(addresses, ["registry.ghcr"]);
    let shown = format!("{compiled:?}");
    assert!(
        !shown.contains("file-token-canary") && !shown.contains("ghcr.io"),
        "{shown}"
    );

    let desired = compiled.desired();
    let spec = specs();
    let password = PropertyPath::from_spec(spec.get("registry").unwrap(), "password").unwrap();
    assert!(password.is_sensitive());
    let _ = desired;
}

#[test]
fn a_secret_that_cannot_be_read_fails_the_compile_naming_the_source_and_never_a_value() {
    let directory = tempfile::tempdir().unwrap();

    let missing_file = compile_in(directory.path(), DOCUMENT, &[]).expect_err("no such file");
    assert!(
        matches!(
            missing_file,
            CompileError::Secret {
                source: SecretError::UnreadableFile { .. },
                ..
            }
        ),
        "{missing_file}"
    );

    let by_env = DOCUMENT.replace("file: secrets/ghcr.token", "env: GHCR_TOKEN");
    let missing_env = compile_in(directory.path(), &by_env, &[]).expect_err("unset variable");
    assert!(
        missing_env.to_string().contains("GHCR_TOKEN"),
        "{missing_env}"
    );
}

#[test]
fn a_file_source_cannot_leave_the_workspace_even_through_a_symlink() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("stolen.txt"), "outside-canary").unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join("secrets")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("stolen.txt"),
        workspace.path().join("secrets/ghcr.token"),
    )
    .unwrap();

    let secrets = WorkspaceSecrets::new(workspace.path(), |_: &str| None);
    #[cfg(unix)]
    {
        let error = secrets
            .read(&Source::File("secrets/ghcr.token".into()))
            .expect_err("a symlink out of the workspace");
        assert!(
            matches!(error, SecretError::OutsideWorkspace { .. }),
            "{error}"
        );
        assert!(!error.to_string().contains("outside-canary"));
    }

    let big = workspace.path().join("secrets/big");
    std::fs::write(&big, vec![0_u8; 1024 * 1024 + 1]).unwrap();
    assert!(matches!(
        secrets.read(&Source::File("secrets/big".into())),
        Err(SecretError::TooLarge { .. })
    ));
}

#[test]
fn a_vault_source_fingerprints_the_reference_not_a_value() {
    let secrets = WorkspaceSecrets::new(std::env::temp_dir(), |_: &str| None);
    let bytes = secrets
        .read(&Source::Vault {
            provider: "infisical".into(),
            secret: "GHCR".into(),
        })
        .unwrap();
    assert_eq!(&bytes[..], b"vault:infisical/GHCR");
}

#[test]
fn an_unknown_dependency_is_a_compile_error() {
    let document = "\
version: 2
settings:
  registries:
    ghcr:
      depends_on: [registry.nowhere]
      url: ghcr.io
";
    let error =
        compile_in(std::path::Path::new("."), document, &[]).expect_err("nothing to depend on");
    assert!(matches!(error, CompileError::Dependency { .. }), "{error}");
}

#[test]
fn the_digest_follows_the_canonical_text_not_the_layout() {
    let directory = tempfile::tempdir().unwrap();
    let reordered = "\
settings:
  registries:
    ghcr:
      url: ghcr.io
      password: { file: secrets/ghcr.token }
      username: ci
      type: cloud
version: 2
";
    std::fs::create_dir_all(directory.path().join("secrets")).unwrap();
    std::fs::write(directory.path().join("secrets/ghcr.token"), "t").unwrap();
    let first = compile_in(directory.path(), DOCUMENT, &[]).unwrap();
    let second = compile_in(directory.path(), reordered, &[]).unwrap();
    assert_eq!(
        format!("{:?}", first.desired()),
        format!("{:?}", second.desired())
    );
}
