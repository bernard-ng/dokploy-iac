//! Writing secrets out of Dokploy into the workspace (ADR 0010).
//!
//! An importer reads secret values in memory and has to put them somewhere the document can name
//! with a `file` source. This is where: a directory below the workspace (`secrets/` by default),
//! files created with mode 0600 in directories with mode 0700, and a `.gitignore` that keeps the
//! directory out of the repository, so the values are on the operator's disk and nowhere else.
//! No error here ever holds a value.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use thiserror::Error;
use zeroize::Zeroizing;

use crate::dotenv::Variable;

/// The directory secrets go to unless the operator names another.
pub const DEFAULT_SECRETS_DIRECTORY: &str = "secrets";

/// A secret that could not be written.
#[derive(Debug, Error)]
pub enum SecretsOutError {
    /// A path leaves the directory, or is not a plain relative path.
    #[error("`{path}` is not a path below the secrets directory")]
    BadPath { path: String },
    /// The file is there already, and an import does not replace what a person may have edited.
    #[error("`{path}` exists already")]
    Exists { path: String },
    /// A variable cannot be written as a line of a dotenv file.
    #[error("variable `{name}` cannot be written to a dotenv file: {reason}")]
    Unwritable { name: String, reason: &'static str },
    /// The workspace could not be written.
    #[error("cannot write `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Writes secret files below a directory of the workspace.
#[derive(Debug)]
pub struct SecretsWriter {
    root: PathBuf,
    directory: PathBuf,
    shown: String,
}

impl SecretsWriter {
    /// Prepares `directory` (relative to the workspace `root`) and keeps it out of git.
    ///
    /// The directory is created with mode 0700, gets a `.gitignore` of its own that ignores
    /// everything in it, and is added to the workspace's `.gitignore` when that does not already
    /// ignore it.
    pub fn new(root: impl Into<PathBuf>, directory: &str) -> Result<Self, SecretsOutError> {
        let root = root.into();
        let relative = relative_path(directory)?;
        let writer = Self {
            directory: root.join(&relative),
            shown: directory.trim_end_matches('/').to_owned(),
            root,
        };
        writer.make_directory(&writer.directory)?;
        writer.ignore_everything_inside()?;
        writer.ignore_from_the_workspace()?;

        Ok(writer)
    }

    /// The path of a file below the workspace, as a `file` source names it.
    #[must_use]
    pub fn source_path(&self, relative: &str) -> String {
        format!("{}/{}", self.shown, relative.trim_start_matches('/'))
    }

    /// Writes a dotenv file of `variables`. What [`crate::dotenv`]'s reader makes of it is
    /// exactly the values given.
    pub fn write_dotenv(
        &self,
        relative: &str,
        variables: &[Variable],
    ) -> Result<String, SecretsOutError> {
        let mut text = Zeroizing::new(Vec::new());
        let mut seen: Vec<&str> = Vec::new();
        for (name, value) in variables {
            let unwritable = |reason| SecretsOutError::Unwritable {
                name: name.clone(),
                reason,
            };
            if !valid_name(name) {
                return Err(unwritable("it is not a variable name"));
            }
            if seen.contains(&name.as_str()) {
                return Err(unwritable("it is set twice"));
            }
            seen.push(name);
            let Ok(value) = std::str::from_utf8(value) else {
                return Err(unwritable("its value is not UTF-8 text"));
            };
            if value.contains(['\n', '\r']) {
                return Err(unwritable("its value spans lines"));
            }
            text.extend_from_slice(name.as_bytes());
            text.push(b'=');
            text.extend_from_slice(quoted(value).as_bytes());
            text.push(b'\n');
        }

        self.write_file(relative, &text)
    }

    /// Writes one file with mode 0600, atomically, and refuses to replace one that exists.
    /// Returns the path to write in a `file` source.
    pub fn write_file(&self, relative: &str, bytes: &[u8]) -> Result<String, SecretsOutError> {
        let path = self.directory.join(relative_path(relative)?);
        let shown = self.source_path(relative);
        if let Some(parent) = path.parent() {
            self.make_directory(parent)?;
        }
        let io = |source| SecretsOutError::Io {
            path: shown.clone(),
            source,
        };
        if path.symlink_metadata().is_ok() {
            return Err(SecretsOutError::Exists { path: shown });
        }
        // Nothing is ever at the final path half written: the bytes go to a file of their own
        // and are moved into place.
        let temporary = path.with_file_name(format!(
            ".{}.writing",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("secret")
        ));
        let mut file = private_file(&temporary).map_err(io)?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(source) = written.and_then(|()| fs::rename(&temporary, &path)) {
            let _ = fs::remove_file(&temporary);
            return Err(io(source));
        }

        Ok(shown)
    }

    fn make_directory(&self, directory: &Path) -> Result<(), SecretsOutError> {
        // Every directory from the workspace down is made private when this creates it.
        let mut current = self.root.clone();
        let below = directory.strip_prefix(&self.root).unwrap_or(directory);
        for component in below.components() {
            current.push(component);
            if current.symlink_metadata().is_ok() {
                if current.is_symlink() {
                    return Err(SecretsOutError::BadPath {
                        path: self.display(&current),
                    });
                }
                continue;
            }
            create_private_directory(&current).map_err(|source| SecretsOutError::Io {
                path: self.display(&current),
                source,
            })?;
        }

        Ok(())
    }

    /// The directory ignores everything in it, so a workspace `.gitignore` that is edited or lost
    /// does not expose the files.
    fn ignore_everything_inside(&self) -> Result<(), SecretsOutError> {
        let path = self.directory.join(".gitignore");
        if path.exists() {
            return Ok(());
        }
        fs::write(
            &path,
            "# Secrets read from Dokploy. Never commit them.\n*\n!.gitignore\n",
        )
        .map_err(|source| SecretsOutError::Io {
            path: self.display(&path),
            source,
        })
    }

    fn ignore_from_the_workspace(&self) -> Result<(), SecretsOutError> {
        let path = self.root.join(".gitignore");
        let entry = format!("/{}/", self.shown.trim_start_matches("./"));
        let existing = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => {
                return Err(SecretsOutError::Io {
                    path: self.display(&path),
                    source,
                });
            }
        };
        let bare = entry.trim_matches('/');
        let ignored = existing.lines().map(str::trim).any(|line| {
            let line = line.trim_matches('/');
            !line.starts_with('#') && !line.starts_with('!') && line == bare
        });
        if ignored {
            return Ok(());
        }
        let separator = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        fs::write(
            &path,
            format!("{existing}{separator}# Secrets read from Dokploy\n{entry}\n"),
        )
        .map_err(|source| SecretsOutError::Io {
            path: self.display(&path),
            source,
        })
    }

    fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

/// A path of plain segments, as the document format has it: no `..`, no root, no empty or `.`
/// segment.
fn relative_path(text: &str) -> Result<PathBuf, SecretsOutError> {
    let bad = || SecretsOutError::BadPath {
        path: text.to_owned(),
    };
    let trimmed = text.strip_suffix('/').unwrap_or(text);
    let plain = !trimmed.is_empty()
        && !trimmed.contains(['\0', '\\'])
        && trimmed
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if !plain {
        return Err(bad());
    }

    Ok(PathBuf::from(trimmed))
}

fn valid_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

/// A value as the dotenv reader will give it back: one pair of matching quotes around a value
/// that has such a pair is added, because the reader takes one pair off.
fn quoted(value: &str) -> String {
    let bytes = value.as_bytes();
    match bytes {
        [first @ (b'"' | b'\''), .., last] if first == last => {
            let quote = char::from(*first);
            format!("{quote}{value}{quote}")
        }
        _ => value.to_owned(),
    }
}

#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dotenv::parse_dotenv;

    fn variable(name: &str, value: &str) -> Variable {
        (name.to_owned(), Zeroizing::new(value.as_bytes().to_vec()))
    }

    fn read_back(path: &Path) -> Vec<(String, String)> {
        parse_dotenv(&fs::read(path).unwrap())
            .unwrap()
            .into_iter()
            .map(|(name, value)| (name, String::from_utf8(value.to_vec()).unwrap()))
            .collect()
    }

    #[test]
    fn a_dotenv_file_reads_back_as_exactly_the_values_written() {
        let workspace = tempfile::tempdir().unwrap();
        let writer = SecretsWriter::new(workspace.path(), "secrets").unwrap();
        let values = [
            ("PLAIN", "info"),
            ("SPACES", "hello world"),
            ("URL", "postgres://u:p@h/db?a=b&c=d"),
            ("EMPTY", ""),
            ("QUOTED", "\"already quoted\""),
            ("SINGLE", "'x'"),
            ("HALF", "\"open"),
            ("HASH", "a # not a comment"),
        ];
        let variables: Vec<Variable> = values.iter().map(|(n, v)| variable(n, v)).collect();

        let shown = writer.write_dotenv("staging/api.env", &variables).unwrap();

        assert_eq!(shown, "secrets/staging/api.env");
        let expected: Vec<(String, String)> = values
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect();
        assert_eq!(read_back(&workspace.path().join(&shown)), expected);
    }

    #[test]
    fn what_cannot_be_a_line_is_refused_with_the_name_and_never_the_value() {
        let workspace = tempfile::tempdir().unwrap();
        let writer = SecretsWriter::new(workspace.path(), "secrets").unwrap();
        let canary = "writer-canary-1";

        for (variables, fragment) in [
            (vec![variable("BAD NAME", canary)], "not a variable name"),
            (vec![variable("A", "x"), variable("A", canary)], "set twice"),
            (
                vec![variable("A", &format!("{canary}\nB=2"))],
                "spans lines",
            ),
        ] {
            let error = writer.write_dotenv("x.env", &variables).unwrap_err();
            assert!(error.to_string().contains(fragment), "{error}");
            assert!(!error.to_string().contains(canary), "{error}");
        }
        assert!(
            !workspace.path().join("secrets/x.env").exists(),
            "nothing is left behind"
        );
    }

    #[test]
    fn paths_stay_below_the_directory_and_nothing_is_replaced() {
        let workspace = tempfile::tempdir().unwrap();
        let writer = SecretsWriter::new(workspace.path(), "secrets").unwrap();

        for path in [
            "../escape.env",
            "/etc/passwd",
            "a/../../b",
            "",
            "./x",
            "a//b",
        ] {
            assert!(
                matches!(
                    writer.write_file(path, b"x"),
                    Err(SecretsOutError::BadPath { .. })
                ),
                "{path:?}"
            );
        }
        assert!(SecretsWriter::new(workspace.path(), "../out").is_err());
        assert!(SecretsWriter::new(workspace.path(), "/tmp/out").is_err());

        writer.write_file("token.txt", b"first").unwrap();
        assert!(matches!(
            writer.write_file("token.txt", b"second"),
            Err(SecretsOutError::Exists { .. })
        ));
        assert_eq!(
            fs::read(workspace.path().join("secrets/token.txt")).unwrap(),
            b"first"
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_and_directories_are_private_and_a_link_out_is_refused() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let workspace = tempfile::tempdir().unwrap();
        let writer = SecretsWriter::new(workspace.path(), "secrets").unwrap();
        writer.write_file("deep/er/token.txt", b"x").unwrap();

        let mode = |path: &str| {
            fs::metadata(workspace.path().join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("secrets"), 0o700);
        assert_eq!(mode("secrets/deep"), 0o700);
        assert_eq!(mode("secrets/deep/er"), 0o700);
        assert_eq!(mode("secrets/deep/er/token.txt"), 0o600);

        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), workspace.path().join("secrets/link")).unwrap();
        assert!(matches!(
            writer.write_file("link/token.txt", b"x"),
            Err(SecretsOutError::BadPath { .. })
        ));
        assert!(!outside.path().join("token.txt").exists());
    }

    #[test]
    fn the_directory_is_kept_out_of_git_twice_and_once() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join(".gitignore"), "target/\nnode_modules").unwrap();

