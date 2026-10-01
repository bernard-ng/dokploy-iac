//! Protected import of one existing Mount together with its target ancestry.
//!
//! The import is read-only. The Mount's typed target service, its environment,
//! and the project are adopted into the same new workspace so the Mount's
//! containment and target dependency are written correctly. File content is
//! write-only upstream and is therefore left unmanaged; the Mount is imported
//! protected so it can neither be replaced nor destroyed by the first plan.

use dokploy_config::{MountDocument, MountSourceConfig};
use dokploy_sdk::{EnvironmentId, MountDetails, MountType, ServiceTarget};

use super::*;

pub(super) async fn discover_mount(
    client: &Dokploy,
    remote_id: &str,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
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

    let (mut imported, target_address) = import_target(client, &mount.target).await?;
    let environment_address = imported
        .resources
        .iter()
        .find(|item| item.address.kind() == ResourceKind::Environment)
        .map(|item| item.address.clone())
        .ok_or(ImportError::MissingContainment)?;
    let (source, mut inputs) = imported_source(&mount)?;
    inputs.insert(
        "target".to_owned(),
        serde_json::json!(target_address.to_string()),
    );
    inputs.insert(
        "mount_type".to_owned(),
        serde_json::json!(source.type_name()),
    );
    inputs.insert("mount_path".to_owned(), serde_json::json!(mount.mount_path));

    imported
        .document
        .environment_mut(environment_address.name())
        .ok_or(ImportError::MissingContainment)?
        .add_mount(
            target.name().clone(),
            MountDocument {
                target: target_address.clone(),
                mount_path: mount.mount_path.clone(),
                source,
                depends_on: Vec::new(),
                lifecycle: LifecycleDocument {
                    protect: Field::Set(true),
                    ..LifecycleDocument::default()
                },
            },
        )?;
    imported.resources.push(ImportedResource {
        address: target.clone(),
        state: resource_state_with_dependencies(
            target,
            mount.mount_id.as_str(),
            true,
            inputs,
            Some(environment_address),
            vec![target_address],
        )?,
    });

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

async fn import_target(
    client: &Dokploy,
    target: &ServiceTarget,
) -> Result<(ImportedWorkspace, ResourceAddress), ImportError> {
    match target {
        ServiceTarget::Application(id) => {
            let application = client.applications().get(id.clone()).await?;
            if application.application_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &application.environment_id).await?;
            let address = address(ResourceKind::Application, &application.name)?;
            let associations = imported_associations(client, &application).await?;
            Ok((
                build_application(project, environment, application, &associations, &address)?,
                address,
            ))
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
            let address = address(ResourceKind::Compose, &compose.name)?;
            Ok((
                build_compose(project, environment, compose, &address)?,
                address,
            ))
        }
        ServiceTarget::Postgres(id) => {
            let database = client.postgres().get(id.clone()).await?;
            if database.postgres_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let address = address(ResourceKind::Postgres, &database.name)?;
            Ok((
                build_postgres(project, environment, database, &address)?,
                address,
            ))
        }
        ServiceTarget::MySql(id) => {
            let database = client.mysql().get(id.clone()).await?;
            if database.mysql_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let address = address(ResourceKind::MySql, &database.name)?;
            Ok((
                build_mysql(project, environment, database, &address)?,
                address,
            ))
        }
        ServiceTarget::MariaDb(id) => {
            let database = client.mariadb().get(id.clone()).await?;
            if database.mariadb_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let address = address(ResourceKind::MariaDb, &database.name)?;
            Ok((
                build_mariadb(project, environment, database, &address)?,
                address,
            ))
        }
        ServiceTarget::Mongo(id) => {
            let database = client.mongo().get(id.clone()).await?;
            if database.mongo_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let address = address(ResourceKind::Mongo, &database.name)?;
            Ok((
                build_mongo(project, environment, database, &address)?,
                address,
            ))
        }
        ServiceTarget::LibSql(id) => {
            let database = client.libsql().get(id.clone()).await?;
            if database.libsql_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let address = address(ResourceKind::LibSql, &database.name)?;
            Ok((
                build_libsql(project, environment, database, &address)?,
                address,
            ))
        }
        ServiceTarget::Redis(id) => {
            let database = client.redis().get(id.clone()).await?;
            if database.redis_id != *id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let (environment, project) =
                environment_and_project(client, &database.environment_id).await?;
            let address = address(ResourceKind::Redis, &database.name)?;
            Ok((
                build_redis(project, environment, database, &address)?,
                address,
            ))
        }
    }
}
