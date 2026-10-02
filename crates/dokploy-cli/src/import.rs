//! Read-only adoption of one existing Dokploy project into a new workspace.
//!
//! Import always selects a **project** and always adopts everything below it in
//! one pass. The pipeline has five stages and only the first and last do I/O:
//!
//! 1. [`inventory`]: crawl the project into a plain remote tree;
//! 2. [`project::validate`]: prove the tree is consistent and resolve external names;
//! 3. [`names`]: allocate deterministic logical names;
//! 4. [`project::build`]: append every resource to an [`ImportContext`], then
//!    prove offline that the first plan will be empty ([`converge`]);
//! 5. re-read the project topology and refuse if it changed during the crawl.
//!
//! Nothing is written unless every stage succeeds. Import only creates a new
//! workspace; there is no merge or refresh mode.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use dokploy_config::{
    ApplicationDocument, ComposeDocument, ConfigDocument, ConfigWriteError, DomainDocument,
    EnvironmentDocument, ExternalSelector, Field, LibSqlDocument, LibSqlNodeConfig,
    LifecycleDocument, MariaDbDocument, MongoDocument, MySqlDocument, NonEmptyText, PortDocument,
    PortNumber, PortProtocolConfig, PortPublishModeConfig, PostgresDocument, RedirectDocument,
    RedisDocument, SecurityDocument, SelectorKind, SourceDocument,
};
use dokploy_sdk::{
    ApplicationDetails, Dokploy, EnvironmentDetails, Error as SdkError, PortDetails, PortProtocol,
    ProjectDetails, ProjectId, PublishMode, RedirectDetails, ResponseField, SecurityDetails,
};
use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceName, ResourceState, StateFile, StateStore,
};
use thiserror::Error;

use crate::external::ExternalDirectory;

mod backup;
mod context;
mod converge;
mod inventory;
mod mount;
mod names;
mod project;
mod schedule;

/// A complete project import selection.
pub struct ImportRequest {
    pub project_id: String,
    pub config_file: PathBuf,
}

/// What an import adopted, for the operator.
#[derive(Debug)]
pub struct ImportReport {
    resources: usize,
    project: ResourceAddress,
    environments: Vec<EnvironmentReport>,
    renamed: Vec<RenamedReport>,
    unmanaged: Vec<&'static str>,
}

#[derive(Debug)]
struct EnvironmentReport {
    address: ResourceAddress,
    counts: BTreeMap<ResourceKind, usize>,
}

#[derive(Debug)]
struct RenamedReport {
    address: ResourceAddress,
    remote_name: String,
}

impl ImportReport {
    /// The number of resources recorded in the new state.
    #[must_use]
    pub const fn resource_count(&self) -> usize {
        self.resources
    }
}

/// The per-environment table, renamed addresses, and deliberately unmanaged fields.
impl fmt::Display for ImportReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "Project {}", self.project)?;
        for environment in &self.environments {
            let counts = environment
                .counts
                .iter()
                .filter(|(kind, _)| **kind != ResourceKind::Environment)
                .map(|(kind, count)| format!("{count} {}", kind_label(*kind)))
                .collect::<Vec<_>>();
            if counts.is_empty() {
                writeln!(formatter, "  {}: empty", environment.address)?;
            } else {
                writeln!(
                    formatter,
                    "  {}: {}",
                    environment.address,
                    counts.join(", ")
                )?;
            }
        }
        if !self.renamed.is_empty() {
            writeln!(
                formatter,
                "Renamed to keep addresses unique (rename later with `moves:`):"
            )?;
            for renamed in &self.renamed {
                writeln!(
                    formatter,
                    "  {} (remote: {})",
                    renamed.address, renamed.remote_name
                )?;
            }
        }
        if !self.unmanaged.is_empty() {
            writeln!(
                formatter,
                "Left unmanaged, never read into configuration or state:"
            )?;
            for field in &self.unmanaged {
                writeln!(formatter, "  {field}")?;
            }
        }

        Ok(())
    }
}

