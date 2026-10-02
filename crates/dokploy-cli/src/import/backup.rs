//! Protected import of one existing database Backup.
//!
//! The destination is written as an exact name selector and never as a physical
//! identity; an unknown, unreadable, shared, or malformed destination name fails
//! closed. The Backup is imported protected so it can neither be replaced nor
//! destroyed by the first plan.

use dokploy_config::BackupDocument;

use super::context::{EnvScope, ImportContext};
use super::*;

impl ImportContext {
    pub(super) fn add_backup(
        &mut self,
        env: &EnvScope,
        service: &ResourceAddress,
        backup: &dokploy_sdk::BackupDetails,
        destination: String,
        enabled: bool,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        let keep_latest = backup.keep_latest_count.map(std::num::NonZeroU32::get);
        let inputs = serde_json::json!({
            "target": service.to_string(),
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
        self.environment(env)?.add_backup(
            address.name().clone(),
            BackupDocument {
                target: service.clone(),
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
        self.push(
            address,
            backup.backup_id.as_str(),
            true,
            inputs,
            env.address(),
            vec![service.clone()],
        )
    }
}
