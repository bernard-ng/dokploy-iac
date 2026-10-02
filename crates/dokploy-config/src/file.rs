use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};
use thiserror::Error;

pub const DEFAULT_CONFIG_FILE: &str = "dokploy.yaml";
pub const DEFAULT_SETTINGS_FILE: &str = "dokploy.settings.yaml";
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

const TEMPLATE: &str = include_str!("template.yaml");
const SETTINGS_TEMPLATE: &str = include_str!("settings_template.yaml");

/// Loads one regular UTF-8 configuration file through a strict one-MiB read bound.
pub fn load(path: impl AsRef<Path>) -> Result<crate::DokployConfig, ConfigFileError> {
    load_with_digest(path).map(|loaded| loaded.config)
}

/// One parsed configuration and the SHA-256 digest of its exact source bytes.
pub struct LoadedConfig {
    pub config: crate::DokployConfig,
    pub source_sha256: [u8; 32],
}

/// Loads and hashes one configuration through the same single bounded read.
pub fn load_with_digest(path: impl AsRef<Path>) -> Result<LoadedConfig, ConfigFileError> {
    let path = path.as_ref();
    let file = open_for_bounded_read(path)?;
    let opened_metadata = file
        .metadata()
        .map_err(|source| ConfigFileError::Read { source })?;

    if !opened_metadata.is_file() {
        return Err(ConfigFileError::NotRegularFile);
    }
    reject_oversized(opened_metadata.len())?;

    let initial_capacity = usize::try_from(opened_metadata.len())
        .unwrap_or(MAX_CONFIG_BYTES + 1)
        .min(MAX_CONFIG_BYTES + 1);
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| ConfigFileError::Read { source })?;

    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigFileError::Configuration(
            crate::ConfigError::InputTooLarge {
                limit_bytes: MAX_CONFIG_BYTES,
            },
        ));
    }

    let source_sha256 = Sha256::digest(&bytes).into();
    let source = String::from_utf8(bytes)
        .map_err(|_| ConfigFileError::Configuration(crate::ConfigError::ParseWithoutLocation))?;

    let config = crate::DokployConfig::parse(&source).map_err(ConfigFileError::Configuration)?;

    Ok(LoadedConfig {
        config,
        source_sha256,
    })
}

#[cfg(unix)]
fn open_for_bounded_read(path: &Path) -> Result<File, ConfigFileError> {
    use rustix::fs::{Mode, OFlags};

    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;

    match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(descriptor) => Ok(File::from(descriptor)),
        Err(rustix::io::Errno::LOOP) => Err(ConfigFileError::NotRegularFile),
        Err(error) => Err(ConfigFileError::Read {
            source: error.into(),
        }),
    }
}

#[cfg(not(unix))]
fn open_for_bounded_read(path: &Path) -> Result<File, ConfigFileError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|source| ConfigFileError::Read { source })?;

    if !metadata.file_type().is_file() {
        return Err(ConfigFileError::NotRegularFile);
    }
    reject_oversized(metadata.len())?;

    File::open(path).map_err(|source| ConfigFileError::Read { source })
}

fn reject_oversized(length: u64) -> Result<(), ConfigFileError> {
    if length > MAX_CONFIG_BYTES as u64 {
        return Err(ConfigFileError::Configuration(
            crate::ConfigError::InputTooLarge {
                limit_bytes: MAX_CONFIG_BYTES,
            },
        ));
    }

    Ok(())
}

/// Reports which state scope the document at `path` belongs to, without
/// validating the rest of it.
///
/// A settings document is recognized by its top-level `settings` key. Anything
/// else, including a missing file, is a project document, so the caller's own
/// load reports the real error. This lets commands open the matching durable
/// state before (or without) fully validating the configuration.
pub fn peek_scope(path: impl AsRef<Path>) -> Result<dokploy_state::StateScope, ConfigFileError> {
    let path = path.as_ref();
    let file = match open_for_bounded_read(path) {
        Ok(file) => file,
        Err(ConfigFileError::Read { source }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(dokploy_state::StateScope::Project);
        }
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| ConfigFileError::Read { source })?;
    let Ok(source) = String::from_utf8(bytes) else {
        return Ok(dokploy_state::StateScope::Project);
    };

    Ok(crate::DokployConfig::scope_of_source(&source))
}

