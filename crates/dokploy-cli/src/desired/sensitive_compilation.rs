//! One-pass sensitive source resolution for desired-state compilation.
//!
//! This private module is the deep implementation behind the instance-bound
//! compiler seam. It owns preflight ordering, credential access, source
//! resolution, bounded file I/O, receipt derivation, effective digest
//! construction, and one-shot execution bindings.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::Path,
};

use dokploy_config::{ConfigValue, DokployConfig, Field, ResourceConfig, SecretSource};
use dokploy_core::{ConfigDigest, PropertyPath};
use dokploy_state::{
    InstanceIdentity, ResourceAddress, SensitiveFingerprint, SensitivePropertyPath,
};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::sensitive::{SensitiveFingerprintError, SensitiveFingerprinter};

use super::{
    CompileDesiredError, CompiledDesired, SensitiveExecutionValue, compile_desired,
    compile_desired_with_fingerprints,
};

const MAX_SENSITIVE_FILE_BYTES: usize = 1024 * 1024;
const EFFECTIVE_DIGEST_DOMAIN: &[u8] = b"dokploy-iac\0effective-config-digest\0v1";
const RECEIPT_IDENTITY_DOMAIN: &[u8] = b"dokploy-iac\0sensitive-receipt-identity\0v1";

pub(super) fn compile_for_instance(
    config: &DokployConfig,
    source_digest: ConfigDigest,
    instance: InstanceIdentity,
    workspace_directory: &Path,
) -> Result<CompiledDesired, CompileDesiredError> {
    let pending = preflight_sensitive_inputs(config)?;
    if pending.is_empty() {
        return compile_desired(config, source_digest);
    }

    compile_preflighted_sensitive_with_loader(
        config,
        source_digest,
        instance,
        workspace_directory,
        pending,
        &SystemSensitiveFingerprinterLoader,
        &SystemSensitiveSourceResolver,
    )
}

trait SensitiveFingerprinterLoader {
    fn load(
        &self,
        instance: InstanceIdentity,
    ) -> Result<SensitiveFingerprinter, SensitiveFingerprintError>;
}

#[derive(Clone, Copy)]
struct SystemSensitiveFingerprinterLoader;

impl SensitiveFingerprinterLoader for SystemSensitiveFingerprinterLoader {
    fn load(
        &self,
        instance: InstanceIdentity,
    ) -> Result<SensitiveFingerprinter, SensitiveFingerprintError> {
        SensitiveFingerprinter::load(instance)
    }
}

