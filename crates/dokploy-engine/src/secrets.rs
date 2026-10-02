//! Reading secret sources (ADR 0010).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use dokploy_model::Source;
use thiserror::Error;
use zeroize::Zeroizing;

/// The largest file read as a secret or content source.
pub const MAX_SOURCE_BYTES: u64 = 1024 * 1024;

/// A secret source that cannot be read. Messages name the source, never a value.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SecretError {
    /// The environment variable is not set.
    #[error("environment variable `{name}` is not set")]
    MissingEnvironment { name: String },
    /// The file does not exist or cannot be read.
    #[error("cannot read the file `{path}`")]
    UnreadableFile { path: String },
    /// The path leaves the workspace.
    #[error("the file `{path}` is outside the workspace")]
    OutsideWorkspace { path: String },
    /// The file is larger than the limit.
    #[error("the file `{path}` is larger than {MAX_SOURCE_BYTES} bytes")]
    TooLarge { path: String },
}

/// Reads the value behind a source.
///
/// The bytes are returned in a buffer that is zeroised when dropped, used for one
/// fingerprint or one request, and never stored.
pub trait SecretReader {
    /// Reads a source's value.
    fn read(&self, source: &Source) -> Result<Zeroizing<Vec<u8>>, SecretError>;
}

/// Reads `env` sources from a lookup and `file` sources from below a workspace root.
///
/// A `vault` source has no value for this tool: Dokploy resolves it, so what is
/// fingerprinted is the reference.
pub struct WorkspaceSecrets<F> {
    root: PathBuf,
    environment: F,
}

impl<F: Fn(&str) -> Option<String>> WorkspaceSecrets<F> {
    /// Reads files below `root` and environment variables through `environment`.
    pub fn new(root: impl Into<PathBuf>, environment: F) -> Self {
        Self {
            root: root.into(),
            environment,
        }
    }
}

impl<F: Fn(&str) -> Option<String>> SecretReader for WorkspaceSecrets<F> {
    fn read(&self, source: &Source) -> Result<Zeroizing<Vec<u8>>, SecretError> {
        match source {
            Source::Env(name) => (self.environment)(name)
                .map(|value| Zeroizing::new(value.into_bytes()))
                .ok_or_else(|| SecretError::MissingEnvironment { name: name.clone() }),
            Source::File(path) => read_workspace_file(&self.root, path),
            Source::Vault { provider, secret } => Ok(Zeroizing::new(
                format!("vault:{provider}/{secret}").into_bytes(),
            )),
        }
    }
}

fn read_workspace_file(root: &Path, relative: &str) -> Result<Zeroizing<Vec<u8>>, SecretError> {
    let unreadable = || SecretError::UnreadableFile {
        path: relative.to_owned(),
    };
    let root = fs::canonicalize(root).map_err(|_| unreadable())?;
    // Canonicalising resolves symlinks, so a link out of the workspace is caught here.
    let path = fs::canonicalize(root.join(relative)).map_err(|_| unreadable())?;
    if !path.starts_with(&root) {
        return Err(SecretError::OutsideWorkspace {
            path: relative.to_owned(),
        });
    }
    let file = fs::File::open(&path).map_err(|_| unreadable())?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(SecretError::TooLarge {
            path: relative.to_owned(),
        });
    }

    Ok(bytes)
}