/// Creates a canonical starter configuration without replacing an existing path.
pub fn initialize(path: impl AsRef<Path>) -> Result<(), ConfigFileError> {
    write_starter(path.as_ref(), TEMPLATE)
}

/// Creates a canonical starter settings document without replacing an existing path.
pub fn initialize_settings(path: impl AsRef<Path>) -> Result<(), ConfigFileError> {
    write_starter(path.as_ref(), SETTINGS_TEMPLATE)
}

fn write_starter(path: &Path, template: &str) -> Result<(), ConfigFileError> {
    let parent = existing_parent(path);
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|source| ConfigFileError::Create { source })?;

    temporary
        .write_all(template.as_bytes())
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| ConfigFileError::Create { source })?;

    temporary
        .persist_noclobber(path)
        .map_err(|error| match error.error.kind() {
            io::ErrorKind::AlreadyExists => ConfigFileError::AlreadyExists,
            _ => ConfigFileError::Create {
                source: error.error,
            },
        })?;

    sync_parent(parent)?;

    Ok(())
}

fn existing_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), ConfigFileError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ConfigFileError::DurabilityUnknown { source })
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), ConfigFileError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum ConfigFileError {
    #[error("the configuration file already exists")]
    AlreadyExists,

    #[error("failed to create the configuration file")]
    Create {
        #[source]
        source: io::Error,
    },

    #[error("the configuration file was created, but its durability could not be confirmed")]
    DurabilityUnknown {
        #[source]
        source: io::Error,
    },

    #[error("failed to read the configuration file")]
    Read {
        #[source]
        source: io::Error,
    },

    #[error("the configuration path is not a regular file")]
    NotRegularFile,

    #[error(transparent)]
    Configuration(crate::ConfigError),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{ConfigFileError, MAX_CONFIG_BYTES, TEMPLATE, initialize, load, load_with_digest};
    use crate::{ConfigError, DokployConfig};

    #[test]
    fn canonical_template_is_valid() {
        DokployConfig::parse(TEMPLATE).expect("canonical template must remain valid");
    }

    #[test]
    fn initialize_writes_the_canonical_template_exactly() {
        let directory = tempfile::tempdir().expect("temporary directory is available");
        let path = directory.path().join("dokploy.yaml");

        initialize(&path).expect("configuration is initialized");

        assert_eq!(
            fs::read_to_string(path).expect("file is readable"),
            TEMPLATE
        );
    }

    #[test]
    fn load_reads_and_validates_a_regular_configuration_file() {
        let directory = tempfile::tempdir().expect("temporary directory is available");
        let path = directory.path().join("dokploy.yaml");
        fs::write(&path, TEMPLATE).expect("fixture is writable");

        let config = load(&path).expect("canonical configuration loads");

        assert_eq!(config.version(), 1);
    }

    #[test]
    fn load_with_digest_hashes_the_exact_bytes_from_the_single_read() {
        use sha2::{Digest, Sha256};

        let directory = tempfile::tempdir().expect("temporary directory is available");
        let path = directory.path().join("dokploy.yaml");
        fs::write(&path, TEMPLATE).expect("fixture is writable");

        let loaded = load_with_digest(&path).expect("canonical configuration loads");

        assert_eq!(loaded.config.version(), 1);
        assert_eq!(
            loaded.source_sha256,
            Sha256::digest(TEMPLATE.as_bytes())[..]
        );
    }

    #[test]
    fn load_rejects_oversized_input() {
        let directory = tempfile::tempdir().expect("temporary directory is available");
        let path = directory.path().join("dokploy.yaml");
        fs::write(&path, vec![b'#'; MAX_CONFIG_BYTES + 1]).expect("fixture is writable");

        let error = load(path).expect_err("oversized input is rejected");

        assert!(matches!(
            error,
            ConfigFileError::Configuration(ConfigError::InputTooLarge { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn load_rejects_symlinks_before_opening_the_target() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory is available");
        let target = directory.path().join("target.yaml");
        let link = directory.path().join("dokploy.yaml");
        fs::write(&target, TEMPLATE).expect("target is writable");
        symlink(&target, &link).expect("symlink is created");

        let error = load(link).expect_err("symlinks are rejected");

        assert!(matches!(error, ConfigFileError::NotRegularFile));
    }

    #[test]
    fn concurrent_initialization_has_exactly_one_winner() {
        let directory = tempfile::tempdir().expect("temporary directory is available");
        let path = Arc::new(directory.path().join("dokploy.yaml"));
        let barrier = Arc::new(Barrier::new(4));
        let threads = (0..4)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);

                thread::spawn(move || {
                    barrier.wait();
                    initialize(path.as_ref())
                })
            })
            .collect::<Vec<_>>();
        let results = threads
            .into_iter()
            .map(|thread| thread.join().expect("initializer thread completes"))
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(ConfigFileError::AlreadyExists)))
                .count(),
            3
        );
        assert_eq!(
            fs::read_to_string(path.as_ref()).expect("winning file is readable"),
            TEMPLATE
        );
    }

    #[cfg(unix)]
    #[test]
    fn initialized_configuration_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory is available");
        let path = directory.path().join("dokploy.yaml");

        initialize(&path).expect("configuration is initialized");

        let mode = fs::metadata(path)
            .expect("configuration metadata is readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn initialize_does_not_replace_a_symlink_or_change_its_target() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory is available");
        let target = directory.path().join("target.yaml");
        let link = directory.path().join("dokploy.yaml");
        let original = b"existing-target-content";
        fs::write(&target, original).expect("target is writable");
        symlink(&target, &link).expect("symlink is created");

        let error = initialize(&link).expect_err("existing symlink is rejected");

        assert!(matches!(error, ConfigFileError::AlreadyExists));
        assert_eq!(fs::read(target).expect("target is readable"), original);
        assert!(
            fs::symlink_metadata(link)
                .expect("symlink metadata is readable")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn initialize_does_not_replace_an_existing_directory() {
        let directory = tempfile::tempdir().expect("temporary directory is available");
        let path = directory.path().join("dokploy.yaml");
        fs::create_dir(&path).expect("existing directory is created");

        let error = initialize(&path).expect_err("existing directory is rejected");

        assert!(matches!(error, ConfigFileError::AlreadyExists));
        assert!(path.is_dir());
    }

    #[test]
    fn initialize_requires_the_parent_directory_to_exist() {
        let directory = tempfile::tempdir().expect("temporary directory is available");
        let missing_parent = directory.path().join("missing");
        let path = missing_parent.join("dokploy.yaml");

        let error = initialize(path).expect_err("missing parent is rejected");

        assert!(matches!(error, ConfigFileError::Create { .. }));
        assert!(!missing_parent.exists());
    }

    #[test]
    fn load_rejects_a_directory_without_opening_it_as_input() {
        let directory = tempfile::tempdir().expect("temporary directory is available");

        let error = load(directory.path()).expect_err("directory input is rejected");

        assert!(matches!(error, ConfigFileError::NotRegularFile));
    }

    #[cfg(unix)]
    #[test]
    fn load_opens_a_fifo_nonblocking_and_rejects_the_opened_handle() {
        use std::process::Command;

        let directory = tempfile::tempdir().expect("temporary directory is available");
        let path = directory.path().join("dokploy.yaml");
        let status = Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo is available on Unix");
        assert!(status.success());

        let error = load(path).expect_err("FIFO input is rejected without blocking");

        assert!(matches!(error, ConfigFileError::NotRegularFile));
    }
}
