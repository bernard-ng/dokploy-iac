//! Read-only adoption of a Dokploy instance into canonical version 2 documents.
//!
//! The importer is generic over kind specs. It inventories the remote containment tree, assigns
//! deterministic document keys, maps remote fields through the same spec paths used by compile,
//! compiles the rendered documents, and proves their imported state plans no changes before it
//! returns anything to persist (ADR 0011).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use dokploy_core::CheckpointMaterializationError;
use dokploy_model::{
    DeployPolicy, Document, EnvFile, EnvValue, Field as DocumentField, Lifecycle, Resource, Root,
    Sections, Selector, Source, Span, Value,
};
use dokploy_sdk::{Error as SdkError, OperationRequest, Transport};
use dokploy_spec::{
    Field as SpecField, FieldType, Granularity, KindSpec, Mutability, Scope, ValueClass, parse_type,
};
use dokploy_state::{RemoteId, ResourceAddress, ResourceKind, ResourceName, StateFile};
use semver::Version;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::canonical::canonical_remote;
use crate::discover::{MAX_COLLECTION_ITEMS, read_collection};
use crate::envtext::EnvText;
use crate::project::{api_name, held_member_ids, item_id, lookup};
use crate::secrets_out::render_dotenv;
use crate::{CompileError, Engine, EngineError, Fingerprinter, SecretError, SecretReader};

const DOKPLOY_DIRECTORY: &str = ".dokploy";
const CONTENT_DIRECTORY: &str = ".dokploy/files";
const MANIFEST_FILE: &str = "dokploy.workspace.yaml";

/// Which documents an import returns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportSelection {
    /// The instance settings document.
    Settings,
    /// One project selected by remote id.
    Project(String),
    /// Settings and every project.
    All,
}

/// How remote environment variables are represented.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnvironmentImportMode {
    /// Classify likely credentials conservatively; public values remain inline.
    #[default]
    Auto,
    /// Write every value inline.
    Inline,
    /// Write every value to a secret file source.
    File,
    /// Leave environment fields unmanaged.
    Skip,
}

/// The inventory pass currently reading Dokploy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportInventoryPass {
    /// The first pass used to construct the imported documents.
    Initial,
    /// The final pass used to prove that the remote topology stayed stable.
    Verification,
}

/// A redaction-safe import milestone suitable for terminal and CI progress output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportProgress {
    /// A remote inventory pass is about to begin.
    InventoryStarted {
        /// Why the inventory is running.
        pass: ImportInventoryPass,
        /// Number of resource kinds that will be visited.
        total_kinds: usize,
    },
    /// One resource kind is about to be read from Dokploy.
    InventoryKind {
        /// Why the inventory is running.
        pass: ImportInventoryPass,
        /// One-based position of this kind in the pass.
        current: usize,
        /// Number of resource kinds in the pass.
        total: usize,
        /// Declarative kind name. Remote identities and values are never included.
        kind: String,
    },
    /// The importer is converting the inventory into canonical documents.
    BuildingDocuments,
    /// One generated document is about to be compiled and checked for convergence.
    ProvingDocument {
        /// One-based position of this document in the proof.
        current: usize,
        /// Number of documents in the proof.
        total: usize,
        /// Workspace-relative document path.
        path: String,
    },
}

/// Import behavior that does not depend on the destination filesystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportOptions {
    /// Environment classification policy.
    pub environment: EnvironmentImportMode,
    /// Workspace-relative directory used in generated secret sources.
    pub secrets_directory: String,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            environment: EnvironmentImportMode::Auto,
            secrets_directory: crate::DEFAULT_SECRETS_DIRECTORY.to_owned(),
        }
    }
}

/// One reason something discovered remotely was not written to a document.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UnsupportedItem {
    /// Stable category used by the CLI report.
    pub category: UnsupportedCategory,
    /// Kind whose field or collection was skipped.
    pub kind: String,
    /// Field path, operation, or remote identity being described.
    pub item: String,
    /// Redaction-safe explanation.
    pub reason: String,
}

/// The closed groups of the unsupported ledger from ADR 0011.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum UnsupportedCategory {
    /// The operator selected a policy that deliberately omits the value.
    UnmanagedByChoice,
    /// The beta specs do not expose a readable representation.
    Unsupported,
    /// Dokploy returned a value outside the document type.
    Unrepresentable,
}

/// One canonical document and the state that adopts its remote resources.
pub struct ImportedDocument {
    path: String,
    document: Document,
    state: StateFile,
}

impl ImportedDocument {
    /// Workspace-relative destination path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Canonical YAML.
    #[must_use]
    pub fn render(&self) -> String {
        self.document.render()
    }

    /// Adopted state used by the first plan.
    #[must_use]
    pub const fn state(&self) -> &StateFile {
        &self.state
    }

    /// Parsed document, primarily for callers that run an additional proof.
    #[must_use]
    pub const fn document(&self) -> &Document {
        &self.document
    }
}

/// A generated file referred to by an imported document.
pub struct ImportedFile {
    path: String,
    bytes: Zeroizing<Vec<u8>>,
    secret: bool,
}

impl ImportedFile {
    /// Workspace-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Whether the file must be written through [`crate::SecretsWriter`].
    #[must_use]
    pub const fn is_secret(&self) -> bool {
        self.secret
    }

    /// File bytes. Callers must not print secret file contents.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for ImportedFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImportedFile")
            .field("path", &self.path)
            .field("secret", &self.secret)
            .field("length", &self.bytes.len())
            .finish()
    }
}

/// Everything an import proved and is ready to persist.
pub struct ImportBundle {
    documents: Vec<ImportedDocument>,
    files: Vec<ImportedFile>,
    unsupported: Vec<UnsupportedItem>,
    secrets_directory: String,
}

impl ImportBundle {
    /// Canonical documents, settings before projects and projects by path.
    #[must_use]
    pub fn documents(&self) -> &[ImportedDocument] {
        &self.documents
    }

    /// Content and secret files referred to by the documents.
    #[must_use]
    pub fn files(&self) -> &[ImportedFile] {
        &self.files
    }

    /// Everything intentionally omitted or impossible to represent.
    #[must_use]
    pub fn unsupported(&self) -> &[UnsupportedItem] {
        &self.unsupported
    }

    /// A deterministic workspace manifest persisted as the workspace's public entry point.
    #[must_use]
    pub fn manifest(&self) -> String {
        let mut text = String::from("version: 1\ndocuments:\n");
        for document in &self.documents {
            text.push_str("  - ");
            text.push_str(document.path());
            text.push('\n');
        }
        text
    }