const fn kind_label(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Project => "project",
        ResourceKind::Environment => "environment",
        ResourceKind::Application => "application",
        ResourceKind::Compose => "compose",
        ResourceKind::Postgres => "postgres",
        ResourceKind::MySql => "mysql",
        ResourceKind::MariaDb => "mariadb",
        ResourceKind::Mongo => "mongo",
        ResourceKind::LibSql => "libsql",
        ResourceKind::Redis => "redis",
        ResourceKind::Domain => "domain",
        ResourceKind::Port => "port",
        ResourceKind::Redirect => "redirect",
        ResourceKind::Security => "security",
        ResourceKind::Mount => "mount",
        ResourceKind::Schedule => "schedule",
        ResourceKind::Backup => "backup",
    }
}

/// Fields the importer never reads, by the kinds the project actually contains.
fn unmanaged_fields(kinds: &std::collections::BTreeSet<ResourceKind>) -> Vec<&'static str> {
    let mut fields = Vec::new();
    let has = |kind| kinds.contains(&kind);
    if has(ResourceKind::Application) {
        fields.push("application environment variables and non-GitHub sources");
    }
    if has(ResourceKind::Compose) {
        fields.push("compose documents");
    }
    if [
        ResourceKind::Postgres,
        ResourceKind::MySql,
        ResourceKind::MariaDb,
        ResourceKind::Mongo,
        ResourceKind::LibSql,
        ResourceKind::Redis,
    ]
    .into_iter()
    .any(has)
    {
        fields.push("database passwords");
    }
    if has(ResourceKind::Security) {
        fields.push("security passwords");
    }
    if has(ResourceKind::Mount) {
        fields.push("mount file contents");
    }
    if has(ResourceKind::Schedule) {
        fields.push("schedule commands and scripts");
    }
    fields
}

/// Injectable terminal selection seam used by the interactive import workflow.
#[doc(hidden)]
pub trait ImportPrompter {
    fn select(&mut self, choices: &[String]) -> Result<usize, ImportError>;
}

struct InquirePrompter;

impl ImportPrompter for InquirePrompter {
    fn select(&mut self, choices: &[String]) -> Result<usize, ImportError> {
        let selected = inquire::Select::new("Select a Dokploy project", choices.to_vec())
            .prompt()
            .map_err(|_| ImportError::Prompt)?;
        choices
            .iter()
            .position(|choice| choice == &selected)
            .ok_or(ImportError::InvalidSelection)
    }
}

/// Lists the visible projects and asks a terminal user which one to import.
pub async fn select_interactively(
    client: &Dokploy,
    config_file: PathBuf,
) -> Result<ImportRequest, ImportError> {
    select_with_prompter(client, config_file, &mut InquirePrompter).await
}

/// Runs project selection through an injected deterministic prompt adapter.
#[doc(hidden)]
pub async fn select_with_prompter(
    client: &Dokploy,
    config_file: PathBuf,
    prompter: &mut dyn ImportPrompter,
) -> Result<ImportRequest, ImportError> {
    let topology = client.projects().all().await?;
    let mut projects = topology.projects().iter().collect::<Vec<_>>();
    projects.sort_by(|left, right| {
        (&left.name, left.project_id.as_str()).cmp(&(&right.name, right.project_id.as_str()))
    });
    if projects.is_empty() {
        return Err(ImportError::NoVisibleResources);
    }
    // The remote identity disambiguates projects that share a name.
    let labels = projects
        .iter()
        .map(|project| format!("{} ({})", project.name, project.project_id.as_str()))
        .collect::<Vec<_>>();
    let index = prompter.select(&labels)?;
    let selected = projects.get(index).ok_or(ImportError::InvalidSelection)?;

    Ok(ImportRequest {
        project_id: selected.project_id.as_str().to_owned(),
        config_file,
    })
}

