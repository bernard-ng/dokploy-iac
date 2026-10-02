//! Adoption of the instance settings into a new settings workspace.
//!
//! Settings import reads the organization's tag collection once, proves offline
//! that the first plan will be empty, re-reads the collection to prove it did
//! not change underneath the import, and only then writes the settings document
//! and its settings-scope state.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use dokploy_config::{Field, SettingsDocument, TagDocument};
use dokploy_sdk::{Dokploy, ResponseField, TagCollection};
use dokploy_state::{
    ExpectedState, InstanceIdentity, ResourceAddress, ResourceKind, ResourceName, StateFile,
    StateScope, StateStore,
};

use super::{
    ImportError, ImportedResource, canonical_workspace, converge, insert_response, logical_name,
    resource_state,
};

/// The most tags one import will adopt.
const TAG_IMPORT_LIMIT: usize = 10_000;

/// A complete settings import selection.
pub struct SettingsImportRequest {
    pub config_file: PathBuf,
}

/// What a settings import adopted, for the operator.
#[derive(Debug)]
pub struct SettingsImportReport {
    tags: Vec<TagReport>,
}

#[derive(Debug)]
struct TagReport {
    address: ResourceAddress,
    remote_name: String,
}

impl SettingsImportReport {
    /// The number of resources recorded in the new state.
    #[must_use]
    pub const fn resource_count(&self) -> usize {
        self.tags.len()
    }
}

impl fmt::Display for SettingsImportReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "Instance settings")?;
        if self.tags.is_empty() {
            return writeln!(formatter, "  no tags");
        }
        writeln!(formatter, "  {} tag(s)", self.tags.len())?;
        for tag in &self.tags {
            if tag.address.name().as_str() != tag.remote_name {
                writeln!(formatter, "  {} (remote: {})", tag.address, tag.remote_name)?;
            }
        }

        Ok(())
    }
}

/// Imports the instance settings without any remote mutation.
pub async fn import_settings(
    client: &Dokploy,
    request: SettingsImportRequest,
) -> Result<SettingsImportReport, ImportError> {
    let workspace = canonical_workspace(&request.config_file)?;
    if request.config_file.exists() {
        return Err(ImportError::ConfigExists);
    }
    let instance = InstanceIdentity::parse(client.base_url().as_str())?;
    let store = StateStore::with_scope(&workspace, instance.clone(), StateScope::Settings)?;
    if store.inspect()?.is_some() {
        return Err(ImportError::StateExists);
    }

    let collection = client.tags().all().await?;
    let built = build(&collection)?;
    let rendered = built.document.render()?;
    let state = StateFile::new_with_resources_in_scope(
        env!("CARGO_PKG_VERSION")
            .parse()
            .expect("crate version is valid semver"),
        instance,
        StateScope::Settings,
        built
            .resources
            .into_iter()
            .map(|resource| (resource.address, resource.state))
            .collect::<BTreeMap<_, _>>(),
    )?;
    converge::verify(&rendered, &state)?;

    // The collection must be exactly what the first read saw.
    if client.tags().all().await? != collection {
        return Err(ImportError::RemoteChanged);
    }

    let mut session = store.begin_write()?;
    if store.inspect()?.is_some() || request.config_file.exists() {
        return Err(ImportError::WorkspaceChanged);
    }
    super::persist_settings(&built.document, &request.config_file, || {
        session.checkpoint(ExpectedState::absent(), &state)
    })?;

    Ok(SettingsImportReport {
        tags: built.reports,
    })
}

struct Built {
    document: SettingsDocument,
    resources: Vec<ImportedResource>,
    reports: Vec<TagReport>,
}

/// Turns the tag collection into a document and its state, or fails closed.
fn build(collection: &TagCollection) -> Result<Built, ImportError> {
    let tags = collection.tags();
    if tags.len() > TAG_IMPORT_LIMIT {
        return Err(ImportError::TooLarge);
    }
    let mut identities = BTreeSet::new();
    let mut names = BTreeSet::new();
    for tag in tags {
        if tag.name.is_empty() || tag.tag_id.as_str().is_empty() {
            return Err(ImportError::InvalidRemoteTopology);
        }
        if !identities.insert(tag.tag_id.as_str()) {
            return Err(ImportError::InvalidRemoteTopology);
        }
        // Reconciliation finds a tag by name, so a shared name could not plan.
        if !names.insert(tag.name.as_str()) {
            return Err(ImportError::DuplicateName { kind: "tag" });
        }
    }

    let mut ordered = tags.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.tag_id.cmp(&right.tag_id));
    let mut taken = BTreeSet::<ResourceName>::new();
    let mut document = SettingsDocument::new();
    let mut resources = Vec::new();
    let mut reports = Vec::new();
    for tag in ordered {
        let base = logical_name(&tag.name);
        let mut logical = base.clone();
        let mut counter = 2_u32;
        while taken.contains(&logical) {
            logical = ResourceName::new(format!("{}-{counter}", base.as_str()))?;
            counter += 1;
        }
        taken.insert(logical.clone());

        let address = ResourceAddress::new(ResourceKind::Tag, logical.clone());
        let color = match &tag.color {
            ResponseField::Value(color) if !color.is_empty() => Field::Set(color.clone()),
            _ => Field::Unmanaged,
        };
        document.add_tag(
            logical.clone(),
            TagDocument {
                name: (logical.as_str() != tag.name).then(|| tag.name.clone()),
                color: color.clone(),
                depends_on: Vec::new(),
                lifecycle: super::context::protected_lifecycle(),
            },
        )?;

        let mut inputs = serde_json::Map::new();
        inputs.insert("name".to_owned(), serde_json::json!(tag.name));
        if matches!(color, Field::Set(_)) {
            insert_response(&mut inputs, "color", &tag.color);
        }
        resources.push(ImportedResource {
            state: resource_state(&address, tag.tag_id.as_str(), true, inputs, None)?,
            address: address.clone(),
        });
        reports.push(TagReport {
            address,
            remote_name: tag.name.clone(),
        });
    }

    Ok(Built {
        document,
        resources,
        reports,
    })
}
