use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

/// One problem found in a spec, located by a dotted path inside it.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Issue {
    /// The kind, or the file when the kind is unknown.
    pub subject: String,
    /// A dotted path inside the spec, such as `fields.password.mutability`.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl Issue {
    pub(crate) fn new(
        subject: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(formatter, "{}: {}", self.subject, self.message)
        } else {
            write!(
                formatter,
                "{}: {}: {}",
                self.subject, self.path, self.message
            )
        }
    }
}

/// Failure to load specs from disk.
#[derive(Debug, Error)]
pub enum LoadError {
    /// A file or directory could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A file is not valid spec YAML.
    #[error("{path}: {message}")]
    Parse {
        /// The offending file.
        path: PathBuf,
        /// The parser's message, with a line and column when known.
        message: String,
    },
    /// The specs parsed but are inconsistent.
    #[error("{} spec issue(s):\n{}", .0.len(), join_issues(.0))]
    Invalid(Vec<Issue>),
}

fn join_issues(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(|issue| format!("  - {issue}"))
        .collect::<Vec<_>>()
        .join("\n")
}