/// Imports one whole project without any remote mutation.
pub async fn import_project(
    client: &Dokploy,
    request: ImportRequest,
) -> Result<ImportReport, ImportError> {
    let workspace = canonical_workspace(&request.config_file)?;
    if request.config_file.exists() {
        return Err(ImportError::ConfigExists);
    }
    let instance = InstanceIdentity::parse(client.base_url().as_str())?;
    let store = StateStore::new(&workspace, instance.clone())?;
    if store.inspect()?.is_some() {
        return Err(ImportError::StateExists);
    }
    let project_id = ProjectId::new(request.project_id.as_str());

    // Stage 1: the only crawl.
    let remote = inventory::crawl(client, &project_id).await?;
    // Stages 2 to 4 are pure; any failure here writes nothing.
    let resolved = project::validate(&remote)?;
    let names = names::allocate(&remote, &resolved)?;
    let built = project::build(&remote, &names, &resolved)?;
    let rendered = built.context.document.render()?;
    let resources = built
        .context
        .resources
        .into_iter()
        .map(|resource| (resource.address, resource.state))
        .collect::<BTreeMap<_, _>>();
    let state = StateFile::new_with_resources(
        env!("CARGO_PKG_VERSION")
            .parse()
            .expect("crate version is valid semver"),
        instance,
        resources,
    )?;
    converge::verify(&rendered, &state)?;

    // Stage 5: the project must be exactly what the crawl saw.
    let after = inventory::read_topology(client, &project_id).await?;
    if after.fingerprint() != remote.topology {
        return Err(ImportError::RemoteChanged);
    }

    let mut session = store.begin_write()?;
    if store.inspect()?.is_some() || request.config_file.exists() {
        return Err(ImportError::WorkspaceChanged);
    }
    persist_import(&built.context.document, &request.config_file, || {
        session.checkpoint(ExpectedState::absent(), &state)
    })?;

    let kinds = state
        .resources()
        .values()
        .map(ResourceState::kind)
        .collect::<std::collections::BTreeSet<_>>();
    Ok(ImportReport {
        resources: state.resources().len(),
        project: names.project.clone(),
        environments: built
            .census
            .into_iter()
            .map(|census| EnvironmentReport {
                address: census.address,
                counts: census.counts,
            })
            .collect(),
        renamed: names
            .renamed()
            .iter()
            .map(|renamed| RenamedReport {
                address: renamed.address.clone(),
                remote_name: renamed.remote_name.clone(),
            })
            .collect(),
        unmanaged: unmanaged_fields(&kinds),
    })
}

fn persist_import(
    document: &ConfigDocument,
    config_file: &Path,
    checkpoint: impl FnOnce() -> Result<(), dokploy_state::StateStoreError>,
) -> Result<(), ImportError> {
    document.write(config_file)?;
    if let Err(error) = checkpoint() {
        std::fs::remove_file(config_file).map_err(|source| ImportError::Rollback { source })?;
        return Err(ImportError::State(error));
    }

    Ok(())
}

struct ImportedResource {
    address: ResourceAddress,
    state: ResourceState,
}

fn validate_compose_import_authority(
    direct: &dokploy_sdk::ComposeDetails,
    collection: &[dokploy_sdk::ComposeSearchItem],
) -> Result<(), ImportError> {
    let matching = collection
        .iter()
        .filter(|candidate| candidate.compose_id == direct.compose_id)
        .collect::<Vec<_>>();
    let [candidate] = matching.as_slice() else {
        return Err(ImportError::InvalidRemoteTopology);
    };
    if candidate.environment_id != direct.environment_id
        || candidate.name != direct.name
        || !response_fields_agree(
            &candidate.app_name,
            &ResponseField::Value(direct.app_name.clone()),
        )
        || !response_fields_agree(&candidate.description, &direct.description)
        || !response_fields_agree(&candidate.source_type, &direct.source_type)
    {
        return Err(ImportError::InvalidRemoteTopology);
    }

    Ok(())
}

