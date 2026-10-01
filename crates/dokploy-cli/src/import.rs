//! Read-only adoption of existing Dokploy resources into one new workspace.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use dokploy_config::{
    ApplicationDocument, ConfigDocument, ConfigWriteError, DomainDocument, EnvironmentDocument,
    Field, LibSqlDocument, LibSqlNodeConfig, LifecycleDocument, MariaDbDocument, MongoDocument,
    MySqlDocument, PostgresDocument, RedisDocument, SourceDocument,
};
use dokploy_sdk::{
    ApplicationDetails, ApplicationId, Dokploy, DomainId, EnvironmentDetails, EnvironmentId,
    Error as SdkError, LibSqlId, MariaDbId, MongoId, MySqlId, PostgresId, ProjectDetails,
    ProjectId, RedisId, ResponseField,
};
use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceName, ResourceState, StateFile, StateStore,
};
use thiserror::Error;

use crate::cli::ImportKind;

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
            }
            for database in &environment.postgres {
                choices.push(ImportChoice::new(
                    ImportKind::Postgres,
                    database.postgres_id.as_str(),
                    database.name.as_deref().unwrap_or("unnamed-postgres"),
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
        ImportKind::Postgres => "postgres",
        ImportKind::MySql => "mysql",
        ImportKind::MariaDb => "mariadb",
        ImportKind::Mongo => "mongo",
        ImportKind::LibSql => "libsql",
        ImportKind::Redis => "redis",
        ImportKind::Domain => "domain",
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

struct ImportedWorkspace {
    document: ConfigDocument,
    resources: Vec<ImportedResource>,
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
) -> Result<ImportedWorkspace, ImportError> {
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
            build_application(project, environment, application, target)
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
            build_postgres(project, environment, database, target)
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
            build_mysql(project, environment, database, target)
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
            build_mariadb(project, environment, database, target)
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
            build_mongo(project, environment, database, target)
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
            build_libsql(project, environment, database, target)
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
            build_redis(project, environment, database, target)
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
            build_domain(project, environment, application, domain, target)
        }
    }
}

fn build_project(
    project: ProjectDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let mut document = ConfigDocument::new(target.name().clone());
    document.project_mut().description = response_field(&project.description);
    let address = target.clone();
    let state = resource_state(
        &address,
        project.project_id.as_str(),
        false,
        description_inputs(&project.description),
        None,
    )?;

    Ok(ImportedWorkspace {
        document,
        resources: vec![ImportedResource { address, state }],
    })
}

fn build_environment(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let mut imported = build_project(project, &project_address)?;
    let mut config = EnvironmentDocument::default();
    config.description = response_field(&environment.description);
    imported
        .document
        .add_environment(target.name().clone(), config)?;
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state(
            target,
            environment.environment_id.as_str(),
            false,
            description_inputs(&environment.description),
            Some(project_address),
        )?,
    });

    Ok(imported)
}

fn build_application(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    application: ApplicationDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let environment_address = address(ResourceKind::Environment, &environment.name)?;
    let mut imported = build_project(project, &project_address)?;
    let mut environment_config = EnvironmentDocument::default();
    environment_config.description = response_field(&environment.description);
    let (application_config, inputs) = application_config(&application);
    environment_config.add_application(target.name().clone(), application_config)?;
    imported
        .document
        .add_environment(environment_address.name().clone(), environment_config)?;
    imported.resources.push(ImportedResource {
        address: environment_address.clone(),
        state: resource_state(
            &environment_address,
            environment.environment_id.as_str(),
            false,
            description_inputs(&environment.description),
            Some(project_address),
        )?,
    });
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state(
            target,
            application.application_id.as_str(),
            false,
            inputs,
            Some(environment_address),
        )?,
    });

    Ok(imported)
}

fn build_postgres(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::PostgresDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let environment_address = address(ResourceKind::Environment, &environment.name)?;
    let mut imported = build_project(project, &project_address)?;
    let mut environment_config = EnvironmentDocument::default();
    environment_config.description = response_field(&environment.description);
    let config = PostgresDocument {
        database: response_field(&database.database_name),
        username: response_field(&database.database_user),
        lifecycle: LifecycleDocument {
            protect: Field::Set(true),
            ..LifecycleDocument::default()
        },
        ..PostgresDocument::default()
    };
    environment_config.add_postgres(target.name().clone(), config)?;
    imported
        .document
        .add_environment(environment_address.name().clone(), environment_config)?;
    push_environment_state(
        &mut imported,
        &environment,
        environment_address.clone(),
        project_address,
    )?;
    let mut inputs = serde_json::Map::new();
    insert_response(&mut inputs, "database", &database.database_name);
    insert_response(&mut inputs, "username", &database.database_user);
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state(
            target,
            database.postgres_id.as_str(),
            true,
            inputs,
            Some(environment_address),
        )?,
    });

    Ok(imported)
}

