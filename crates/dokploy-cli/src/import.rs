//! Read-only adoption of existing Dokploy resources into one new workspace.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use dokploy_config::{
    ApplicationDocument, ComposeDocument, ConfigDocument, ConfigWriteError, DomainDocument,
    EnvironmentDocument, ExternalSelector, Field, LibSqlDocument, LibSqlNodeConfig,
    LifecycleDocument, MariaDbDocument, MongoDocument, MySqlDocument, NonEmptyText, PortDocument,
    PortNumber, PortProtocolConfig, PortPublishModeConfig, PostgresDocument, RedirectDocument,
    RedisDocument, SecurityDocument, SelectorKind, SourceDocument,
};
use dokploy_sdk::{
    ApplicationDetails, ApplicationId, ComposeId, Dokploy, DomainId, EnvironmentDetails,
    EnvironmentId, Error as SdkError, LibSqlId, MariaDbId, MongoId, MySqlId, PortDetails, PortId,
    PortProtocol, PostgresId, ProjectDetails, ProjectId, PublishMode, RedirectDetails, RedirectId,
    RedisId, ResponseField, SecurityDetails, SecurityId,
};
use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceName, ResourceState, StateFile, StateStore,
};
use thiserror::Error;

use crate::cli::ImportKind;
use crate::external::ExternalDirectory;

use self::context::{EnvScope, ImportContext};

mod backup;
mod context;
mod mount;
mod schedule;

const INTERACTIVE_LIBSQL_ITEM_LIMIT: usize = 10_000;

/// A complete noninteractive import selection.
pub struct ImportRequest {
    pub kind: ImportKind,
    pub remote_id: String,
    pub address: ResourceAddress,
    pub config_file: PathBuf,
}

/// Injectable terminal selection seam used by the interactive import workflow.
#[doc(hidden)]
pub trait ImportPrompter {
    fn select(&mut self, choices: &[String]) -> Result<usize, ImportError>;
    fn address(&mut self, default: &str) -> Result<String, ImportError>;
}

struct InquirePrompter;

impl ImportPrompter for InquirePrompter {
    fn select(&mut self, choices: &[String]) -> Result<usize, ImportError> {
        let selected = inquire::Select::new("Select a Dokploy resource", choices.to_vec())
            .prompt()
            .map_err(|_| ImportError::Prompt)?;
        choices
            .iter()
            .position(|choice| choice == &selected)
            .ok_or(ImportError::InvalidSelection)
    }

    fn address(&mut self, default: &str) -> Result<String, ImportError> {
        inquire::Text::new("Logical address")
            .with_default(default)
            .prompt()
            .map_err(|_| ImportError::Prompt)
    }
}

/// Discovers visible MVP resources and asks a terminal user which one to import.
pub async fn select_interactively(
    client: &Dokploy,
    config_file: PathBuf,
) -> Result<ImportRequest, ImportError> {
    select_with_prompter(client, config_file, &mut InquirePrompter).await
}