fn response_fields_agree<T: PartialEq>(left: &ResponseField<T>, right: &ResponseField<T>) -> bool {
    matches!(
        (left, right),
        (ResponseField::NotReturned, _) | (_, ResponseField::NotReturned)
    ) || left == right
}
fn imported_libsql_node(
    database: &dokploy_sdk::LibSqlDetails,
) -> Result<LibSqlNodeConfig, ImportError> {
    match (&database.sqld_node, &database.sqld_primary_url) {
        (ResponseField::Value(node), ResponseField::NotReturned | ResponseField::Null)
            if node == "primary" =>
        {
            Ok(LibSqlNodeConfig::Primary)
        }
        (ResponseField::Value(node), ResponseField::Value(primary_url))
            if node == "replica" && !primary_url.is_empty() =>
        {
            Ok(LibSqlNodeConfig::Replica {
                primary_url: primary_url.clone(),
            })
        }
        _ => Err(ImportError::InvalidLibSqlNode),
    }
}

/// Exact names of the external records an imported application is attached to.
///
/// Physical server and registry identities never enter the imported document or
/// state; each existing association is written as a stable name selector.
#[derive(Default)]
struct ImportedAssociations {
    server: Option<String>,
    build_server: Option<String>,
    registry: Option<String>,
    build_registry: Option<String>,
    rollback_registry: Option<String>,
}

/// Resolves an application's external associations from the crawl's directory.
///
/// An identity that is unknown, unreadable, or whose name is shared by another
/// record cannot be written back as an unambiguous selector, so the import fails
/// closed instead of guessing.
fn associations_from(
    directory: &ExternalDirectory,
    application: &ApplicationDetails,
) -> Result<ImportedAssociations, ImportError> {
    fn id<T>(field: &ResponseField<T>, as_str: fn(&T) -> &str) -> Option<String> {
        match field {
            ResponseField::Value(value) => Some(as_str(value).to_owned()),
            ResponseField::NotReturned | ResponseField::Null => None,
        }
    }
    let name = |kind, id: Option<String>| -> Result<Option<String>, ImportError> {
        id.map(|id| {
            directory
                .unique_name_of(kind, &id)
                .map(str::to_owned)
                .ok_or(ImportError::ExternalAssociation)
        })
        .transpose()
    };

    Ok(ImportedAssociations {
        server: name(
            SelectorKind::Server,
            id(&application.server_id, dokploy_sdk::ServerId::as_str),
        )?,
        build_server: name(
            SelectorKind::Server,
            id(&application.build_server_id, dokploy_sdk::ServerId::as_str),
        )?,
        registry: name(
            SelectorKind::Registry,
            id(&application.registry_id, dokploy_sdk::RegistryId::as_str),
        )?,
        build_registry: name(
            SelectorKind::Registry,
            id(
                &application.build_registry_id,
                dokploy_sdk::RegistryId::as_str,
            ),
        )?,
        rollback_registry: name(
            SelectorKind::Registry,
            id(
                &application.rollback_registry_id,
                dokploy_sdk::RegistryId::as_str,
            ),
        )?,
    })
}

/// Resolves a service's server placement from the crawl's directory.
///
/// A local (`null`) or omitted placement stays unmanaged. An identity that is
/// unknown, unreadable, or whose name is shared by another record fails closed.
fn server_from(
    directory: &ExternalDirectory,
    field: &ResponseField<dokploy_sdk::ServerId>,
) -> Result<Option<String>, ImportError> {
    let ResponseField::Value(server_id) = field else {
        return Ok(None);
    };
    directory
        .unique_name_of(SelectorKind::Server, server_id.as_str())
        .map(|name| Some(name.to_owned()))
        .ok_or(ImportError::ExternalAssociation)
}

fn imported_server_selector(server: Option<&str>) -> Field<ExternalSelector> {
    server.map_or(Field::Unmanaged, |name| {
        Field::Set(ExternalSelector::named(name))
    })
}

/// Records the imported server as a stable name selector in durable inputs.
fn service_inputs(
    mut inputs: serde_json::Map<String, serde_json::Value>,
    server: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    if let Some(name) = server {
        inputs.insert("server".to_owned(), serde_json::json!({ "name": name }));
    }
    inputs
}