    /// Persists the already-proved bundle into a workspace that has no imported documents or
    /// state. Every destination is checked before the first write. A failed write removes the
    /// artifacts and state created by this call and restores the workspace `.gitignore`.
    pub fn persist(&self, root: impl AsRef<Path>) -> Result<(), ImportPersistError> {
        let root = root.as_ref();
        let existed = root.exists();
        if existed && !root.is_dir() {
            return Err(ImportPersistError::NotDirectory {
                path: root.display().to_string(),
            });
        }
        if !existed {
            fs::create_dir_all(root).map_err(|source| ImportPersistError::Io {
                path: root.display().to_string(),
                source,
            })?;
        }
        let root = fs::canonicalize(root).map_err(|source| ImportPersistError::Io {
            path: root.display().to_string(),
            source,
        })?;
        self.check_destinations(&root)?;

        let gitignore = root.join(".gitignore");
        let gitignore_before = fs::read(&gitignore).ok();
        let secret_gitignore = root.join(&self.secrets_directory).join(".gitignore");
        let secret_gitignore_before = fs::read(&secret_gitignore).ok();
        let mut created = Vec::new();
        let result = self.persist_checked(&root, &mut created);
        if let Err(error) = result {
            for path in created.iter().rev() {
                let _ = if path.is_dir() {
                    fs::remove_dir_all(path)
                } else {
                    fs::remove_file(path)
                };
            }
            let _ = fs::remove_dir_all(root.join(DOKPLOY_DIRECTORY));
            match gitignore_before {
                Some(bytes) => {
                    let _ = fs::write(&gitignore, bytes);
                }
                None => {
                    let _ = fs::remove_file(&gitignore);
                }
            }
            match secret_gitignore_before {
                Some(bytes) => {
                    let _ = fs::write(&secret_gitignore, bytes);
                }
                None => {
                    let _ = fs::remove_file(&secret_gitignore);
                }
            }
            if !existed {
                let _ = fs::remove_dir_all(&root);
            }
            return Err(error);
        }

        Ok(())
    }

    fn check_destinations(&self, root: &Path) -> Result<(), ImportPersistError> {
        let mut paths: Vec<PathBuf> = self
            .documents
            .iter()
            .map(|document| root.join(document.path()))
            .chain(self.files.iter().map(|file| root.join(file.path())))
            .collect();
        paths.push(root.join(MANIFEST_FILE));
        paths.push(root.join(DOKPLOY_DIRECTORY));
        if let Some(path) = paths
            .into_iter()
            .find(|path| path.symlink_metadata().is_ok())
        {
            return Err(ImportPersistError::Exists {
                path: path.display().to_string(),
            });
        }

        Ok(())
    }

    fn persist_checked(
        &self,
        root: &Path,
        created: &mut Vec<PathBuf>,
    ) -> Result<(), ImportPersistError> {
        let secret_directory = root.join(&self.secrets_directory);
        let secret_directory_existed = secret_directory.exists();
        let secret_writer = self
            .files
            .iter()
            .any(ImportedFile::is_secret)
            .then(|| crate::SecretsWriter::new(root, &self.secrets_directory))
            .transpose()?;
        if secret_writer.is_some() && !secret_directory_existed {
            created.push(secret_directory);
        }
        for file in &self.files {
            if file.is_secret() {
                let relative = file
                    .path()
                    .strip_prefix(self.secrets_directory.trim_end_matches('/'))
                    .and_then(|path| path.strip_prefix('/'))
                    .ok_or_else(|| ImportPersistError::SecretPath {
                        path: file.path().to_owned(),
                    })?;
                secret_writer
                    .as_ref()
                    .expect("secret artifacts create a writer")
                    .write_file(relative, file.bytes())?;
                created.push(root.join(file.path()));
            } else {
                let path = root.join(file.path());
                create_file(&path, file.bytes())?;
                created.push(path);
            }
        }
        for document in &self.documents {
            let path = root.join(document.path());
            create_file(&path, document.render().as_bytes())?;
            created.push(path);
        }
        let path = root.join(MANIFEST_FILE);
        create_file(&path, self.manifest().as_bytes())?;
        created.push(path);
        for document in &self.documents {
            let store = dokploy_state::StateStore::for_document(
                root,
                document.state().instance().clone(),
                document.state().document().clone(),
            )?;
            let mut write = store.begin_write()?;
            write.checkpoint(dokploy_state::ExpectedState::absent(), document.state())?;
        }
        created.push(root.join(DOKPLOY_DIRECTORY));

        Ok(())
    }
}

impl std::fmt::Debug for ImportBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImportBundle")
            .field("documents", &self.documents.len())
            .field("files", &self.files)
            .field("unsupported", &self.unsupported)
            .finish()
    }
}