fn build_redis(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::RedisDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let environment_address = address(ResourceKind::Environment, &environment.name)?;
    let mut imported = build_project(project, &project_address)?;
    let mut environment_config = EnvironmentDocument::default();
    environment_config.description = response_field(&environment.description);
    let config = RedisDocument {
        lifecycle: LifecycleDocument {
            protect: Field::Set(true),
            ..LifecycleDocument::default()
        },
        ..RedisDocument::default()
    };
    environment_config.add_redis(target.name().clone(), config)?;
    imported
        .document
        .add_environment(environment_address.name().clone(), environment_config)?;
    push_environment_state(
        &mut imported,
        &environment,
        environment_address.clone(),
        project_address,
    )?;
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state(
            target,
            database.redis_id.as_str(),
            true,
            serde_json::Map::new(),
            Some(environment_address),
        )?,
    });

    Ok(imported)
}

fn build_mysql(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::MySqlDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let environment_address = address(ResourceKind::Environment, &environment.name)?;
    let mut imported = build_project(project, &project_address)?;
    let mut environment_config = EnvironmentDocument::default();
    environment_config.description = response_field(&environment.description);
    let config = MySqlDocument {
        database: response_field(&database.database_name),
        username: response_field(&database.database_user),
        lifecycle: LifecycleDocument {
            protect: Field::Set(true),
            ..LifecycleDocument::default()
        },
        ..MySqlDocument::default()
    };
    environment_config.add_mysql(target.name().clone(), config)?;
    imported
        .document
        .add_environment(environment_address.name().clone(), environment_config)?;
    push_environment_state(
        &mut imported,
        &environment,
        environment_address.clone(),
        project_address,
    )?;
    let mut inputs = serde_json::Map::new();
    insert_response(&mut inputs, "database", &database.database_name);
    insert_response(&mut inputs, "username", &database.database_user);
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state(
            target,
            database.mysql_id.as_str(),
            true,
            inputs,
            Some(environment_address),
        )?,
    });

    Ok(imported)
}

fn build_mariadb(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::MariaDbDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let environment_address = address(ResourceKind::Environment, &environment.name)?;
    let mut imported = build_project(project, &project_address)?;
    let mut environment_config = EnvironmentDocument::default();
    environment_config.description = response_field(&environment.description);
    let config = MariaDbDocument {
        database: response_field(&database.database_name),
        username: response_field(&database.database_user),
        lifecycle: LifecycleDocument {
            protect: Field::Set(true),
            ..LifecycleDocument::default()
        },
        ..MariaDbDocument::default()
    };
    environment_config.add_mariadb(target.name().clone(), config)?;
    imported
        .document
        .add_environment(environment_address.name().clone(), environment_config)?;
    push_environment_state(
        &mut imported,
        &environment,
        environment_address.clone(),
        project_address,
    )?;
    let mut inputs = serde_json::Map::new();
    insert_response(&mut inputs, "database", &database.database_name);
    insert_response(&mut inputs, "username", &database.database_user);
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state(
            target,
            database.mariadb_id.as_str(),
            true,
            inputs,
            Some(environment_address),
        )?,
    });

    Ok(imported)
}

fn build_mongo(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::MongoDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let environment_address = address(ResourceKind::Environment, &environment.name)?;
    let mut imported = build_project(project, &project_address)?;
    let mut environment_config = EnvironmentDocument::default();
    environment_config.description = response_field(&environment.description);
    let config = MongoDocument {
        username: response_field(&database.database_user),
        replica_sets: response_field(&database.replica_sets),
        lifecycle: LifecycleDocument {
            protect: Field::Set(true),
            ..LifecycleDocument::default()
        },
        ..MongoDocument::default()
    };
    environment_config.add_mongo(target.name().clone(), config)?;
    imported
        .document
        .add_environment(environment_address.name().clone(), environment_config)?;
    push_environment_state(
        &mut imported,
        &environment,
        environment_address.clone(),
        project_address,
    )?;
    let mut inputs = serde_json::Map::new();
    insert_response(&mut inputs, "username", &database.database_user);
    insert_response(&mut inputs, "replica_sets", &database.replica_sets);
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state(
            target,
            database.mongo_id.as_str(),
            true,
            inputs,
            Some(environment_address),
        )?,
    });

    Ok(imported)
}

