//! A validated document.

use std::collections::BTreeMap;

use dokploy_spec::Scope;

use crate::{Field, Span};

/// Resources of one kind, by key, grouped by the section that holds them.
pub type Sections = BTreeMap<String, BTreeMap<String, Resource>>;

/// A validated document: spans aside, exactly what the specs allow.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// Whether this is a project or the settings document.
    pub scope: Scope,
    /// The document's content.
    pub root: Root,
    /// Addresses that were renamed: the resource keeps its remote identity.
    pub moves: Vec<Move>,
    /// Declared resources that should take ownership of an existing remote identity.
    pub adopt: Vec<Adoption>,
    /// Resources the document stops managing, or deletes.
    pub removed: Vec<Removal>,
}

/// `adopt`: a declared resource that should take ownership of its remote match.
#[derive(Clone, Debug, PartialEq)]
pub struct Adoption {
    /// The declared address, as the user wrote it (a suffix of the full address).
    pub address: String,
    /// Where the entry is written.
    pub span: Span,
}

/// `moves`: a resource now has another address, and is still the same resource.
#[derive(Clone, Debug, PartialEq)]
pub struct Move {
    /// The address it had, as the user wrote it (a suffix of the full address).
    pub from: String,
    /// The address it has now, which the document declares.
    pub to: String,
    /// Where the entry is written.
    pub span: Span,
}

/// `removed`: a resource that is no longer in the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Removal {
    /// The address it had, as the user wrote it (a suffix of the full address).
    pub from: String,
    /// Whether to delete it at Dokploy (`true`) or only stop managing it (`false`).
    pub destroy: bool,
    /// Where the entry is written.
    pub span: Span,
}

/// What a document holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Root {
    /// A project document: the project resource itself, keyed by its `slug`.
    Project(Box<Resource>),
    /// The settings document: sections of top-level resources.
    Settings(Sections),
}

/// Lifecycle directives of one resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lifecycle {
    /// Whether the resource is protected from deletion and replacement.
    pub protect: Option<bool>,
    /// Properties whose remote changes are ignored.
    pub ignore_changes: Vec<String>,
}

/// A dotenv file the document expands into an `env` field (`env_file`, ADR 0010).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvFile {
    /// The document key it is written under, such as `env_file`.
    pub key: String,
    /// The path below the workspace.
    pub path: String,
    /// Whether the file's values are declared non-secret (`public: true`).
    pub public: bool,
    /// Where the key is written.
    pub span: Span,
}

/// One resource: its fields and the resources it contains.
#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    /// The kind, such as `registry` or `application`.
    pub kind: String,
    /// The key it has in the document (ADR 0006).
    pub key: String,
    /// Whether the settings section renders this resource directly, without its fixed key.
    pub singleton: bool,
    /// Where the key is written.
    pub span: Span,
    /// The configured fields, by document name. An omitted field is unmanaged.
    pub fields: BTreeMap<String, Field>,
    /// Dotenv files expanded into an `env` field, by the name of that field. The entries the
    /// field gives itself win over the file's.
    pub env_files: BTreeMap<String, EnvFile>,
    /// Lifecycle directives.
    pub lifecycle: Lifecycle,
    /// Addresses this resource depends on.
    pub depends_on: Vec<String>,
    /// Contained resources, by section.
    pub children: Sections,
}

impl Document {
    /// Every resource in the document, parents before children, in a stable order.
    #[must_use]
    pub fn resources(&self) -> Vec<&Resource> {
        fn walk<'a>(resource: &'a Resource, found: &mut Vec<&'a Resource>) {
            found.push(resource);
            for section in resource.children.values() {
                for child in section.values() {
                    walk(child, found);
                }
            }
        }
        let mut found = Vec::new();
        match &self.root {
            Root::Project(project) => walk(project, &mut found),
            Root::Settings(sections) => {
                for section in sections.values() {
                    for resource in section.values() {
                        walk(resource, &mut found);
                    }
                }
            }
        }
        found
    }
}
