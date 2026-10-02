use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::{ResourceName, ResourceNameError, StateScope};

/// Which document a state file tracks (ADR 0009).
///
/// A workspace holds one `settings` document and any number of project documents,
/// each with its own state lineage, lock, journal, and backup, so a crash while
/// applying one never blocks another.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DocumentId {
    /// The instance settings document.
    Settings,
    /// One project document, named by its slug.
    Project(ResourceName),
    /// The first engine's single project document, whose state lives at the
    /// workspace root. Deleted together with the first engine (ADR 0016).
    Workspace,
}

impl DocumentId {
    /// Returns the scope of the resources this document tracks.
    #[must_use]
    pub const fn scope(&self) -> StateScope {
        match self {
            Self::Settings => StateScope::Settings,
            Self::Project(_) | Self::Workspace => StateScope::Project,
        }
    }

    /// Returns the document that holds a scope in a first-engine workspace.
    #[must_use]
    pub const fn for_scope(scope: StateScope) -> Self {
        match scope {
            StateScope::Settings => Self::Settings,
            StateScope::Project => Self::Workspace,
        }
    }

    /// Returns the directories below `.dokploy/` that hold this document's state.
    pub(crate) fn directories(&self) -> Vec<String> {
        match self {
            Self::Workspace => Vec::new(),
            Self::Settings => vec!["settings".to_owned()],
            Self::Project(slug) => vec!["projects".to_owned(), slug.as_str().to_owned()],
        }
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Settings => formatter.write_str("settings"),
            Self::Project(slug) => write!(formatter, "project.{slug}"),
            Self::Workspace => formatter.write_str("project"),
        }
    }
}

impl FromStr for DocumentId {
    type Err = DocumentIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "settings" => Ok(Self::Settings),
            "project" => Ok(Self::Workspace),
            _ => {
                let slug = value
                    .strip_prefix("project.")
                    .ok_or(DocumentIdParseError::Unknown)?;
                Ok(Self::Project(ResourceName::new(slug)?))
            }
        }
    }
}

impl Serialize for DocumentId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DocumentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

/// A document id that is not `settings` or `project.<slug>`.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DocumentIdParseError {
    #[error("a document id is `settings` or `project.<slug>`")]
    Unknown,
    #[error("invalid project slug: {0}")]
    Slug(#[from] ResourceNameError),
}
