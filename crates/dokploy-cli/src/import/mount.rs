//! Protected import of one existing Mount together with its target ancestry.
//!
//! The import is read-only. The Mount's typed target service, its environment,
//! and the project are adopted into the same new workspace so the Mount's
//! containment and target dependency are written correctly. File content is
//! write-only upstream and is therefore left unmanaged; the Mount is imported
//! protected so it can neither be replaced nor destroyed by the first plan.

use dokploy_config::{MountDocument, MountSourceConfig};
use dokploy_sdk::{EnvironmentId, MountDetails, MountType, ServiceTarget};

use super::context::{EnvScope, ImportContext};
use super::*;

impl ImportContext {
    pub(super) fn add_mount(
        &mut self,
        env: &EnvScope,
        service: &ResourceAddress,
        mount: &MountDetails,
        source: MountSourceConfig,
        mut inputs: serde_json::Map<String, serde_json::Value>,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        inputs.insert("target".to_owned(), serde_json::json!(service.to_string()));
        inputs.insert(
            "mount_type".to_owned(),
            serde_json::json!(source.type_name()),
        );
        inputs.insert("mount_path".to_owned(), serde_json::json!(mount.mount_path));
        self.environment(env)?.add_mount(
            address.name().clone(),
            MountDocument {
                target: service.clone(),
                mount_path: mount.mount_path.clone(),
                source,
                depends_on: Vec::new(),
                lifecycle: LifecycleDocument {
                    protect: Field::Set(true),
                    ..LifecycleDocument::default()
                },
            },
        )?;
        self.push(
            address,
            mount.mount_id.as_str(),
            true,
            inputs,
            env.address(),
            vec![service.clone()],
        )
    }
}

pub(super) async fn discover_mount(
    client: &Dokploy,
    remote_id: &str,
    target: &ResourceAddress,
) -> Result<ImportContext, ImportError> {
    let requested_id = dokploy_sdk::MountId::new(remote_id);
    let mount = client.mounts().get(requested_id.clone()).await?;
    if mount.mount_id != requested_id {
        return Err(ImportError::InvalidRemoteTopology);
    }
    let collection = client.mounts().by_target(mount.target.clone()).await?;
    let matching = collection
        .mounts()
        .iter()
        .filter(|candidate| candidate.mount_id == mount.mount_id)
        .collect::<Vec<_>>();
    if matching.as_slice() != [&mount] {
        return Err(ImportError::InvalidRemoteTopology);
    }

    let (mut imported, scope, target_address) = import_target(client, &mount.target).await?;
    let (source, inputs) = imported_source(&mount)?;
    imported.add_mount(&scope, &target_address, &mount, source, inputs, target)?;

    Ok(imported)
}

fn imported_source(
    mount: &MountDetails,
) -> Result<
    (
        MountSourceConfig,
        serde_json::Map<String, serde_json::Value>,
    ),
    ImportError,
> {
    let value = |field: &ResponseField<String>| match field {
        ResponseField::Value(value) if !value.is_empty() => Ok(value.clone()),
        ResponseField::NotReturned | ResponseField::Null | ResponseField::Value(_) => {
            Err(ImportError::InvalidRemoteTopology)
        }
    };
    let mut inputs = serde_json::Map::new();
    let source = match mount.mount_type {
        MountType::Bind => {
            let host_path = value(&mount.host_path)?;
            inputs.insert("host_path".to_owned(), serde_json::json!(host_path));
            MountSourceConfig::Bind { host_path }
        }
        MountType::Volume => {
            let volume_name = value(&mount.volume_name)?;
            inputs.insert("volume_name".to_owned(), serde_json::json!(volume_name));
            MountSourceConfig::Volume { volume_name }
        }
        MountType::File => {
            let file_path = value(&mount.file_path)?;
            inputs.insert("file_path".to_owned(), serde_json::json!(file_path));
            MountSourceConfig::File {
                file_path,
                content: Field::Unmanaged,
            }
        }
    };

    Ok((source, inputs))
}

