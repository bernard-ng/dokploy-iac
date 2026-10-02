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

/// One resource: its fields and the resources it contains.
#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    /// The kind, such as `registry` or `application`.
    pub kind: String,
    /// The key it has in the document (ADR 0006).
    pub key: String,
    /// Where the key is written.
    pub span: Span,
    /// The configured fields, by document name. An omitted field is unmanaged.
    pub fields: BTreeMap<String, Field>,
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