enum PendingSensitiveSource<'config> {
    Literal(&'config str),
    Environment(&'config str),
    File(&'config str),
}

enum EnvironmentSource {
    Missing,
    NotUtf8,
    Value(Zeroizing<Vec<u8>>),
}

#[derive(Clone, Copy, Debug)]
enum SensitiveFileReadError {
    Unavailable,
    TooLarge,
}

trait SensitiveSourceResolver {
    fn environment(&self, name: &str) -> EnvironmentSource;

    fn file(
        &self,
        workspace: &Path,
        path: &str,
    ) -> Result<Zeroizing<Vec<u8>>, SensitiveFileReadError>;
}

#[derive(Clone, Copy)]
struct SystemSensitiveSourceResolver;

impl SensitiveSourceResolver for SystemSensitiveSourceResolver {
    fn environment(&self, name: &str) -> EnvironmentSource {
        read_environment(name)
    }

    fn file(
        &self,
        workspace: &Path,
        path: &str,
    ) -> Result<Zeroizing<Vec<u8>>, SensitiveFileReadError> {
        read_sensitive_file(workspace, path)
    }
}

#[cfg(unix)]
fn read_environment(name: &str) -> EnvironmentSource {
    let Some(value) = std::env::var_os(name) else {
        return EnvironmentSource::Missing;
    };

    unix_environment_source(value)
}

#[cfg(unix)]
fn unix_environment_source(value: std::ffi::OsString) -> EnvironmentSource {
    use std::os::unix::ffi::OsStringExt;

    let bytes = Zeroizing::new(value.into_vec());
    if std::str::from_utf8(bytes.as_slice()).is_err() {
        return EnvironmentSource::NotUtf8;
    }

    EnvironmentSource::Value(bytes)
}

#[cfg(not(unix))]
fn read_environment(name: &str) -> EnvironmentSource {
    let Some(value) = std::env::var_os(name) else {
        return EnvironmentSource::Missing;
    };
    let Ok(value) = value.into_string() else {
        return EnvironmentSource::NotUtf8;
    };

    EnvironmentSource::Value(Zeroizing::new(value.into_bytes()))
}

fn preflight_sensitive_inputs(
    config: &DokployConfig,
) -> Result<
    BTreeMap<(ResourceAddress, PropertyPath), PendingSensitiveSource<'_>>,
    CompileDesiredError,
> {
    let mut inputs = BTreeMap::new();
    let mut contains_reference = false;

    for (address, resource) in config.resources() {
        match resource {
            ResourceConfig::Application(application) => {
                if let Field::Set(environment) = application.environment() {
                    for (name, value) in environment {
                        let Field::Set(value) = value else {
                            continue;
                        };
                        let path = PropertyPath::environment_variable(name)
                            .expect("validated configuration has valid environment names");
                        match value {
                            ConfigValue::Literal(value) => {
                                inputs.insert(
                                    (address.clone(), path),
                                    PendingSensitiveSource::Literal(value),
                                );
                            }
                            ConfigValue::Secret(source) => {
                                inputs
                                    .insert((address.clone(), path), pending_secret_source(source));
                            }
                            ConfigValue::Reference(_) => contains_reference = true,
                        }
                    }
                }
            }
            ResourceConfig::Postgres(postgres) => {
                if let Field::Set(source) = postgres.password() {
                    inputs.insert(
                        (address.clone(), PropertyPath::Password),
                        pending_secret_source(source),
                    );
                }
            }
            ResourceConfig::MySql(mysql) => {
                if let Field::Set(source) = mysql.password() {
                    inputs.insert(
                        (address.clone(), PropertyPath::Password),
                        pending_secret_source(source),
                    );
                }
                if let Field::Set(source) = mysql.root_password() {
                    inputs.insert(
                        (address.clone(), PropertyPath::RootPassword),
                        pending_secret_source(source),
                    );
                }
            }
            ResourceConfig::MariaDb(mariadb) => {
                if let Field::Set(source) = mariadb.password() {
                    inputs.insert(
                        (address.clone(), PropertyPath::Password),
                        pending_secret_source(source),
                    );
                }
                if let Field::Set(source) = mariadb.root_password() {
                    inputs.insert(
                        (address.clone(), PropertyPath::RootPassword),
                        pending_secret_source(source),
                    );
                }
            }
            ResourceConfig::Mongo(mongo) => {
                if let Field::Set(source) = mongo.password() {
                    inputs.insert(
                        (address.clone(), PropertyPath::Password),
                        pending_secret_source(source),
                    );
                }
            }
            ResourceConfig::LibSql(libsql) => {
                if let Field::Set(source) = libsql.password() {
                    inputs.insert(
                        (address.clone(), PropertyPath::Password),
                        pending_secret_source(source),
                    );
                }
            }
            ResourceConfig::Redis(redis) => {
                if let Field::Set(source) = redis.password() {
                    inputs.insert(
                        (address.clone(), PropertyPath::Password),
                        pending_secret_source(source),
                    );
                }
            }
            ResourceConfig::Project(_)
            | ResourceConfig::Environment(_)
            | ResourceConfig::Domain(_) => {}
        }
    }

    if contains_reference {
        return Err(CompileDesiredError::SensitiveIntentUnsupported);
    }

    Ok(inputs)
}

fn pending_secret_source(source: &SecretSource) -> PendingSensitiveSource<'_> {
    match source {
        SecretSource::Env(name) => PendingSensitiveSource::Environment(name),
        SecretSource::File(path) => PendingSensitiveSource::File(path),
    }
}

#[cfg(test)]
fn compile_for_instance_with(
    config: &DokployConfig,
    source_digest: ConfigDigest,
    instance: InstanceIdentity,
    workspace_directory: &Path,
    loader: &dyn SensitiveFingerprinterLoader,
    resolver: &dyn SensitiveSourceResolver,
) -> Result<CompiledDesired, CompileDesiredError> {
    let pending = preflight_sensitive_inputs(config)?;
    if pending.is_empty() {
        return compile_desired(config, source_digest);
    }

    compile_preflighted_sensitive_with_loader(
        config,
        source_digest,
        instance,
        workspace_directory,
        pending,
        loader,
        resolver,
    )
}

fn compile_preflighted_sensitive_with_loader(
    config: &DokployConfig,
    source_digest: ConfigDigest,
    instance: InstanceIdentity,
    workspace_directory: &Path,
    pending: BTreeMap<(ResourceAddress, PropertyPath), PendingSensitiveSource<'_>>,
    loader: &dyn SensitiveFingerprinterLoader,
    resolver: &dyn SensitiveSourceResolver,
) -> Result<CompiledDesired, CompileDesiredError> {
    let fingerprinter = loader
        .load(instance)
        .map_err(|_| CompileDesiredError::SensitiveFingerprintUnavailable)?;
    let mut fingerprints = BTreeMap::new();
    let mut sensitive = BTreeMap::new();

    for ((address, path), source) in pending {
        let value = resolve_sensitive_source(source, workspace_directory, resolver)?;
        let sensitive_path = SensitivePropertyPath::parse(&path.to_string())
            .expect("preflight emits only sensitive planner paths");
        let fingerprint = fingerprinter.fingerprint(&address, &sensitive_path, value.as_slice());

        fingerprints.insert((address.clone(), path.clone()), fingerprint.clone());
        sensitive.insert(
            (address, path),
            SensitiveExecutionValue { value, fingerprint },
        );
    }

    let digest = effective_digest(&source_digest, &fingerprints);
    let mut compiled = compile_desired_with_fingerprints(config, digest, &fingerprints)?;
    compiled.bindings.sensitive = sensitive;

    Ok(compiled)
}

fn resolve_sensitive_source(
    source: PendingSensitiveSource<'_>,
    workspace_directory: &Path,
    resolver: &dyn SensitiveSourceResolver,
) -> Result<Zeroizing<Vec<u8>>, CompileDesiredError> {
    match source {
        PendingSensitiveSource::Literal(value) => Ok(Zeroizing::new(value.as_bytes().to_vec())),
        PendingSensitiveSource::Environment(name) => match resolver.environment(name) {
            EnvironmentSource::Missing => Err(CompileDesiredError::SensitiveEnvironmentMissing),
            EnvironmentSource::NotUtf8 => Err(CompileDesiredError::SensitiveEnvironmentNotUtf8),
            EnvironmentSource::Value(value) => Ok(value),
        },
        PendingSensitiveSource::File(path) => {
            let value = resolver
                .file(workspace_directory, path)
                .map_err(|error| match error {
                    SensitiveFileReadError::Unavailable => {
                        CompileDesiredError::SensitiveFileUnavailable
                    }
                    SensitiveFileReadError::TooLarge => CompileDesiredError::SensitiveFileTooLarge,
                })?;
            if value.len() > MAX_SENSITIVE_FILE_BYTES {
                return Err(CompileDesiredError::SensitiveFileTooLarge);
            }
            std::str::from_utf8(value.as_slice())
                .map_err(|_| CompileDesiredError::SensitiveFileNotUtf8)?;

            Ok(value)
        }
    }
}

fn effective_digest(
    source_digest: &ConfigDigest,
    fingerprints: &BTreeMap<(ResourceAddress, PropertyPath), SensitiveFingerprint>,
) -> ConfigDigest {
    if fingerprints.is_empty() {
        return source_digest.clone();
    }

    let mut digest = Sha256::new();
    digest.update(EFFECTIVE_DIGEST_DOMAIN);
    update_digest_field(&mut digest, b"source", source_digest.as_str().as_bytes());

    for ((address, path), fingerprint) in fingerprints {
        update_digest_field(&mut digest, b"resource", address.to_string().as_bytes());
        update_digest_field(&mut digest, b"property", path.to_string().as_bytes());

        let mut receipt_digest = Sha256::new();
        receipt_digest.update(RECEIPT_IDENTITY_DOMAIN);
        serde_json::to_writer(DigestWriter(&mut receipt_digest), fingerprint)
            .expect("hash writers cannot fail");
        update_digest_field(
            &mut digest,
            b"receipt",
            receipt_digest.finalize().as_slice(),
        );
    }

    ConfigDigest::parse(encode_hex(digest.finalize().as_slice()))
        .expect("SHA-256 is a canonical configuration digest")
}

struct DigestWriter<'digest>(&'digest mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn update_digest_field(digest: &mut Sha256, label: &[u8], value: &[u8]) {
    digest.update(
        u64::try_from(label.len())
            .expect("field labels fit in u64")
            .to_be_bytes(),
    );
    digest.update(label);
    digest.update(
        u64::try_from(value.len())
            .expect("memory slices fit in u64")
            .to_be_bytes(),
    );
    digest.update(value);
}

fn encode_hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);

    for byte in value {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }

    encoded
}