fn build_libsql(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    database: dokploy_sdk::LibSqlDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let project_address = address(ResourceKind::Project, &project.name)?;
    let environment_address = address(ResourceKind::Environment, &environment.name)?;
    let mut imported = build_project(project, &project_address)?;
    let mut environment_config = EnvironmentDocument::default();
    environment_config.description = response_field(&environment.description);
    let node = imported_libsql_node(&database)?;
    let description = match &database.description {
        Some(description) => Field::Set(description.clone()),
        None => Field::Clear,
    };
    let config = LibSqlDocument {
        description,
        username: response_field(&database.database_user),
        node: Field::Set(node.clone()),
        lifecycle: LifecycleDocument {
            protect: Field::Set(true),
            ..LifecycleDocument::default()
        },
        ..LibSqlDocument::default()
    };
    environment_config.add_libsql(target.name().clone(), config)?;
    imported
        .document
        .add_environment(environment_address.name().clone(), environment_config)?;
    push_environment_state(
        &mut imported,
        &environment,
        environment_address.clone(),
        project_address,
    )?;
    let mut inputs = serde_json::Map::new();
    inputs.insert(
        "description".to_owned(),
        database
            .description
            .map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    insert_response(&mut inputs, "username", &database.database_user);
    inputs.insert(
        "node".to_owned(),
        match node {
            LibSqlNodeConfig::Primary => serde_json::json!({"type":"primary"}),
            LibSqlNodeConfig::Replica { primary_url } => {
                serde_json::json!({"type":"replica","primary_url":primary_url})
            }
        },
    );
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state(
            target,
            database.libsql_id.as_str(),
            true,
            inputs,
            Some(environment_address),
        )?,
    });

    Ok(imported)
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

fn build_domain(
    project: ProjectDetails,
    environment: EnvironmentDetails,
    application: ApplicationDetails,
    domain: dokploy_sdk::DomainDetails,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let application_address = address(ResourceKind::Application, &application.name)?;
    let mut imported = build_application(project, environment, application, &application_address)?;
    let environment_address = imported
        .resources
        .iter()
        .find(|item| item.address.kind() == ResourceKind::Environment)
        .map(|item| item.address.clone())
        .ok_or(ImportError::MissingContainment)?;
    let environment_config = imported
        .document
        .environment_mut(environment_address.name())
        .ok_or(ImportError::MissingContainment)?;
    environment_config.add_domain(
        target.name().clone(),
        DomainDocument {
            host: Field::Set(domain.host.clone()),
            application: Field::Set(application_address.clone()),
            ..DomainDocument::default()
        },
    )?;
    let mut inputs = serde_json::Map::new();
    inputs.insert("host".to_owned(), serde_json::json!(domain.host));
    inputs.insert(
        "application".to_owned(),
        serde_json::json!(application_address.to_string()),
    );
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state_with_dependencies(
            target,
            domain.domain_id.as_str(),
            false,
            inputs,
            Some(environment_address),
            vec![application_address],
        )?,
    });

    Ok(imported)
}

fn push_environment_state(
    imported: &mut ImportedWorkspace,
    environment: &EnvironmentDetails,
    address: ResourceAddress,
    project: ResourceAddress,
) -> Result<(), ImportError> {
    imported.resources.push(ImportedResource {
        state: resource_state(
            &address,
            environment.environment_id.as_str(),
            false,
            description_inputs(&environment.description),
            Some(project),
        )?,
        address,
    });
    Ok(())
}

fn application_config(
    application: &ApplicationDetails,
) -> (
    ApplicationDocument,
    serde_json::Map<String, serde_json::Value>,
) {
    let mut config = ApplicationDocument::default();
    let mut inputs = serde_json::Map::new();
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
        ImportKind::Postgres => ResourceKind::Postgres,
        ImportKind::MySql => ResourceKind::MySql,
        ImportKind::MariaDb => ResourceKind::MariaDb,
        ImportKind::Mongo => ResourceKind::Mongo,
        ImportKind::LibSql => ResourceKind::LibSql,
        ImportKind::Redis => ResourceKind::Redis,
        ImportKind::Domain => ResourceKind::Domain,
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