/// Runs interactive discovery through an injected deterministic prompt adapter.
#[doc(hidden)]
pub async fn select_with_prompter(
    client: &Dokploy,
    config_file: PathBuf,
    prompter: &mut dyn ImportPrompter,
) -> Result<ImportRequest, ImportError> {
    let topology = client.projects().all().await?;
    let mut libsql_ids = HashSet::new();
    let mut libsql_count = 0_usize;
    for project in topology.projects() {
        for environment in &project.environments {
            for database in &environment.libsql {
                libsql_count = libsql_count
                    .checked_add(1)
                    .ok_or(ImportError::InvalidRemoteTopology)?;
                if libsql_count > INTERACTIVE_LIBSQL_ITEM_LIMIT
                    || database.libsql_id.as_str().is_empty()
                    || !libsql_ids.insert(database.libsql_id.as_str())
                {
                    return Err(ImportError::InvalidRemoteTopology);
                }
            }
        }
    }
    let mut choices = Vec::new();
    for project in topology.projects() {
        choices.push(ImportChoice::new(
            ImportKind::Project,
            project.project_id.as_str(),
            &project.name,
        ));
        for environment in &project.environments {
            choices.push(ImportChoice::new(
                ImportKind::Environment,
                environment.environment_id.as_str(),
                &environment.name,
            ));
            for application in &environment.applications {
                choices.push(ImportChoice::new(
                    ImportKind::Application,
                    application.application_id.as_str(),
                    &application.name,
                ));
                for domain in client
                    .domains()
                    .by_application(application.application_id.clone())
                    .await?
                    .domains()
                {
                    choices.push(ImportChoice::new(
                        ImportKind::Domain,
                        domain.domain_id.as_str(),
                        &domain.host,
                    ));
                }
                for port in client
                    .ports()
                    .by_application(application.application_id.clone())
                    .await?
                    .ports()
                {
                    choices.push(ImportChoice::new(
                        ImportKind::Port,
                        port.port_id.as_str(),
                        &format!(
                            "{}-{}",
                            port.published_port,
                            port_protocol_label(port.protocol)
                        ),
                    ));
                }
                for redirect in client
                    .redirects()
                    .by_application(application.application_id.clone())
                    .await?
                    .redirects()
                {
                    choices.push(ImportChoice::new(
                        ImportKind::Redirect,
                        redirect.redirect_id.as_str(),
                        &redirect.regex,
                    ));
                }
                for entry in client
                    .security()
                    .by_application(application.application_id.clone())
                    .await?
                    .entries()
                {
                    choices.push(ImportChoice::new(
                        ImportKind::Security,
                        entry.security_id.as_str(),
                        &entry.username,
                    ));
                }
            }
            for database in &environment.postgres {
                choices.push(ImportChoice::new(
                    ImportKind::Postgres,
                    database.postgres_id.as_str(),
                    database.name.as_deref().unwrap_or("unnamed-postgres"),
                ));
            }
            for compose in client
                .composes()
                .by_environment(environment.environment_id.clone())
                .await?
                .composes()
            {
                choices.push(ImportChoice::new(
                    ImportKind::Compose,
                    compose.compose_id.as_str(),
                    &compose.name,
                ));
            }
            for database in client
                .mysql()
                .by_environment(environment.environment_id.clone())
                .await?
                .mysql()
            {
                choices.push(ImportChoice::new(
                    ImportKind::MySql,
                    database.mysql_id.as_str(),
                    &database.name,
                ));
            }
            for database in client
                .mariadb()
                .by_environment(environment.environment_id.clone())
                .await?
                .mariadb()
            {
                choices.push(ImportChoice::new(
                    ImportKind::MariaDb,
                    database.mariadb_id.as_str(),
                    &database.name,
                ));
            }
            for database in client
                .mongo()
                .by_environment(environment.environment_id.clone())
                .await?
                .mongo()
            {
                choices.push(ImportChoice::new(
                    ImportKind::Mongo,
                    database.mongo_id.as_str(),
                    &database.name,
                ));
            }
            for database in &environment.libsql {
                let details = client.libsql().get(database.libsql_id.clone()).await?;
                if details.libsql_id != database.libsql_id
                    || details.environment_id != environment.environment_id
                    || details.name.is_empty()
                {
                    return Err(ImportError::InvalidRemoteTopology);
                }
                choices.push(ImportChoice::new(
                    ImportKind::LibSql,
                    details.libsql_id.as_str(),
                    &details.name,
                ));
            }
            for database in &environment.redis {
                choices.push(ImportChoice::new(
                    ImportKind::Redis,
                    database.redis_id.as_str(),
                    database.name.as_deref().unwrap_or("unnamed-redis"),
                ));
            }
        }
    }
    if choices.is_empty() {
        return Err(ImportError::NoVisibleResources);
    }
    let labels = choices
        .iter()
        .map(|choice| {
            format!(
                "{} {} ({})",
                kind_name(choice.kind),
                choice.name,
                choice.remote_id
            )
        })
        .collect::<Vec<_>>();
    let index = prompter.select(&labels)?;
    if index >= choices.len() {
        return Err(ImportError::InvalidSelection);
    }
    let selected = &choices[index];
    let default = ResourceAddress::new(resource_kind(selected.kind), logical_name(&selected.name));
    let answer = prompter.address(&default.to_string())?;
    let address = if answer.trim().is_empty() {
        default
    } else {
        answer
            .trim()
            .parse::<ResourceAddress>()
            .map_err(|_| ImportError::InvalidAddress)?
    };

    Ok(ImportRequest {
        kind: selected.kind,
        remote_id: selected.remote_id.clone(),
        address,
        config_file,
    })
}

struct ImportChoice {
    kind: ImportKind,
    remote_id: String,
    name: String,
}

impl ImportChoice {
    fn new(kind: ImportKind, remote_id: &str, name: &str) -> Self {
        Self {
            kind,
            remote_id: remote_id.to_owned(),
            name: name.to_owned(),
        }
    }
}