#[cfg(unix)]
fn read_sensitive_file(
    workspace: &Path,
    relative_path: &str,
) -> Result<Zeroizing<Vec<u8>>, SensitiveFileReadError> {
    use rustix::fs::{Mode, OFlags, open, openat};

    let segments =
        sensitive_file_segments(relative_path).ok_or(SensitiveFileReadError::Unavailable)?;
    let (final_name, parent_segments) = segments
        .split_last()
        .ok_or(SensitiveFileReadError::Unavailable)?;
    let directory_flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY;
    let mut directory = open(workspace, directory_flags, Mode::empty())
        .map_err(|_| SensitiveFileReadError::Unavailable)?;

    for segment in parent_segments {
        directory = openat(&directory, *segment, directory_flags, Mode::empty())
            .map_err(|_| SensitiveFileReadError::Unavailable)?;
    }

    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let descriptor = openat(&directory, *final_name, flags, Mode::empty())
        .map_err(|_| SensitiveFileReadError::Unavailable)?;
    let file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|_| SensitiveFileReadError::Unavailable)?;
    if !metadata.is_file() {
        return Err(SensitiveFileReadError::Unavailable);
    }
    if metadata.len() > MAX_SENSITIVE_FILE_BYTES as u64 {
        return Err(SensitiveFileReadError::TooLarge);
    }

    let initial_capacity = usize::try_from(metadata.len())
        .unwrap_or(MAX_SENSITIVE_FILE_BYTES + 1)
        .min(MAX_SENSITIVE_FILE_BYTES + 1);
    let mut value = Zeroizing::new(Vec::with_capacity(initial_capacity));
    file.take((MAX_SENSITIVE_FILE_BYTES + 1) as u64)
        .read_to_end(&mut value)
        .map_err(|_| SensitiveFileReadError::Unavailable)?;
    if value.len() > MAX_SENSITIVE_FILE_BYTES {
        return Err(SensitiveFileReadError::TooLarge);
    }

    Ok(value)
}

fn sensitive_file_segments(value: &str) -> Option<Vec<&str>> {
    if value.is_empty()
        || value.len() > 4096
        || value.contains(['\\', ':'])
        || value.starts_with(['~', '/'])
        || Path::new(value).is_absolute()
    {
        return None;
    }

    let mut segments: Vec<_> = value.split('/').collect();
    if segments.first() == Some(&".") {
        segments.remove(0);
    }
    if segments.is_empty()
        || segments
            .iter()
            .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
    {
        return None;
    }

    Some(segments)
}

#[cfg(not(unix))]
fn read_sensitive_file(
    _workspace: &Path,
    _relative_path: &str,
) -> Result<Zeroizing<Vec<u8>>, SensitiveFileReadError> {
    Err(SensitiveFileReadError::Unavailable)
}

#[cfg(test)]
mod tests;