        SecretsWriter::new(workspace.path(), "secrets").unwrap();
        SecretsWriter::new(workspace.path(), "secrets").unwrap();

        let root = fs::read_to_string(workspace.path().join(".gitignore")).unwrap();
        assert_eq!(
            root, "target/\nnode_modules\n# Secrets read from Dokploy\n/secrets/\n",
            "an entry is added once, after the lines that were there"
        );
        let inner = fs::read_to_string(workspace.path().join("secrets/.gitignore")).unwrap();
        assert!(inner.contains("\n*\n!.gitignore\n"), "{inner}");
    }

    #[test]
    fn an_ignore_the_workspace_already_has_is_not_repeated() {
        for existing in ["secrets/\n", "/secrets\n", "/secrets/\n"] {
            let workspace = tempfile::tempdir().unwrap();
            fs::write(workspace.path().join(".gitignore"), existing).unwrap();

            SecretsWriter::new(workspace.path(), "secrets").unwrap();

            assert_eq!(
                fs::read_to_string(workspace.path().join(".gitignore")).unwrap(),
                existing
            );
        }
    }

    #[test]
    fn a_directory_other_than_the_default_is_ignored_by_its_own_name() {
        let workspace = tempfile::tempdir().unwrap();

        let writer = SecretsWriter::new(workspace.path(), "private/keys").unwrap();

        assert_eq!(writer.source_path("api.env"), "private/keys/api.env");
        assert_eq!(
            fs::read_to_string(workspace.path().join(".gitignore")).unwrap(),
            "# Secrets read from Dokploy\n/private/keys/\n"
        );
    }
}