const fn kind_name(kind: ImportKind) -> &'static str {
    match kind {
        ImportKind::Project => "project",
        ImportKind::Environment => "environment",
        ImportKind::Application => "application",
        ImportKind::Compose => "compose",
        ImportKind::Postgres => "postgres",
        ImportKind::MySql => "mysql",
        ImportKind::MariaDb => "mariadb",
        ImportKind::Mongo => "mongo",
        ImportKind::LibSql => "libsql",
        ImportKind::Redis => "redis",
        ImportKind::Domain => "domain",
        ImportKind::Port => "port",
        ImportKind::Redirect => "redirect",
        ImportKind::Security => "security",
        ImportKind::Mount => "mount",
        ImportKind::Schedule => "schedule",
        ImportKind::Backup => "backup",
    }
}

/// Imports one resource and its required containment ancestors without remote mutations.
pub async fn import_resource(
    client: &Dokploy,
    request: ImportRequest,
) -> Result<usize, ImportError> {
    if request.address.kind() != resource_kind(request.kind) {
        return Err(ImportError::AddressKindMismatch);
    }
    let workspace = canonical_workspace(&request.config_file)?;
    if request.config_file.exists() {
        return Err(ImportError::ConfigExists);
    }
    let instance = InstanceIdentity::parse(client.base_url().as_str())?;
    let store = StateStore::new(&workspace, instance.clone())?;
    if store.inspect()?.is_some() {
        return Err(ImportError::StateExists);
    }

    let imported = discover(client, request.kind, &request.remote_id, &request.address).await?;
    imported.document.render()?;
    let mut session = store.begin_write()?;
    if store.inspect()?.is_some() || request.config_file.exists() {
        return Err(ImportError::WorkspaceChanged);
    }

    let resources = imported
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
    persist_import(&imported.document, &request.config_file, || {
        session.checkpoint(ExpectedState::absent(), &state)
    })?;

    Ok(state.resources().len())
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

async fn discover(
    client: &Dokploy,
    kind: ImportKind,
    remote_id: &str,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    match kind {
        ImportKind::Project => {
            let project = client.projects().get(ProjectId::new(remote_id)).await?;
            build_project(project, target)
        }
        ImportKind::Environment => {
            let environment = client
                .environments()
                .get(EnvironmentId::new(remote_id))
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            build_environment(project, environment, target)
        }
        ImportKind::Application => {
            let application = client
                .applications()
                .get(ApplicationId::new(remote_id))
                .await?;
            let environment = client
                .environments()
                .get(application.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            let associations = imported_associations(client, &application).await?;
            build_application(project, environment, application, &associations, target)
        }
        ImportKind::Compose => {
            let requested_id = ComposeId::new(remote_id);
            let compose = client.composes().get(requested_id.clone()).await?;
            if compose.compose_id != requested_id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let environment = client
                .environments()
                .get(compose.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            let collection = client
                .composes()
                .by_environment(compose.environment_id.clone())
                .await?;
            validate_compose_import_authority(&compose, collection.composes())?;
            {
                let server = imported_server(client, &compose.server_id).await?;
                build_compose(project, environment, compose, server.as_deref(), target)
            }
        }
        ImportKind::Postgres => {
            let database = client.postgres().get(PostgresId::new(remote_id)).await?;
            let environment = client
                .environments()
                .get(database.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            {
                let server = imported_server(client, &database.server_id).await?;
                build_postgres(project, environment, database, server.as_deref(), target)
            }
        }
        ImportKind::MySql => {
            let database = client.mysql().get(MySqlId::new(remote_id)).await?;
            let environment = client
                .environments()
                .get(database.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            {
                let server = imported_server(client, &database.server_id).await?;
                build_mysql(project, environment, database, server.as_deref(), target)
            }
        }
        ImportKind::MariaDb => {
            let database = client.mariadb().get(MariaDbId::new(remote_id)).await?;
            let environment = client
                .environments()
                .get(database.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            {
                let server = imported_server(client, &database.server_id).await?;
                build_mariadb(project, environment, database, server.as_deref(), target)
            }
        }
        ImportKind::Mongo => {
            let database = client.mongo().get(MongoId::new(remote_id)).await?;
            let environment = client
                .environments()
                .get(database.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            {
                let server = imported_server(client, &database.server_id).await?;
                build_mongo(project, environment, database, server.as_deref(), target)
            }
        }
        ImportKind::LibSql => {
            let database = client.libsql().get(LibSqlId::new(remote_id)).await?;
            let environment = client
                .environments()
                .get(database.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            {
                let server = imported_server(client, &database.server_id).await?;
                build_libsql(project, environment, database, server.as_deref(), target)
            }
        }
        ImportKind::Redis => {
            let database = client.redis().get(RedisId::new(remote_id)).await?;
            let environment = client
                .environments()
                .get(database.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            {
                let server = imported_server(client, &database.server_id).await?;
                build_redis(project, environment, database, server.as_deref(), target)
            }
        }
        ImportKind::Domain => {
            let domain = client.domains().get(DomainId::new(remote_id)).await?;
            let application_id = domain
                .application_id
                .clone()
                .ok_or(ImportError::MissingContainment)?;
            let application = client.applications().get(application_id).await?;
            let environment = client
                .environments()
                .get(application.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            let associations = imported_associations(client, &application).await?;
            build_domain(
                project,
                environment,
                application,
                &associations,
                domain,
                target,
            )
        }
        ImportKind::Port => {
            let requested_id = PortId::new(remote_id);
            let port = client.ports().get(requested_id.clone()).await?;
            if port.port_id != requested_id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let collection = client
                .ports()
                .by_application(port.application_id.clone())
                .await?;
            validate_port_import_authority(&port, collection.ports())?;
            let application = client
                .applications()
                .get(port.application_id.clone())
                .await?;
            let environment = client
                .environments()
                .get(application.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            let associations = imported_associations(client, &application).await?;
            build_port(
                project,
                environment,
                application,
                &associations,
                port,
                target,
            )
        }
        ImportKind::Redirect => {
            let requested_id = RedirectId::new(remote_id);
            let redirect = client.redirects().get(requested_id.clone()).await?;
            if redirect.redirect_id != requested_id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let collection = client
                .redirects()
                .by_application(redirect.application_id.clone())
                .await?;
            validate_redirect_import_authority(&redirect, collection.redirects())?;
            let application = client
                .applications()
                .get(redirect.application_id.clone())
                .await?;
            let environment = client
                .environments()
                .get(application.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            let associations = imported_associations(client, &application).await?;
            build_redirect(
                project,
                environment,
                application,
                &associations,
                redirect,
                target,
            )
        }
        ImportKind::Security => {
            let requested_id = SecurityId::new(remote_id);
            let entry = client.security().get(requested_id.clone()).await?;
            if entry.security_id != requested_id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let collection = client
                .security()
                .by_application(entry.application_id.clone())
                .await?;
            validate_security_import_authority(&entry, collection.entries())?;
            let application = client
                .applications()
                .get(entry.application_id.clone())
                .await?;
            let environment = client
                .environments()
                .get(application.environment_id.clone())
                .await?;
            let project = client
                .projects()
                .get(environment.project_id.clone())
                .await?;
            let associations = imported_associations(client, &application).await?;
            build_security(
                project,
                environment,
                application,
                &associations,
                entry,
                target,
            )
        }
        ImportKind::Mount => mount::discover_mount(client, remote_id, target).await,
        ImportKind::Schedule => schedule::discover_schedule(client, remote_id, target).await,
        ImportKind::Backup => backup::discover_backup(client, remote_id, target).await,
    }
}

fn validate_redirect_import_authority(
    direct: &RedirectDetails,
    collection: &[RedirectDetails],
) -> Result<(), ImportError> {
    let matching = collection
        .iter()
        .filter(|candidate| candidate.redirect_id == direct.redirect_id)
        .collect::<Vec<_>>();
    let collisions = collection
        .iter()
        .filter(|candidate| candidate.regex == direct.regex)
        .count();
    if matching.as_slice() != [direct] || collisions != 1 {
        return Err(ImportError::InvalidRemoteTopology);
    }

    Ok(())
}

fn validate_security_import_authority(
    direct: &SecurityDetails,
    collection: &[SecurityDetails],
) -> Result<(), ImportError> {
    let matching = collection
        .iter()
        .filter(|candidate| candidate.security_id == direct.security_id)
        .collect::<Vec<_>>();
    let collisions = collection
        .iter()
        .filter(|candidate| candidate.username == direct.username)
        .count();
    if matching.as_slice() != [direct] || collisions != 1 {
        return Err(ImportError::InvalidRemoteTopology);
    }

    Ok(())
}

fn validate_port_import_authority(
    direct: &PortDetails,
    collection: &[PortDetails],
) -> Result<(), ImportError> {
    let matching = collection
        .iter()
        .filter(|candidate| candidate.port_id == direct.port_id)
        .collect::<Vec<_>>();
    if matching.as_slice() != [direct] {
        return Err(ImportError::InvalidRemoteTopology);
    }

    Ok(())
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

/// Starts a context holding the project and one environment.
fn single_environment(
    project: &ProjectDetails,
    environment: &EnvironmentDetails,
) -> Result<(ImportContext, EnvScope), ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let mut context = ImportContext::new(project, &project_address)?;
    let scope = context.add_environment(
        environment,
        address(ResourceKind::Environment, &environment.name)?,
    )?;

    Ok((context, scope))
}

fn build_project(
    project: ProjectDetails,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    ImportContext::new(&project, target)
}

fn build_environment(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let mut context = ImportContext::new(&project, &project_address)?;
    context.add_environment(&environment, target.clone())?;

    Ok(context)
}

fn build_application(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    application: ApplicationDetails,
    associations: &ImportedAssociations,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope) = single_environment(&project, &environment)?;
    context.add_application(&scope, &application, associations, target)?;

    Ok(context)
}

/// Adds an application's ancestry and returns the context with the application address.
fn application_ancestry(
    project: &ProjectDetails,
    environment: &EnvironmentDetails,
    application: &ApplicationDetails,
    associations: &ImportedAssociations,
) -> Result<(ImportContext, EnvScope, ResourceAddress), ImportError> {
    let (mut context, scope) = single_environment(project, environment)?;
    let application_address = address(ResourceKind::Application, &application.name)?;
    context.add_application(&scope, application, associations, &application_address)?;

    Ok((context, scope, application_address))
}

fn build_port(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    application: ApplicationDetails,
    associations: &ImportedAssociations,
    port: PortDetails,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope, application_address) =
        application_ancestry(&project, &environment, &application, associations)?;
    context.add_port(&scope, &application_address, &port, target)?;

    Ok(context)
}

fn build_redirect(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    application: ApplicationDetails,
    associations: &ImportedAssociations,
    redirect: RedirectDetails,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope, application_address) =
        application_ancestry(&project, &environment, &application, associations)?;
    context.add_redirect(&scope, &application_address, &redirect, target)?;

    Ok(context)
}

fn build_security(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    application: ApplicationDetails,
    associations: &ImportedAssociations,
    entry: SecurityDetails,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope, application_address) =
        application_ancestry(&project, &environment, &application, associations)?;
    context.add_security(&scope, &application_address, &entry, target)?;

    Ok(context)
}

fn build_domain(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    application: ApplicationDetails,
    associations: &ImportedAssociations,
    domain: dokploy_sdk::DomainDetails,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope, application_address) =
        application_ancestry(&project, &environment, &application, associations)?;
    context.add_domain(&scope, &application_address, &domain, target)?;

    Ok(context)
}

fn build_compose(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    compose: dokploy_sdk::ComposeDetails,
    server: Option<&str>,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope) = single_environment(&project, &environment)?;
    context.add_compose(&scope, &compose, server, target)?;

    Ok(context)
}

fn build_postgres(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::PostgresDetails,
    server: Option<&str>,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope) = single_environment(&project, &environment)?;
    context.add_postgres(&scope, &database, server, target)?;

    Ok(context)
}

fn build_redis(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::RedisDetails,
    server: Option<&str>,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope) = single_environment(&project, &environment)?;
    context.add_redis(&scope, &database, server, target)?;

    Ok(context)
}

fn build_mysql(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::MySqlDetails,
    server: Option<&str>,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope) = single_environment(&project, &environment)?;
    context.add_mysql(&scope, &database, server, target)?;

    Ok(context)
}

fn build_mariadb(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::MariaDbDetails,
    server: Option<&str>,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope) = single_environment(&project, &environment)?;
    context.add_mariadb(&scope, &database, server, target)?;

    Ok(context)
}

fn build_mongo(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::MongoDetails,
    server: Option<&str>,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope) = single_environment(&project, &environment)?;
    context.add_mongo(&scope, &database, server, target)?;

    Ok(context)
}

fn build_libsql(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::LibSqlDetails,
    server: Option<&str>,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let (mut context, scope) = single_environment(&project, &environment)?;
    context.add_libsql(&scope, &database, server, target)?;

    Ok(context)
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

/// Reads fresh minimal collections only when the application has an association.
///
/// An identity that is unknown, unreadable, or whose name is shared by another
/// record cannot be written back as an unambiguous selector, so the import fails
/// closed instead of guessing.
async fn imported_associations(
    client: &Dokploy,
    application: &ApplicationDetails,
) -> Result<ImportedAssociations, ImportError> {
    fn id<T>(field: &ResponseField<T>, as_str: fn(&T) -> &str) -> Option<String> {
        match field {
            ResponseField::Value(value) => Some(as_str(value).to_owned()),
            ResponseField::NotReturned | ResponseField::Null => None,
        }
    }
    let server = id(&application.server_id, dokploy_sdk::ServerId::as_str);
    let build_server = id(&application.build_server_id, dokploy_sdk::ServerId::as_str);
    let registry = id(&application.registry_id, dokploy_sdk::RegistryId::as_str);
    let build_registry = id(
        &application.build_registry_id,
        dokploy_sdk::RegistryId::as_str,
    );
    let rollback_registry = id(
        &application.rollback_registry_id,
        dokploy_sdk::RegistryId::as_str,
    );

    let mut kinds = std::collections::BTreeSet::new();
    if server.is_some() || build_server.is_some() {
        kinds.insert(SelectorKind::Server);
    }
    if registry.is_some() || build_registry.is_some() || rollback_registry.is_some() {
        kinds.insert(SelectorKind::Registry);
    }
    if kinds.is_empty() {
        return Ok(ImportedAssociations::default());
    }

    let directory = ExternalDirectory::load(client, &kinds).await;
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
        server: name(SelectorKind::Server, server)?,
        build_server: name(SelectorKind::Server, build_server)?,
        registry: name(SelectorKind::Registry, registry)?,
        build_registry: name(SelectorKind::Registry, build_registry)?,
        rollback_registry: name(SelectorKind::Registry, rollback_registry)?,
    })
}