/// A proved import bundle that could not be persisted safely.
#[derive(Debug, Error)]
pub enum ImportPersistError {
    /// The destination path is not a directory.
    #[error("import destination `{path}` is not a directory")]
    NotDirectory { path: String },
    /// Import never replaces a document, artifact, or state directory.
    #[error("import destination `{path}` already exists")]
    Exists { path: String },
    /// A secret artifact is not below the configured secret directory.
    #[error("secret artifact `{path}` is outside the configured secret directory")]
    SecretPath { path: String },
    /// A file could not be created or synced.
    #[error("cannot persist `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The hardened secret writer refused the destination.
    #[error(transparent)]
    Secrets(#[from] crate::SecretsOutError),
    /// Durable state could not be initialized.
    #[error(transparent)]
    State(#[from] dokploy_state::StateStoreError),
}

/// A remote instance that cannot be imported faithfully.
#[derive(Debug, Error)]
pub enum ImportError {
    /// Secret artifacts must stay below one workspace-relative `.dokploy` directory.
    #[error("secret output directory `{path}` must be a safe path below `.dokploy/`")]
    SecretsDirectory { path: String },
    /// A secret value cannot be represented by the generated file format.
    #[error(transparent)]
    SecretArtifact(#[from] crate::SecretsOutError),
    /// A remote read failed or returned an invalid response.
    #[error("cannot inventory `{kind}`: {reason}")]
    Inventory { kind: String, reason: String },
    /// A project id selected no project.
    #[error("project `{id}` was not found")]
    ProjectNotFound { id: String },
    /// A remote name cannot become a deterministic document key.
    #[error("cannot allocate a key for `{kind}` remote id `{remote_id}`")]
    Key { kind: String, remote_id: String },
    /// A remote relation names an object outside the imported inventory.
    #[error("`{kind}.{field}` references unknown remote id `{remote_id}`")]
    UnknownReference {
        kind: String,
        field: String,
        remote_id: String,
    },
    /// A generated document did not parse or compile.
    #[error("generated `{path}` does not satisfy the document contract: {reason}")]
    Generated { path: String, reason: String },
    /// Desired state could not be materialized as imported state.
    #[error("cannot adopt `{address}` into state: {source}")]
    Checkpoint {
        address: ResourceAddress,
        #[source]
        source: CheckpointMaterializationError,
    },
    /// The generated document and imported state do not immediately converge.
    #[error("generated `{path}` does not converge: {summary}")]
    DoesNotConverge { path: String, summary: String },
    /// A compiler failure while proving convergence.
    #[error(transparent)]
    Compile(#[from] CompileError),
    /// A planner or discovery failure while proving convergence.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// The topology changed between the initial crawl and the final check.
    #[error("Dokploy topology changed during import; nothing is safe to write")]
    TopologyChanged,
}

#[derive(Clone)]
struct RemoteNode {
    kind: String,
    id: RemoteId,
    parent: Option<RemoteParent>,
    raw: Json,
    key: Option<String>,
    address: Option<ResourceAddress>,
}

type RemoteParent = (String, String);
type AllocationGroup = (Option<RemoteParent>, String, String);
type TopologyFingerprint = Vec<(String, String, Option<RemoteParent>)>;

impl RemoteNode {
    fn identity(&self) -> (&str, &str) {
        (&self.kind, self.id.as_str())
    }
}

struct BuildContext<'a> {
    specs: &'a dokploy_spec::SpecRegistry,
    nodes: &'a [RemoteNode],
    options: &'a ImportOptions,
    files: Vec<ImportedFile>,
    unsupported: Vec<UnsupportedItem>,
}

impl<T: Transport> Engine<T> {
    /// Inventories, builds, re-parses, compiles, and proves an import without writing files.
    pub async fn import(
        &self,
        selection: ImportSelection,
        fingerprinter: &Fingerprinter,
        options: &ImportOptions,
    ) -> Result<ImportBundle, ImportError> {
        self.import_with_progress(selection, fingerprinter, options, |_| {})
            .await
    }

    /// Inventories and proves an import while reporting redaction-safe progress milestones.
    pub async fn import_with_progress<F>(
        &self,
        selection: ImportSelection,
        fingerprinter: &Fingerprinter,
        options: &ImportOptions,
        mut progress: F,
    ) -> Result<ImportBundle, ImportError>
    where
        F: FnMut(&ImportProgress),
    {
        if !safe_relative_directory(&options.secrets_directory) {
            return Err(ImportError::SecretsDirectory {
                path: options.secrets_directory.clone(),
            });
        }
        let mut nodes = inventory(
            &self.transport,
            &self.specs,
            ImportInventoryPass::Initial,
            &mut progress,
        )
        .await?;
        allocate(&self.specs, &mut nodes)?;
        let topology = topology_fingerprint(&nodes);

        progress(&ImportProgress::BuildingDocuments);
        let mut build = BuildContext {
            specs: &self.specs,
            nodes: &nodes,
            options,
            files: Vec::new(),
            unsupported: ignored_remote_fields(&self.specs, &nodes),
        };
        let built = build_documents(&selection, &mut build)?;
        let secrets = ArtifactSecrets::new(&build.files);
        let mut documents = Vec::new();
        let total_documents = built.len();
        for (index, (path, document, ids)) in built.into_iter().enumerate() {
            progress(&ImportProgress::ProvingDocument {
                current: index + 1,
                total: total_documents,
                path: path.clone(),
            });
            let text = document.render();
            let reparsed = self.parse(&text).map_err(|error| ImportError::Generated {
                path: path.clone(),
                reason: error.to_string(),
            })?;
            let compiled = self.compile(&reparsed, fingerprinter, &secrets)?;
            let mut resources = BTreeMap::new();
            let deployments = compiled
                .desired()
                .resources()
                .iter()
                .filter(|(address, _)| {
                    compiled
                        .resources
                        .get(*address)
                        .and_then(|resource| self.specs.get(&resource.spec_kind))
                        .is_some_and(|spec| spec.api.deploy.is_some())
                })
                .map(|(address, desired)| (address.clone(), desired.deployment_digest()))
                .collect();
            for (address, desired) in compiled.desired().resources() {
                let remote_id =
                    ids.get(address)
                        .cloned()
                        .ok_or_else(|| ImportError::Generated {
                            path: path.clone(),
                            reason: format!("no remote identity for `{address}`"),
                        })?;
                let adopted =
                    desired
                        .materialize_adopted(address, remote_id)
                        .map_err(|source| ImportError::Checkpoint {
                            address: address.clone(),
                            source,
                        })?;
                resources.insert(address.clone(), adopted);
            }
            let state = StateFile::with_resources_and_deployments(
                Version::parse(env!("CARGO_PKG_VERSION")).expect("package version is semver"),
                self.instance.clone(),
                compiled.document().clone(),
                resources,
                deployments,
            )
            .map_err(EngineError::from)?;
            let plan = self.plan(&compiled, Some(&state)).await?;
            if !plan.complete() || !plan.applyable() || !plan.changes().is_empty() {
                return Err(ImportError::DoesNotConverge {
                    path,
                    summary: String::from_utf8_lossy(&plan.to_json_bytes()).into_owned(),
                });
            }
            documents.push(ImportedDocument {
                path,
                document: reparsed,
                state,
            });
        }

        let mut checked = inventory(
            &self.transport,
            &self.specs,
            ImportInventoryPass::Verification,
            &mut progress,
        )
        .await?;
        allocate(&self.specs, &mut checked)?;
        if topology != topology_fingerprint(&checked) {
            return Err(ImportError::TopologyChanged);
        }

        documents.sort_by(|left, right| left.path.cmp(&right.path));
        documents.sort_by_key(|document| !matches!(document.document.root, Root::Settings(_)));
        build
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        build.unsupported.sort();

        Ok(ImportBundle {
            documents,
            files: build.files,
            unsupported: build.unsupported,
            secrets_directory: options.secrets_directory.clone(),
        })
    }
}

async fn inventory<T: Transport>(
    transport: &T,
    specs: &dokploy_spec::SpecRegistry,
    pass: ImportInventoryPass,
    progress: &mut impl FnMut(&ImportProgress),
) -> Result<Vec<RemoteNode>, ImportError> {
    let mut nodes = Vec::new();
    let mut ordered: Vec<&KindSpec> = specs.kinds().collect();
    ordered.sort_by_key(|spec| (kind_depth(specs, &spec.kind), spec.kind.as_str()));

    let total_kinds = ordered.len();
    progress(&ImportProgress::InventoryStarted { pass, total_kinds });

    for (index, spec) in ordered.into_iter().enumerate() {
        progress(&ImportProgress::InventoryKind {
            pass,
            current: index + 1,
            total: total_kinds,
            kind: spec.kind.clone(),
        });
        if let Some(singleton) = &spec.api.read.singleton {
            let mut raw = singleton_read(transport, singleton)
                .await
                .map_err(|reason| inventory_error(spec, reason))?;
            if !raw.is_object() {
                return Err(inventory_error(spec, "singleton response is not an object"));
            }
            merge_singleton_entries(transport, spec, &mut raw).await?;
            let id = item_id(spec, &raw)
                .and_then(|value| RemoteId::new(value).ok())
                .ok_or_else(|| inventory_error(spec, "singleton response has no identity"))?;
            nodes.push(RemoteNode {
                kind: spec.kind.clone(),
                id,
                parent: None,
                raw,
                key: None,
                address: None,
            });
            continue;
        }

        let Some(list) = &spec.api.read.list else {
            continue;
        };
        let parents: Vec<Option<(String, String)>> = if spec.parents.is_empty() {
            vec![None]
        } else {
            nodes
                .iter()
                .filter(|node| spec.parents.contains(&node.kind))
                .map(|node| Some((node.kind.clone(), node.id.as_str().to_owned())))
                .collect()
        };
        for parent in parents {
            let parent_ref = parent
                .as_ref()
                .map(|(kind, id)| (kind.as_str(), id.as_str()));
            let items = read_collection(transport, specs, spec, parent_ref)
                .await
                .map_err(|reason| inventory_error(spec, format!("{reason:?}")))?;
            if items.len() > MAX_COLLECTION_ITEMS {
                return Err(inventory_error(spec, "collection exceeds the item limit"));
            }
            let mut seen = BTreeSet::new();
            for listed in items {
                let Some(id_text) = item_id(spec, &listed).map(str::to_owned) else {
                    continue;
                };
                if !seen.insert(id_text.clone()) {
                    return Err(inventory_error(spec, "collection repeats an identity"));
                }
                let Some(raw) = complete_item(transport, spec, &listed).await? else {
                    // Shared collection operations can contain rows owned by another spec.
                    continue;
                };
                let id = RemoteId::new(id_text).map_err(|_| {
                    inventory_error(spec, "collection contains an invalid identity")
                })?;
                nodes.push(RemoteNode {
                    kind: spec.kind.clone(),
                    id,
                    parent: parent.clone(),
                    raw,
                    key: None,
                    address: None,
                });
            }
        }
        if list.embedded_in.is_some() && spec.parents.is_empty() {
            return Err(inventory_error(spec, "top-level collection is embedded"));
        }
    }

    let mut identities = BTreeSet::new();
    for node in &nodes {
        if !identities.insert((node.kind.clone(), node.id.as_str().to_owned())) {
            return Err(ImportError::Inventory {
                kind: node.kind.clone(),
                reason: "the same identity was discovered twice".to_owned(),
            });
        }
    }

    Ok(nodes)
}

async fn singleton_read<T: Transport>(
    transport: &T,
    read: &dokploy_spec::SingletonRead,
) -> Result<Json, String> {
    let mut value = transport
        .call(OperationRequest::new(&read.op))
        .await
        .map_err(redacted_sdk)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "singleton response is not an object".to_owned())?;
    for (field, supplemental) in &read.merge {
        let value = match transport
            .call(OperationRequest::new(&supplemental.op))
            .await
        {
            Ok(value) => value,
            Err(SdkError::Api(detail))
                if matches!(detail.status(), 404 | 500) && supplemental.fallback.is_some() =>
            {
                supplemental.fallback.clone().unwrap_or(Json::Null)
            }
            Err(error) => return Err(redacted_sdk(error)),
        };
        object.insert(field.clone(), value);
    }

    Ok(value)
}

async fn merge_singleton_entries<T: Transport>(
    transport: &T,
    spec: &KindSpec,
    raw: &mut Json,
) -> Result<(), ImportError> {
    for info in spec.properties() {
        let Some(entry) = info.entry else {
            continue;
        };
        // Keyed entry reads cannot enumerate names. They are reported after build if the remote
        // root is absent; known keys are only available when another singleton response lists them.
        let Some(values) = lookup(raw, &info.api).and_then(Json::as_object) else {
            continue;
        };
        let keys: Vec<String> = values.keys().cloned().collect();
        for key in keys {
            let remote_key = entry
                .key_format
                .as_deref()
                .map_or_else(|| key.clone(), |format| format.replace("{key}", &key));
            let value = transport
                .call(
                    OperationRequest::new(&entry.read.op).query(entry.read.key.clone(), remote_key),
                )
                .await
                .map_err(|error| inventory_error(spec, redacted_sdk(error)))?;
            if let Some(object) = raw.as_object_mut()
                && let Some(root) = object.get_mut(&info.api).and_then(Json::as_object_mut)
            {
                root.insert(key, value);
            }
        }
    }

    Ok(())
}

async fn complete_item<T: Transport>(
    transport: &T,
    spec: &KindSpec,
    listed: &Json,
) -> Result<Option<Json>, ImportError> {
    let Some(one) = &spec.api.read.one else {
        return Ok(Some(listed.clone()));
    };
    let Some(id) = item_id(spec, listed) else {
        return Ok(None);
    };
    let query_id = match one.id_from.as_deref() {
        Some(pointer) => match lookup(listed, pointer).and_then(Json::as_str) {
            Some(value) => value,
            None => return Ok(None),
        },
        None => id,
    };
    let mut direct = match transport
        .call(OperationRequest::new(&one.op).query(one.id_param.clone(), query_id))
        .await
    {
        Ok(value) if value.is_object() => value,
        Ok(_) => return Err(inventory_error(spec, "direct response is not an object")),
        Err(SdkError::Api(detail)) if detail.status() == 404 => return Ok(None),
        Err(error) => return Err(inventory_error(spec, redacted_sdk(error))),
    };
    if item_id(spec, &direct) != Some(id) {
        return Ok(None);
    }
    // Collections sometimes retain write-only secrets that direct reads omit. Preserve only
    // missing top-level values; direct reads remain authoritative for values they returned.
    if let (Some(target), Some(source)) = (direct.as_object_mut(), listed.as_object()) {
        for (key, value) in source {
            target.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }

    Ok(Some(direct))
}

fn kind_depth(specs: &dokploy_spec::SpecRegistry, kind: &str) -> usize {
    let Some(spec) = specs.get(kind) else {
        return 0;
    };
    spec.parents
        .iter()
        .map(|parent| 1 + kind_depth(specs, parent))
        .max()
        .unwrap_or(0)
}

fn allocate(
    specs: &dokploy_spec::SpecRegistry,
    nodes: &mut [RemoteNode],
) -> Result<(), ImportError> {
    let max_depth = specs
        .kinds()
        .map(|spec| kind_depth(specs, &spec.kind))
        .max()
        .unwrap_or(0);
    for depth in 0..=max_depth {
        let indexes: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| kind_depth(specs, &node.kind) == depth)
            .map(|(index, _)| index)
            .collect();
        let mut groups: BTreeMap<AllocationGroup, Vec<usize>> = BTreeMap::new();
        for index in indexes {
            let node = &nodes[index];
            let spec = specs.get(&node.kind).expect("inventory kinds have specs");
            let base = match spec.singleton.as_deref() {
                Some(key) => key.to_owned(),
                None => remote_key_base(spec, &node.raw).ok_or_else(|| ImportError::Key {
                    kind: node.kind.clone(),
                    remote_id: node.id.as_str().to_owned(),
                })?,
            };
            groups
                .entry((node.parent.clone(), node.kind.clone(), base))
                .or_default()
                .push(index);
        }
        for ((_, _, base), mut members) in groups {
            members.sort_by_key(|index| nodes[*index].id.as_str().to_owned());
            for (ordinal, index) in members.into_iter().enumerate() {
                let key = if ordinal == 0 {
                    base.clone()
                } else {
                    format!("{base}-{}", ordinal + 1)
                };
                let kind: ResourceKind = nodes[index]
                    .kind
                    .parse()
                    .expect("registered inventory kind");
                let name = ResourceName::new(key.clone()).map_err(|_| ImportError::Key {
                    kind: nodes[index].kind.clone(),
                    remote_id: nodes[index].id.as_str().to_owned(),
                })?;
                let address = match &nodes[index].parent {
                    Some(parent) => {
                        let parent_address = nodes
                            .iter()
                            .find(|candidate| {
                                candidate.identity() == (parent.0.as_str(), parent.1.as_str())
                            })
                            .and_then(|candidate| candidate.address.clone())
                            .ok_or_else(|| {
                                inventory_error(
                                    specs.get(&nodes[index].kind).expect("known spec"),
                                    "child has no discovered parent",
                                )
                            })?;
                        parent_address
                            .child(kind, name)
                            .map_err(|_| ImportError::Key {
                                kind: nodes[index].kind.clone(),
                                remote_id: nodes[index].id.as_str().to_owned(),
                            })?
                    }
                    None => ResourceAddress::new(kind, name),
                };
                nodes[index].key = Some(key);
                nodes[index].address = Some(address);
            }
        }
    }

    Ok(())
}

type Built = (String, Document, BTreeMap<ResourceAddress, RemoteId>);

fn build_documents(
    selection: &ImportSelection,
    context: &mut BuildContext<'_>,
) -> Result<Vec<Built>, ImportError> {
    let mut built = Vec::new();
    if matches!(selection, ImportSelection::Settings | ImportSelection::All) {
        let mut sections = Sections::new();
        let settings: Vec<RemoteNode> = context
            .nodes
            .iter()
            .filter(|node| {
                context
                    .specs
                    .get(&node.kind)
                    .is_some_and(|spec| spec.scope == Scope::Settings && node.parent.is_none())
            })
            .cloned()
            .collect();
        let mut ids = BTreeMap::new();
        for node in &settings {
            let resource = build_resource(node, context, &mut ids)?;
            let spec = context.specs.get(&node.kind).expect("known kind");
            sections
                .entry(spec.section.clone())
                .or_default()
                .insert(resource.key.clone(), resource);
        }
        built.push((
            format!("{DOKPLOY_DIRECTORY}/settings.yaml"),
            Document {
                scope: Scope::Settings,
                root: Root::Settings(sections),
                moves: Vec::new(),
                adopt: Vec::new(),
                removed: Vec::new(),
            },
            ids,
        ));
    }

    if matches!(
        selection,
        ImportSelection::Project(_) | ImportSelection::All
    ) {
        let requested = match selection {
            ImportSelection::Project(id) => Some(id.as_str()),
            _ => None,
        };
        let projects: Vec<RemoteNode> = context
            .nodes
            .iter()
            .filter(|node| {
                node.kind == "project" && requested.is_none_or(|id| node.id.as_str() == id)
            })
            .cloned()
            .collect();
        if let Some(id) = requested
            && projects.is_empty()
        {
            return Err(ImportError::ProjectNotFound { id: id.to_owned() });
        }
        for project in &projects {
            let mut ids = BTreeMap::new();
            let resource = build_resource(project, context, &mut ids)?;
            let key = resource.key.clone();
            built.push((
                format!("{DOKPLOY_DIRECTORY}/projects/{key}.yaml"),
                Document {
                    scope: Scope::Project,
                    root: Root::Project(Box::new(resource)),
                    moves: Vec::new(),
                    adopt: Vec::new(),
                    removed: Vec::new(),
                },
                ids,
            ));
        }
    }

    Ok(built)
}

fn build_resource(
    node: &RemoteNode,
    context: &mut BuildContext<'_>,
    ids: &mut BTreeMap<ResourceAddress, RemoteId>,
) -> Result<Resource, ImportError> {
    let spec = context
        .specs
        .get(&node.kind)
        .expect("inventory kind has a spec");
    let key = node.key.clone().expect("keys are allocated before build");
    let address = node
        .address
        .clone()
        .expect("addresses are allocated before build");
    ids.insert(address, node.id.clone());
    let mut fields = BTreeMap::new();
    let mut env_files = BTreeMap::new();
    for (name, field) in &spec.fields {
        if field.mutability == Mutability::Computed {
            continue;
        }
        if parse_type(&field.ty).is_ok_and(|ty| matches!(ty, FieldType::Env)) {
            let (value, file) = import_environment(context, node, spec, field, name)?;
            if let Some(value) = value {
                fields.insert(
                    name.clone(),
                    DocumentField {
                        value,
                        span: Span::default(),
                    },
                );
            }
            if let Some(file) = file {
                env_files.insert(name.clone(), file);
            }
            continue;
        }
        match import_field(context, node, spec, name, field, name)? {
            Some(value) => {
                fields.insert(
                    name.clone(),
                    DocumentField {
                        value,
                        span: Span::default(),
                    },
                );
            }
            None if field.default.as_deref() == Some("key") => {
                let remote = remote_name(spec, &node.raw);
                if remote.as_deref() != Some(&key)
                    && let Some(remote) = remote
                {
                    fields.insert(
                        name.clone(),
                        DocumentField {
                            value: Value::Text(remote),
                            span: Span::default(),
                        },
                    );
                }
            }
            None => {}
        }
    }
    let children: Vec<RemoteNode> = context
        .nodes
        .iter()
        .filter(|candidate| {
            candidate.parent.as_ref() == Some(&(node.kind.clone(), node.id.as_str().to_owned()))
        })
        .cloned()
        .collect();
    let mut sections = Sections::new();
    for child in &children {
        let resource = build_resource(child, context, ids)?;
        let child_spec = context.specs.get(&child.kind).expect("known child");
        sections
            .entry(child_spec.section.clone())
            .or_default()
            .insert(resource.key.clone(), resource);
    }

    Ok(Resource {
        kind: node.kind.clone(),
        key,
        singleton: spec.singleton.is_some(),
        span: Span::default(),
        fields,
        env_files,
        lifecycle: Lifecycle {
            deploy: if spec.api.deploy.is_some() {
                DeployPolicy::OnChange
            } else {
                DeployPolicy::Never
            },
            ..Lifecycle::default()
        },
        depends_on: Vec::new(),
        children: sections,
    })
}

fn import_field(
    context: &mut BuildContext<'_>,
    node: &RemoteNode,
    spec: &KindSpec,
    name: &str,
    field: &SpecField,
    dotted: &str,
) -> Result<Option<Value>, ImportError> {
    let ty = parse_type(&field.ty).map_err(|_| ImportError::Generated {
        path: spec.kind.clone(),
        reason: format!("field `{dotted}` has an invalid type"),
    })?;
    if matches!(ty, FieldType::Env) {
        let (value, file) = import_environment(context, node, spec, field, dotted)?;
        if file.is_some() {
            return Err(ImportError::Generated {
                path: spec.kind.clone(),
                reason: format!("nested environment field `{dotted}` declares a file key"),
            });
        }
        return Ok(value);
    }
    if matches!(ty, FieldType::Struct) {
        let mut members = BTreeMap::new();
        for (member, member_field) in &field.members {
            if member_field.mutability == Mutability::Computed {
                continue;
            }
            let member_path = format!("{dotted}.{member}");
            if let Some(value) =
                import_field(context, node, spec, member, member_field, &member_path)?
            {
                members.insert(member.clone(), value);
            }
        }
        return Ok((!members.is_empty()).then_some(Value::Map(members)));
    }
    if let FieldType::Union { .. } = &ty {
        let tag_pointer = api_name(name, field);
        let Some(tag) = lookup(&node.raw, tag_pointer).and_then(Json::as_str) else {
            return Ok(None);
        };
        let Some(arm) = field.arms.get(tag) else {
            context.unsupported.push(UnsupportedItem {
                category: UnsupportedCategory::Unrepresentable,
                kind: spec.kind.clone(),
                item: dotted.to_owned(),
                reason: "remote union tag is not declared by the spec".to_owned(),
            });
            return Ok(None);
        };
        let mut fields = BTreeMap::new();
        for (member, member_field) in arm {
            if member_field.mutability == Mutability::Computed {
                continue;
            }
            let path = format!("{dotted}.{tag}.{member}");
            if let Some(value) = import_field(context, node, spec, member, member_field, &path)? {
                fields.insert(member.clone(), value);
            }
        }
        return Ok(Some(Value::Union {
            tag: tag.to_owned(),
            fields,
        }));
    }

    let info = match spec.property(dotted) {
        Ok(info) => info,
        Err(_) => return Ok(None),
    };
    let raw = lookup(&node.raw, &info.api);
    if raw.is_none() {
        if info.is_sensitive() {
            context.unsupported.push(UnsupportedItem {
                category: UnsupportedCategory::Unsupported,
                kind: spec.kind.clone(),
                item: dotted.to_owned(),
                reason: "Dokploy did not return the managed secret or content value".to_owned(),
            });
        }
        return Ok(None);
    }
    let raw = raw.expect("checked present");
    if raw.is_null() {
        if matches!(&ty, FieldType::Selector(kind) if kind == "server") {
            return Ok(Some(Value::Selector(Selector::Local)));
        }
        return Ok(info.nullable.then_some(Value::Null));
    }

    if info.class == ValueClass::Content || matches!(ty, FieldType::File) {
        let Some(text) = raw.as_str() else {
            return unrepresentable(context, spec, dotted, "content is not text");
        };
        let path = format!(
            "{CONTENT_DIRECTORY}/{}/{}-{}-{}.txt",
            document_key(context, node),
            spec.kind,
            node.key.as_deref().unwrap_or("item"),
            safe_path(dotted)
        );
        push_file(context, path.clone(), text.as_bytes(), false);
        return Ok(Some(Value::Source(Source::File(path))));
    }
    if info.is_sensitive() {
        let bytes = match raw {
            Json::String(text) => text.as_bytes().to_vec(),
            value if field.json => serde_json::to_vec(value).unwrap_or_default(),
            _ => return unrepresentable(context, spec, dotted, "secret is not text"),
        };
        let relative = format!(
            "{}/{}-{}-{}.secret",
            document_key(context, node),
            spec.kind,
            node.key.as_deref().unwrap_or("item"),
            safe_path(dotted)
        );
        let path = format!(
            "{}/{}",
            context.options.secrets_directory.trim_end_matches('/'),
            relative
        );
        push_file(context, path.clone(), &bytes, true);
        return Ok(Some(Value::Source(Source::File(path))));
    }

    match ty {
        FieldType::Selector(kind) => selector_value(context, spec, dotted, &kind, raw),
        FieldType::Ref(kind) => reference_value(context, node, spec, dotted, &kind, raw),
        FieldType::Set(ref item)
            if matches!(**item, FieldType::Selector(_))
                && field.granularity == Some(Granularity::Key) =>
        {
            let FieldType::Selector(kind) = item.as_ref() else {
                unreachable!()
            };
            let Some(ids) = held_member_ids(&node.raw, &info.api) else {
                return unrepresentable(context, spec, dotted, "selector set is not an id array");
            };
            let mut values = Vec::new();
            for id in ids {
                values.push(resolve_selector(context, spec, dotted, kind, &id)?);
            }
            values.sort_by_key(|value| match value {
                Value::Selector(Selector::Name(name)) => name.clone(),
                _ => String::new(),
            });
            Ok(Some(Value::List(values)))
        }
        FieldType::Map(ref item)
            if item.is_map_of_selector_sets()
                || matches!(item.as_ref(), FieldType::Set(_))
                    && item.selector_set_kind().is_some() =>
        {
            import_selector_map(context, node, spec, dotted, field, item)
        }
        FieldType::Map(ref item) if matches!(item.as_ref(), FieldType::File) => {
            let Some(entries) = raw.as_object() else {
                return unrepresentable(context, spec, dotted, "file map is not an object");
            };
            let mut values = BTreeMap::new();
            for (key, value) in entries {
                let Some(text) = value.as_str() else {
                    return unrepresentable(context, spec, dotted, "file map entry is not text");
                };
                let path = format!(
                    "{CONTENT_DIRECTORY}/{}/{}-{}-{}-{}.txt",
                    document_key(context, node),
                    spec.kind,
                    node.key.as_deref().unwrap_or("item"),
                    safe_path(dotted),
                    safe_path(key)
                );
                push_file(context, path.clone(), text.as_bytes(), false);
                values.insert(key.clone(), Value::Source(Source::File(path)));
            }
            Ok(Some(Value::Map(values)))
        }
        _ => match canonical_remote(raw, &ty).and_then(json_to_value) {
            Some(value) => Ok(Some(value)),
            None => unrepresentable(
                context,
                spec,
                dotted,
                "remote value does not match its spec type",
            ),
        },
    }
}

fn import_environment(
    context: &mut BuildContext<'_>,
    node: &RemoteNode,
    spec: &KindSpec,
    field: &SpecField,
    dotted: &str,
) -> Result<(Option<Value>, Option<EnvFile>), ImportError> {
    if context.options.environment == EnvironmentImportMode::Skip {
        context.unsupported.push(UnsupportedItem {
            category: UnsupportedCategory::UnmanagedByChoice,
            kind: spec.kind.clone(),
            item: dotted.to_owned(),
            reason: "environment import mode is skip".to_owned(),
        });
        return Ok((None, None));
    }
    let info = spec.property(dotted).map_err(|_| ImportError::Generated {
        path: spec.kind.clone(),
        reason: format!("environment field `{dotted}` has no property path"),
    })?;
    let Some(raw) = lookup(&node.raw, &info.api) else {
        return Ok((None, None));
    };
    if raw.is_null() {
        return Ok((field.nullable.then_some(Value::Null), None));
    }
    let Some(text) = raw.as_str() else {
        let value = unrepresentable(context, spec, dotted, "environment is not text")?;
        return Ok((value, None));
    };
    let parsed = EnvText::parse(text);
    let mut variables = BTreeMap::new();
    let mut secret_variables = Vec::new();
    for name in parsed.keys() {
        let value = parsed.value(name).unwrap_or_default();
        let secret = match context.options.environment {
            EnvironmentImportMode::Inline => false,
            EnvironmentImportMode::File => true,
            EnvironmentImportMode::Auto => likely_secret(name, value),
            EnvironmentImportMode::Skip => unreachable!(),
        };
        if secret && field.file_key.is_some() {
            secret_variables.push((name.to_owned(), Zeroizing::new(value.as_bytes().to_vec())));
            continue;
        }
        let imported = if secret {
            let relative = format!(
                "{}/{}-{}-{}-{}.secret",
                document_key(context, node),
                spec.kind,
                node.key.as_deref().unwrap_or("item"),
                safe_path(dotted),
                safe_path(name)
            );
            let path = format!(
                "{}/{}",
                context.options.secrets_directory.trim_end_matches('/'),
                relative
            );
            push_file(context, path.clone(), value.as_bytes(), true);
            EnvValue::Secret(Source::File(path))
        } else {
            EnvValue::Public(value.to_owned())
        };
        variables.insert(name.to_owned(), imported);
    }

    let file = if secret_variables.is_empty() {
        None
    } else {
        let key = field
            .file_key
            .as_ref()
            .expect("only fields with a file key collect a dotenv file")
            .clone();
        let relative = format!(
            "{}/{}-{}-{}.env",
            document_key(context, node),
            spec.kind,
            node.key.as_deref().unwrap_or("item"),
            safe_path(dotted)
        );
        let path = format!(
            "{}/{}",
            context.options.secrets_directory.trim_end_matches('/'),
            relative
        );
        let dotenv = render_dotenv(&secret_variables)?;
        push_file(context, path.clone(), &dotenv, true);
        Some(EnvFile {
            key,
            path,
            public: false,
            span: Span::default(),
        })
    };
    let value = (!variables.is_empty() || file.is_none()).then_some(Value::Env(variables));

    Ok((value, file))
}

fn selector_value(
    context: &mut BuildContext<'_>,
    spec: &KindSpec,
    dotted: &str,
    kind: &str,
    raw: &Json,
) -> Result<Option<Value>, ImportError> {
    if raw.is_null() && kind == "server" {
        return Ok(Some(Value::Selector(Selector::Local)));
    }
    let Some(id) = raw.as_str() else {
        return unrepresentable(context, spec, dotted, "selector is not a remote id");
    };

    Ok(Some(resolve_selector(context, spec, dotted, kind, id)?))
}

fn resolve_selector(
    context: &BuildContext<'_>,
    spec: &KindSpec,
    dotted: &str,
    kind: &str,
    id: &str,
) -> Result<Value, ImportError> {
    let candidate = context.nodes.iter().find(|node| {
        node.id.as_str() == id
            && (node.kind == kind
                || kind == "git_provider"
                    && matches!(
                        node.kind.as_str(),
                        "github_provider"
                            | "gitlab_provider"
                            | "bitbucket_provider"
                            | "gitea_provider"
                    ))
    });
    let Some(candidate) = candidate else {
        return Err(ImportError::UnknownReference {
            kind: spec.kind.clone(),
            field: dotted.to_owned(),
            remote_id: id.to_owned(),
        });
    };
    let target_spec = context.specs.get(&candidate.kind).expect("known target");
    let name =
        remote_name(target_spec, &candidate.raw).ok_or_else(|| ImportError::UnknownReference {
            kind: spec.kind.clone(),
            field: dotted.to_owned(),
            remote_id: id.to_owned(),
        })?;

    Ok(Value::Selector(Selector::Name(name)))
}

fn reference_value(
    context: &mut BuildContext<'_>,
    node: &RemoteNode,
    spec: &KindSpec,
    dotted: &str,
    kind: &str,
    raw: &Json,
) -> Result<Option<Value>, ImportError> {
    let Some(id) = raw.as_str() else {
        return unrepresentable(context, spec, dotted, "reference is not a remote id");
    };
    let root = node
        .address
        .as_ref()
        .and_then(|address| address.segments().first());
    let candidate = context.nodes.iter().find(|candidate| {
        candidate.kind == kind
            && candidate.id.as_str() == id
            && candidate
                .address
                .as_ref()
                .and_then(|address| address.segments().first())
                == root
    });
    let Some(key) = candidate.and_then(|candidate| candidate.key.clone()) else {
        return Err(ImportError::UnknownReference {
            kind: spec.kind.clone(),
            field: dotted.to_owned(),
            remote_id: id.to_owned(),
        });
    };

    Ok(Some(Value::Text(key)))
}

fn import_selector_map(
    context: &mut BuildContext<'_>,
    node: &RemoteNode,
    spec: &KindSpec,
    dotted: &str,
    field: &SpecField,
    item: &FieldType,
) -> Result<Option<Value>, ImportError> {
    let info = spec.property(dotted).map_err(|_| ImportError::Generated {
        path: spec.kind.clone(),
        reason: format!("selector map `{dotted}` has no property path"),
    })?;
    let Some((base, rest)) = info.api.split_once("/*[") else {
        return unrepresentable(
            context,
            spec,
            dotted,
            "selector map has no keyed array mapping",
        );
    };
    let Some((condition, member)) = rest.split_once("]/") else {
        return unrepresentable(context, spec, dotted, "selector map mapping is malformed");
    };
    let Some((key_name, _)) = condition.split_once('=') else {
        return unrepresentable(
            context,
            spec,
            dotted,
            "selector map key mapping is malformed",
        );
    };
    let Some(elements) = lookup(&node.raw, base).and_then(Json::as_array) else {
        return Ok(None);
    };
    let target = item.selector_set_kind().unwrap_or_default();
    let mut map = BTreeMap::new();
    for element in elements {
        let Some(remote_key) = element.get(key_name).and_then(Json::as_str) else {
            return unrepresentable(
                context,
                spec,
                dotted,
                "selector map key is not an id or text",
            );
        };
        let key = if let Some(kind) = field.key_selector.as_deref() {
            match resolve_selector(context, spec, dotted, kind, remote_key)? {
                Value::Selector(Selector::Name(name)) => name,
                _ => remote_key.to_owned(),
            }
        } else {
            remote_key.to_owned()
        };
        let ids = element
            .get(member)
            .and_then(Json::as_array)
            .ok_or_else(|| ImportError::Generated {
                path: spec.kind.clone(),
                reason: format!("selector map `{dotted}` has a non-array member"),
            })?;
        let mut values = Vec::new();
        for id in ids {
            let Some(id) = id.as_str() else {
                return unrepresentable(context, spec, dotted, "selector map member is not an id");
            };
            values.push(resolve_selector(context, spec, dotted, target, id)?);
        }
        map.insert(key, Value::List(values));
    }

    Ok(Some(Value::Map(map)))
}

fn remote_name(spec: &KindSpec, raw: &Json) -> Option<String> {
    let key = spec.identity.key.as_deref()?;
    let field = spec.fields.get(key)?;
    canonical_remote(
        lookup(raw, api_name(key, field))?,
        &parse_type(&field.ty).ok()?,
    )?
    .as_str()
    .map(str::to_owned)
}

fn remote_key_base(spec: &KindSpec, raw: &Json) -> Option<String> {
    let names: Vec<&str> = spec.identity.key.as_deref().map_or_else(
        || spec.identity.collision.iter().map(String::as_str).collect(),
        |key| vec![key],
    );
    if names.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for name in names {
        let field = spec.fields.get(name)?;
        let ty = parse_type(&field.ty).ok()?;
        let value = canonical_remote(lookup(raw, api_name(name, field))?, &ty)?;
        let part = match value {
            Json::Bool(value) => value.to_string(),
            Json::Number(value) => value.to_string(),
            Json::String(value) => value,
            _ => return None,
        };
        parts.push(part);
    }
    let source = parts.join("-");
    let mut key = slug(&source);
    let digest = Sha256::digest(source.as_bytes());
    let suffix: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if key.is_empty() {
        key = format!("{}-{suffix}", spec.kind);
    } else if !key.starts_with(|character: char| character.is_ascii_lowercase()) {
        key = format!("{}-{key}", spec.kind);
    }
    if key.len() > 112 {
        key.truncate(112);
        key.push('-');
        key.push_str(&suffix);
    }

    Some(key)
}

fn json_to_value(value: Json) -> Option<Value> {
    match value {
        Json::Null => Some(Value::Null),
        Json::Bool(value) => Some(Value::Bool(value)),
        Json::Number(value) if value.is_i64() => Some(Value::Int(value.as_i64()?)),
        Json::Number(value) if value.is_u64() => {
            i64::try_from(value.as_u64()?).ok().map(Value::Int)
        }
        Json::Number(value) => value.as_f64().map(Value::Number),
        Json::String(value) => Some(Value::Text(value)),
        Json::Array(values) => values
            .into_iter()
            .map(json_to_value)
            .collect::<Option<Vec<_>>>()
            .map(Value::List),
        Json::Object(values) => values
            .into_iter()
            .map(|(key, value)| Some((key, json_to_value(value)?)))
            .collect::<Option<BTreeMap<_, _>>>()
            .map(Value::Map),
    }
}

fn unrepresentable<T>(
    context: &mut BuildContext<'_>,
    spec: &KindSpec,
    dotted: &str,
    reason: &str,
) -> Result<Option<T>, ImportError> {
    context.unsupported.push(UnsupportedItem {
        category: UnsupportedCategory::Unrepresentable,
        kind: spec.kind.clone(),
        item: dotted.to_owned(),
        reason: reason.to_owned(),
    });
    Ok(None)
}

fn push_file(context: &mut BuildContext<'_>, path: String, bytes: &[u8], secret: bool) {
    if !context.files.iter().any(|file| file.path == path) {
        context.files.push(ImportedFile {
            path,
            bytes: Zeroizing::new(bytes.to_vec()),
            secret,
        });
    }
}

struct ArtifactSecrets {
    files: BTreeMap<String, Zeroizing<Vec<u8>>>,
}

impl ArtifactSecrets {
    fn new(files: &[ImportedFile]) -> Self {
        Self {
            files: files
                .iter()
                .map(|file| (file.path.clone(), Zeroizing::new(file.bytes.to_vec())))
                .collect(),
        }
    }
}

impl SecretReader for ArtifactSecrets {
    fn read(&self, source: &Source) -> Result<Zeroizing<Vec<u8>>, SecretError> {
        match source {
            Source::File(path) => self
                .files
                .get(path)
                .map(|bytes| Zeroizing::new(bytes.to_vec()))
                .ok_or_else(|| SecretError::UnreadableFile { path: path.clone() }),
            Source::Env(name) => Err(SecretError::MissingEnvironment { name: name.clone() }),
            Source::Vault { provider, secret } => Ok(Zeroizing::new(
                format!("${{{{vault.{provider}.{secret}}}}}").into_bytes(),
            )),
        }
    }
}

fn topology_fingerprint(nodes: &[RemoteNode]) -> TopologyFingerprint {
    let mut fingerprint: Vec<_> = nodes
        .iter()
        .map(|node| {
            (
                node.kind.clone(),
                node.id.as_str().to_owned(),
                node.parent.clone(),
            )
        })
        .collect();
    fingerprint.sort();
    fingerprint
}

fn document_key(context: &BuildContext<'_>, node: &RemoteNode) -> String {
    if context
        .specs
        .get(&node.kind)
        .is_some_and(|spec| spec.scope == Scope::Settings)
    {
        return "settings".to_owned();
    }

    node.address
        .as_ref()
        .and_then(|address| address.segments().first())
        .map_or_else(
            || "settings".to_owned(),
            |segment| segment.name().as_str().to_owned(),
        )
}

fn ignored_remote_fields(
    specs: &dokploy_spec::SpecRegistry,
    nodes: &[RemoteNode],
) -> Vec<UnsupportedItem> {
    let mut items = Vec::new();
    for node in nodes {
        let Some(spec) = specs.get(&node.kind) else {
            continue;
        };
        for ignored in &spec.ledger.ignored {
            let Some(value) = lookup(&node.raw, &ignored.field) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            let address = node
                .address
                .as_ref()
                .map_or_else(|| node.id.as_str().to_owned(), ToString::to_string);
            items.push(UnsupportedItem {
                category: UnsupportedCategory::Unsupported,
                kind: node.kind.clone(),
                item: format!("{address}:{}", ignored.field),
                reason: ignored.reason.clone(),
            });
        }
    }

    items
}

fn slug(value: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for character in value.trim().chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() || character == '_' {
            if separator && !result.is_empty() {
                result.push('-');
            }
            result.push(character);
            separator = false;
        } else {
            separator = true;
        }
    }
    while result.ends_with('-') {
        result.pop();
    }
    if result.starts_with(|character: char| !character.is_ascii_lowercase()) {
        result.insert_str(0, "item-");
    }
    result.truncate(256);
    result
}

fn safe_path(value: &str) -> String {
    let slugged = slug(value);
    if slugged.is_empty() {
        "value".to_owned()
    } else {
        slugged
    }
}

fn likely_secret(name: &str, value: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    let sensitive_name = [
        "secret",
        "token",
        "key",
        "pass",
        "pwd",
        "credential",
        "dsn",
        "private",
        "auth",
        "salt",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));
    let embedded_credentials = value
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('@'))
        .is_some_and(|(credentials, _)| credentials.contains(':'));
    let high_entropy = value.len() >= 24
        && value
            .chars()
            .any(|character| character.is_ascii_lowercase())
        && value
            .chars()
            .any(|character| character.is_ascii_uppercase())
        && value.chars().any(|character| character.is_ascii_digit());
    sensitive_name || embedded_credentials || high_entropy
}