fn application_config(
    application: &ApplicationDetails,
    associations: &ImportedAssociations,
) -> (
    ApplicationDocument,
    serde_json::Map<String, serde_json::Value>,
) {
    let mut config = ApplicationDocument::default();
    let mut inputs = serde_json::Map::new();
    for (name, selected, field) in [
        ("server", &associations.server, &mut config.server),
        (
            "build_server",
            &associations.build_server,
            &mut config.build_server,
        ),
        ("registry", &associations.registry, &mut config.registry),
        (
            "build_registry",
            &associations.build_registry,
            &mut config.build_registry,
        ),
        (
            "rollback_registry",
            &associations.rollback_registry,
            &mut config.rollback_registry,
        ),
    ] {
        if let Some(selected) = selected {
            *field = Field::Set(ExternalSelector::named(selected));
            inputs.insert(name.to_owned(), serde_json::json!({ "name": selected }));
        }
    }
    config.description = response_field(&application.description);
    config.replicas = response_field(&application.replicas);
    insert_response(&mut inputs, "description", &application.description);
    insert_response(&mut inputs, "replicas", &application.replicas);
    if matches!(&application.source_type, ResponseField::Value(value) if value == "github")
        && let ResponseField::Value(repository) = &application.repository
    {
        config.source = Field::Set(SourceDocument::GitHub {
            repository: repository.clone(),
            branch: response_field(&application.branch),
        });
        let mut source = serde_json::Map::new();
        source.insert("repository".to_owned(), serde_json::json!(repository));
        insert_response(&mut source, "branch", &application.branch);
        inputs.insert("source".to_owned(), serde_json::Value::Object(source));
    }
    (config, inputs)
}

fn response_field<T: Clone>(field: &ResponseField<T>) -> Field<T> {
    match field {
        ResponseField::NotReturned => Field::Unmanaged,
        ResponseField::Null => Field::Clear,
        ResponseField::Value(value) => Field::Set(value.clone()),
    }
}

fn description_inputs(field: &ResponseField<String>) -> serde_json::Map<String, serde_json::Value> {
    let mut inputs = serde_json::Map::new();
    insert_response(&mut inputs, "description", field);
    inputs
}

fn insert_response<T: Clone + serde::Serialize>(
    inputs: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    field: &ResponseField<T>,
) {
    match field {
        ResponseField::NotReturned => {}
        ResponseField::Null => {
            inputs.insert(name.to_owned(), serde_json::Value::Null);
        }
        ResponseField::Value(value) => {
            inputs.insert(
                name.to_owned(),
                serde_json::to_value(value).expect("SDK response values are serializable"),
            );
        }
    }
}

fn resource_state(
    address: &ResourceAddress,
    remote_id: &str,
    protected: bool,
    inputs: serde_json::Map<String, serde_json::Value>,
    containment: Option<ResourceAddress>,
) -> Result<ResourceState, ImportError> {
    resource_state_with_dependencies(
        address,
        remote_id,
        protected,
        inputs,
        containment,
        Vec::new(),
    )
}

fn resource_state_with_dependencies(
    address: &ResourceAddress,
    remote_id: &str,
    protected: bool,
    inputs: serde_json::Map<String, serde_json::Value>,
    containment: Option<ResourceAddress>,
    dependencies: Vec<ResourceAddress>,
) -> Result<ResourceState, ImportError> {
    Ok(ResourceState::new(
        address.kind(),
        RemoteId::new(remote_id)?,
        protected,
        ManagedInputs::try_from_json(serde_json::Value::Object(inputs))?,
        containment,
        dependencies,
    ))
}

fn logical_name(value: &str) -> ResourceName {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.is_empty() {
        slug.push_str("imported");
    }
    if !slug.starts_with(|character: char| character.is_ascii_lowercase()) {
        slug.insert_str(0, "imported-");
    }

    ResourceName::new(slug).expect("generated slugs satisfy the logical name grammar")
}