/// Reads the fresh server collection only when a service is attached to a server.
///
/// A local (`null`) or omitted placement stays unmanaged. An identity that is
/// unknown, unreadable, or whose name is shared by another record cannot be
/// written back as an unambiguous selector, so the import fails closed.
async fn imported_server(
    client: &Dokploy,
    field: &ResponseField<dokploy_sdk::ServerId>,
) -> Result<Option<String>, ImportError> {
    let ResponseField::Value(server_id) = field else {
        return Ok(None);
    };
    let kinds = std::collections::BTreeSet::from([SelectorKind::Server]);
    let directory = ExternalDirectory::load(client, &kinds).await;
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

fn address(kind: ResourceKind, name: &str) -> Result<ResourceAddress, ImportError> {
    Ok(ResourceAddress::new(kind, logical_name(name)))
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

const fn resource_kind(kind: ImportKind) -> ResourceKind {
    match kind {
        ImportKind::Project => ResourceKind::Project,
        ImportKind::Environment => ResourceKind::Environment,
        ImportKind::Application => ResourceKind::Application,
        ImportKind::Compose => ResourceKind::Compose,
        ImportKind::Postgres => ResourceKind::Postgres,
        ImportKind::MySql => ResourceKind::MySql,
        ImportKind::MariaDb => ResourceKind::MariaDb,
        ImportKind::Mongo => ResourceKind::Mongo,
        ImportKind::LibSql => ResourceKind::LibSql,
        ImportKind::Redis => ResourceKind::Redis,
        ImportKind::Domain => ResourceKind::Domain,
        ImportKind::Port => ResourceKind::Port,
        ImportKind::Redirect => ResourceKind::Redirect,
        ImportKind::Security => ResourceKind::Security,
        ImportKind::Mount => ResourceKind::Mount,
        ImportKind::Schedule => ResourceKind::Schedule,
        ImportKind::Backup => ResourceKind::Backup,
    }
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
    #[error("the logical address kind does not match the selected import kind")]
    AddressKindMismatch,
    #[error("dokploy.yaml already exists")]
    ConfigExists,
    #[error("durable state already exists")]
    StateExists,
    #[error("the workspace changed during import")]
    WorkspaceChanged,
    #[error("the selected resource containment is unavailable")]
    MissingContainment,
    #[error("the selected LibSQL node topology is invalid")]
    InvalidLibSqlNode,
    #[error("the remote resource topology is invalid")]
    InvalidRemoteTopology,
    #[error(
        "an external server, registry, or destination association is unknown, unreadable, or has a name shared by another record"
    )]
    ExternalAssociation,
    #[error("no importable resources are visible")]
    NoVisibleResources,
    #[error("the interactive import selection is invalid")]
    InvalidSelection,
    #[error("the logical import address is invalid")]
    InvalidAddress,
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