fn inventory_error(spec: &KindSpec, reason: impl ToString) -> ImportError {
    ImportError::Inventory {
        kind: spec.kind.clone(),
        reason: reason.to_string(),
    }
}

fn redacted_sdk(error: SdkError) -> String {
    match error {
        SdkError::Api(detail) => format!("Dokploy returned HTTP {}", detail.status()),
        _ => "transport request failed".to_owned(),
    }
}

fn safe_relative_directory(value: &str) -> bool {
    if value.is_empty() || value.contains(['\\', '\0']) || Path::new(value).is_absolute() {
        return false;
    }

    let mut components = Path::new(value).components();
    matches!(
        components.next(),
        Some(std::path::Component::Normal(directory)) if directory == DOKPLOY_DIRECTORY
    ) && components.next().is_some()
        && components.all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn create_file(path: &Path, bytes: &[u8]) -> Result<(), ImportPersistError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ImportPersistError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| ImportPersistError::Io {
            path: path.display().to_string(),
            source,
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| ImportPersistError::Io {
            path: path.display().to_string(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use dokploy_state::RemoteId;

    use super::{RemoteNode, ignored_remote_fields, remote_key_base};

    #[test]
    fn keyless_kinds_derive_valid_keys_from_every_collision_field() {
        let specs = dokploy_spec::embedded().expect("repository specs load");
        let domain = specs.get("domain").expect("domain spec");
        let port = specs.get("port").expect("port spec");
        let redirect = specs.get("redirect").expect("redirect spec");

        assert_eq!(
            remote_key_base(domain, &serde_json::json!({ "host": "api.example.test" })),
            Some("api-example-test".to_owned())
        );
        assert_eq!(
            remote_key_base(
                port,
                &serde_json::json!({ "publishedPort": 443, "protocol": "tcp" })
            ),
            Some("item-443-tcp".to_owned())
        );
        let punctuation = remote_key_base(redirect, &serde_json::json!({ "regex": ".*" }))
            .expect("punctuation-only collision values get a stable fallback");
        assert!(punctuation.starts_with("redirect-"), "{punctuation}");
    }

    #[test]
    fn unsupported_report_excludes_null_and_structural_metadata() {
        let specs = dokploy_spec::embedded().expect("repository specs load");
        let nodes = [RemoteNode {
            kind: "domain".to_owned(),
            id: RemoteId::new("domain-1").unwrap(),
            parent: None,
            raw: serde_json::json!({ "previewDeploymentId": null }),
            key: None,
            address: None,
        }];

        assert!(ignored_remote_fields(&specs, &nodes).is_empty());

        let mount = specs.get("mount").expect("mount spec");
        assert!(mount.ledger.ignored.is_empty());
        assert!(mount.ledger.derived.contains(&"postgresId".to_owned()));
    }
}