async fn environment_and_project(
    client: &Dokploy,
    environment_id: &EnvironmentId,
) -> Result<(EnvironmentDetails, ProjectDetails), ImportError> {
    let environment = client.environments().get(environment_id.clone()).await?;
    if environment.environment_id != *environment_id {
        return Err(ImportError::InvalidRemoteTopology);
    }
    let project = client
        .projects()
        .get(environment.project_id.clone())
        .await?;

    Ok((environment, project))
}

/// Imports the target service and its ancestry, returning the context, its
/// environment scope, and the service address.
pub(super) async fn import_target(
    client: &Dokploy,
    target: &ServiceTarget,
) -> Result<(ImportContext, EnvScope, ResourceAddress), ImportError> {
    match target {
        ServiceTarget::Application(id) => {
            let application = client.applications().get(id.clone()).await?;
            if application.application_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &application.environment_id).await?;
            let associations = imported_associations(client, &application).await?;
            let (mut context, scope) = single_environment(&project, &environment)?;
            let address = address(ResourceKind::Application, &application.name)?;
            context.add_application(&scope, &application, &associations, &address)?;
            Ok((context, scope, address))
        }
        ServiceTarget::Compose(id) => {
            let compose = client.composes().get(id.clone()).await?;
            if compose.compose_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &compose.environment_id).await?;
            let collection = client
                .composes()
                .by_environment(compose.environment_id.clone())
                .await?;
            validate_compose_import_authority(&compose, collection.composes())?;
            let server = imported_server(client, &compose.server_id).await?;
            let (mut context, scope) = single_environment(&project, &environment)?;
            let address = address(ResourceKind::Compose, &compose.name)?;
            context.add_compose(&scope, &compose, server.as_deref(), &address)?;
            Ok((context, scope, address))
        }
        ServiceTarget::Postgres(id) => {
            let database = client.postgres().get(id.clone()).await?;
            if database.postgres_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let server = imported_server(client, &database.server_id).await?;
            let (mut context, scope) = single_environment(&project, &environment)?;
            let address = address(ResourceKind::Postgres, &database.name)?;
            context.add_postgres(&scope, &database, server.as_deref(), &address)?;
            Ok((context, scope, address))
        }
        ServiceTarget::MySql(id) => {
            let database = client.mysql().get(id.clone()).await?;
            if database.mysql_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let server = imported_server(client, &database.server_id).await?;
            let (mut context, scope) = single_environment(&project, &environment)?;
            let address = address(ResourceKind::MySql, &database.name)?;
            context.add_mysql(&scope, &database, server.as_deref(), &address)?;
            Ok((context, scope, address))
        }
        ServiceTarget::MariaDb(id) => {
            let database = client.mariadb().get(id.clone()).await?;
            if database.mariadb_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let server = imported_server(client, &database.server_id).await?;
            let (mut context, scope) = single_environment(&project, &environment)?;
            let address = address(ResourceKind::MariaDb, &database.name)?;
            context.add_mariadb(&scope, &database, server.as_deref(), &address)?;
            Ok((context, scope, address))
        }
        ServiceTarget::Mongo(id) => {
            let database = client.mongo().get(id.clone()).await?;
            if database.mongo_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let server = imported_server(client, &database.server_id).await?;
            let (mut context, scope) = single_environment(&project, &environment)?;
            let address = address(ResourceKind::Mongo, &database.name)?;
            context.add_mongo(&scope, &database, server.as_deref(), &address)?;
            Ok((context, scope, address))
        }
        ServiceTarget::LibSql(id) => {
            let database = client.libsql().get(id.clone()).await?;
            if database.libsql_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let server = imported_server(client, &database.server_id).await?;
            let (mut context, scope) = single_environment(&project, &environment)?;
            let address = address(ResourceKind::LibSql, &database.name)?;
            context.add_libsql(&scope, &database, server.as_deref(), &address)?;
            Ok((context, scope, address))
        }
        ServiceTarget::Redis(id) => {
            let database = client.redis().get(id.clone()).await?;
            if database.redis_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let server = imported_server(client, &database.server_id).await?;
            let (mut context, scope) = single_environment(&project, &environment)?;
            let address = address(ResourceKind::Redis, &database.name)?;
            context.add_redis(&scope, &database, server.as_deref(), &address)?;
            Ok((context, scope, address))
        }
    }
}
