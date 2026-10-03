//! Where a document lives, and which state it belongs to (ADR 0005).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use dokploy_model::{Document, MAX_DOCUMENT_BYTES, Root};
use dokploy_spec::SpecRegistry;
use dokploy_state::{DocumentId, ResourceName};
use miette::{IntoDiagnostic, Result};

/// The default project document.
pub const DEFAULT_PROJECT_FILE: &str = "dokploy.yaml";
/// The default settings document.
pub const DEFAULT_SETTINGS_FILE: &str = "dokploy.settings.yaml";

/// A document file and the directory that owns its state.
pub struct Workspace {
    pub(crate) directory: PathBuf,
    text: String,
}

impl Workspace {
    /// Reads the document at `file`. The directory holding it owns the state, the `files/`
    /// and the secrets it names.
    pub fn load(file: &Path) -> Result<Self> {
        let directory = file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let directory = fs::canonicalize(directory).into_diagnostic()?;
        let handle = fs::File::open(file)
            .map_err(|error| miette::miette!("cannot read {}: {error}", file.display()))?;
        let mut text = String::new();
        let limit = u64::try_from(MAX_DOCUMENT_BYTES).unwrap_or(u64::MAX);
        handle
            .take(limit + 1)
            .read_to_string(&mut text)
            .map_err(|error| miette::miette!("cannot read {}: {error}", file.display()))?;
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(miette::miette!(
                "{} is larger than the {MAX_DOCUMENT_BYTES}-byte input limit",
                file.display()
            ));
        }

        Ok(Self { directory, text })
    }

    /// Parses and validates the document against the kind specs.
    pub fn parse(&self, specs: &SpecRegistry) -> Result<Document> {
        Document::parse(&self.text, specs).map_err(|diagnostics| {
            miette::miette!("The document failed validation:\n{diagnostics}")
        })
    }
}

/// The state a document is tracked in.
pub fn document_id(document: &Document) -> Result<DocumentId> {
    match &document.root {
        Root::Settings(_) => Ok(DocumentId::Settings),
        Root::Project(project) => ResourceName::new(project.key.clone())
            .map(DocumentId::Project)
            .map_err(|error| miette::miette!("the project slug is not a valid key: {error}")),
    }
}
