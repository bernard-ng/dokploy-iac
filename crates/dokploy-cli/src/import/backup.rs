//! Protected import of one existing database Backup with its target ancestry.
//!
//! The import is read-only. The Backup's typed database target, its
//! environment, and the project are adopted into the same new workspace so the
//! Backup's containment and target dependency are written correctly. The
//! destination is written as an exact name selector and never as a physical
//! identity; an unknown, unreadable, shared, or malformed destination name fails
//! closed. The Backup is imported protected so it can neither be replaced nor
//! destroyed by the first plan.

use std::collections::BTreeSet;

use dokploy_config::{BackupDocument, ExternalSelector, SelectorKind};
use dokploy_sdk::{BackupTarget, ServiceTarget};

use super::mount::import_target;
use super::*;

pub(super) async fn discover_backup(
    client: &Dokploy,
    remote_id: &str,
    target: &ResourceAddress,
) -> Result<ImportedWorkspace, ImportError> {
    let requested_id = dokploy_sdk::BackupId::new(remote_id);
    let backup = client.backups().get(requested_id.clone()).await?;
    if backup.backup_id != requested_id {
        return Err(ImportError::InvalidRemoteTopology);
    }
    let collection = client.backups().by_target(backup.target.clone()).await?;
    let matching = collection
        .backups()
        .iter()
        .filter(|candidate| candidate.backup_id == backup.backup_id)
        .collect::<Vec<_>>();
    if matching.as_slice() != [&backup] {
        return Err(ImportError::InvalidRemoteTopology);
    }
    // A null flag cannot be written as a boolean, so it is never guessed.
    let enabled = backup.enabled.ok_or(ImportError::InvalidRemoteTopology)?;

    let directory =
        ExternalDirectory::load(client, &BTreeSet::from([SelectorKind::Destination])).await;
    let destination = directory
        .unique_name_of(SelectorKind::Destination, backup.destination_id.as_str())
        .map(str::to_owned)
        .ok_or(ImportError::ExternalAssociation)?;

    let (mut imported, target_address) =
        import_target(client, &service_target(&backup.target)).await?;
    let environment_address = imported
        .resources
        .iter()
        .find(|item| item.address.kind() == ResourceKind::Environment)
        .map(|item| item.address.clone())
        .ok_or(ImportError::MissingContainment)?;
    let keep_latest = backup.keep_latest_count.map(std::num::NonZeroU32::get);
    let inputs = serde_json::json!({
        "target": target_address.to_string(),
        "destination": { "name": destination },
        "schedule": backup.schedule,
        "prefix": backup.prefix,
        "database": backup.database,
        "enabled": enabled,
        "keep_latest": keep_latest,
        "include_encryption_key": backup.include_encryption_key,
    });
    let serde_json::Value::Object(inputs) = inputs else {
        unreachable!("the managed inputs are a JSON object");
    };

    imported
        .document
        .environment_mut(environment_address.name())
        .ok_or(ImportError::MissingContainment)?
        .add_backup(
            target.name().clone(),
            BackupDocument {
                target: target_address.clone(),
                destination: ExternalSelector::named(destination),
                schedule: backup.schedule.clone(),
                prefix: backup.prefix.clone(),
                database: backup.database.clone(),
                enabled,
                keep_latest: keep_latest.map_or(Field::Clear, Field::Set),
                include_encryption_key: backup.include_encryption_key,
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
            backup.backup_id.as_str(),
            true,
            inputs,
            Some(environment_address),
            vec![target_address],
        )?,
    });

    Ok(imported)
}

fn service_target(target: &BackupTarget) -> ServiceTarget {
    match target {
        BackupTarget::Postgres(id) => ServiceTarget::Postgres(id.clone()),
        BackupTarget::MySql(id) => ServiceTarget::MySql(id.clone()),
        BackupTarget::MariaDb(id) => ServiceTarget::MariaDb(id.clone()),
        BackupTarget::Mongo(id) => ServiceTarget::Mongo(id.clone()),
        BackupTarget::LibSql(id) => ServiceTarget::LibSql(id.clone()),
    }
}
