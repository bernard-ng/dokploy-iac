//! Where a document lives, and which state it belongs to (ADR 0005).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use dokploy_model::{Document, MAX_DOCUMENT_BYTES, Root};
use dokploy_spec::SpecRegistry;
use dokploy_state::{DocumentId, ResourceName};
use miette::{IntoDiagnostic, Result};
use serde::Deserialize;

/// The default project document.
pub const DEFAULT_PROJECT_FILE: &str = "dokploy.yaml";
/// The default settings document.
pub const DEFAULT_SETTINGS_FILE: &str = "dokploy.settings.yaml";
/// The workspace manifest written by every import selection.
pub const DEFAULT_MANIFEST_FILE: &str = "dokploy.workspace.yaml";
const DOKPLOY_DIRECTORY: &str = ".dokploy";

/// A validated multi-document workspace manifest.
pub struct WorkspaceManifest {
    directory: PathBuf,
    documents: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    version: u32,
    documents: Vec<String>,
}

impl WorkspaceManifest {
    /// Reads `dokploy.workspace.yaml` below `directory` and validates every path before any
    /// document is opened.
    pub fn load(directory: &Path) -> Result<Self> {
        let directory = fs::canonicalize(directory).map_err(|error| {
            miette::miette!(
                "cannot read workspace directory {}: {error}",
                directory.display()
            )
        })?;
        let path = directory.join(DEFAULT_MANIFEST_FILE);
        let text = fs::read_to_string(&path)
            .map_err(|error| miette::miette!("cannot read {}: {error}", path.display()))?;
        let raw: RawManifest = serde_saphyr::from_str(&text)
            .map_err(|error| miette::miette!("cannot parse {}: {error}", path.display()))?;
        if raw.version != 1 {
            return Err(miette::miette!(
                "{} has unsupported workspace version {}",
                path.display(),
                raw.version
            ));
        }
        if raw.documents.is_empty() {
            return Err(miette::miette!("{} lists no documents", path.display()));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut documents = Vec::new();
        for document in raw.documents {
            let plain = !document.is_empty()
                && !document.contains(['\\', '\0'])
                && document
                    .split('/')
                    .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
            if !plain || Path::new(&document).is_absolute() {
                return Err(miette::miette!(
                    "workspace document `{document}` is not a relative path below the workspace"
                ));
            }
            if !document.starts_with(".dokploy/") {
                return Err(miette::miette!(
                    "workspace document `{document}` must be below `.dokploy/`"
                ));
            }
            if !seen.insert(document.clone()) {
                return Err(miette::miette!(
                    "workspace document `{document}` is listed twice"
                ));
            }
            documents.push(PathBuf::from(document));
        }

        Ok(Self {
            directory,
            documents,
        })
    }

    /// Canonical workspace root.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Document paths relative to the workspace root, in manifest order.
    #[must_use]
    pub fn documents(&self) -> &[PathBuf] {
        &self.documents
    }
}

/// A document file and the directory that owns its state.
pub struct Workspace {
    pub(crate) directory: PathBuf,
    file: PathBuf,
    text: String,
}

impl Workspace {
    /// Reads the document at `file`. A document below `.dokploy/` uses that directory's parent as
    /// its workspace root; a standalone document uses the directory holding the file.
    pub fn load(file: &Path) -> Result<Self> {
        let canonical = fs::canonicalize(file)
            .map_err(|error| miette::miette!("cannot read {}: {error}", file.display()))?;
        let document_directory = canonical
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let directory = canonical
            .ancestors()
            .find(|ancestor| {
                ancestor
                    .file_name()
                    .is_some_and(|name| name == DOKPLOY_DIRECTORY)
            })
            .and_then(Path::parent)
            .unwrap_or(document_directory)
            .to_path_buf();
        let handle = fs::File::open(&canonical)
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

        Ok(Self {
            directory,
            file: canonical,
            text,
        })
    }

    /// Reads a manifest-owned document while keeping file, content, secret, and state paths
    /// relative to the workspace root.
    pub fn load_in(directory: &Path, relative: &Path) -> Result<Self> {
        let directory = fs::canonicalize(directory).into_diagnostic()?;
        let file = directory.join(relative);
        let canonical = fs::canonicalize(&file)
            .map_err(|error| miette::miette!("cannot read {}: {error}", file.display()))?;
        if !canonical.starts_with(&directory) {
            return Err(miette::miette!(
                "workspace document {} is outside {}",
                file.display(),
                directory.display()
            ));
        }
        let handle = fs::File::open(&canonical)
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

        Ok(Self {
            directory,
            file: canonical,
            text,
        })
    }

    /// Canonical path of the document file.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
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