const fn port_publish_mode_label(mode: PublishMode) -> &'static str {
    match mode {
        PublishMode::Ingress => "ingress",
        PublishMode::Host => "host",
    }
}

const fn port_protocol_label(protocol: PortProtocol) -> &'static str {
    match protocol {
        PortProtocol::Tcp => "tcp",
        PortProtocol::Udp => "udp",
    }
}

fn canonical_workspace(config_file: &Path) -> Result<PathBuf, ImportError> {
    let parent = config_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::canonicalize(parent).map_err(|source| ImportError::Workspace { source })
}

/// A redaction-safe import failure.
#[derive(Debug, Error)]
pub enum ImportError {
    #[error("dokploy.yaml already exists")]
    ConfigExists,
    #[error("durable state already exists")]
    StateExists,
    #[error("the workspace changed during import")]
    WorkspaceChanged,
    #[error("the project changed while it was being read; run the import again")]
    RemoteChanged,
    #[error("the project is larger than an import can safely read")]
    TooLarge,
    #[error("the selected resource containment is unavailable")]
    MissingContainment,
    #[error("the selected LibSQL node topology is invalid")]
    InvalidLibSqlNode,
    #[error("the remote resource topology is invalid")]
    InvalidRemoteTopology,
    #[error(
        "two {kind} resources in one project share a name, which reconciliation cannot tell apart; rename one in Dokploy first"
    )]
    DuplicateName { kind: &'static str },
    #[error(
        "a Compose has Schedules on more than one of its services, which the configuration model cannot describe yet (DOKCFG055)"
    )]
    ComposeSchedulesSpanServices,
    #[error(
        "an external server, registry, or destination association is unknown, unreadable, or has a name shared by another record"
    )]
    ExternalAssociation,
    #[error("the imported workspace would not plan clean on its first plan ({subject})")]
    NotConvergent { subject: String },
    #[error("no importable projects are visible")]
    NoVisibleResources,
    #[error("the interactive import selection is invalid")]
    InvalidSelection,
    #[error("failed to resolve the import workspace")]
    Workspace { source: std::io::Error },
    #[error("failed to remove the generated configuration after state persistence failed")]
    Rollback { source: std::io::Error },
    #[error("failed to read the selected Dokploy resource")]
    Remote(#[from] SdkError),
    #[error("the selected resource has an invalid logical name")]
    Name(#[from] dokploy_state::ResourceNameError),
    #[error("the selected resource has an invalid remote identity")]
    RemoteId(#[from] dokploy_state::RemoteIdError),
    #[error("the generated import document is invalid")]
    Config(#[from] ConfigWriteError),
    #[error("the generated import document contains a duplicate resource")]
    ConfigDocument(#[from] dokploy_config::ConfigDocumentError),
    #[error("the imported state contains unsafe managed inputs")]
    ManagedInputs(#[from] dokploy_state::ManagedInputsError),
    #[error("the imported state is invalid")]
    StateDomain(#[from] dokploy_state::StateError),
    #[error("failed to access durable import state")]
    State(#[from] dokploy_state::StateStoreError),
    #[error("failed to parse the Dokploy instance identity")]
    Instance(#[from] dokploy_state::InstanceIdentityError),
    #[error("interactive import prompting failed")]
    Prompt,
}

#[cfg(test)]
mod tests {
    use dokploy_state::{ResourceName, StateStoreError};

    use super::{ConfigDocument, ImportError, persist_import};

    #[test]
    fn removes_config_when_the_single_populated_checkpoint_fails() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let config_file = workspace.path().join("dokploy.yaml");
        let document = ConfigDocument::new(ResourceName::new("adopted").expect("valid name"));

        let error = persist_import(&document, &config_file, || {
            Err(StateStoreError::LockContended)
        })
        .expect_err("checkpoint must fail");

        assert!(matches!(
            error,
            ImportError::State(StateStoreError::LockContended)
        ));
        assert!(!config_file.exists());
        assert!(!workspace.path().join(".dokploy/state.json").exists());
    }
}
