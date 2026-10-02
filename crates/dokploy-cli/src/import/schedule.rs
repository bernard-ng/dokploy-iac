//! Protected import of one existing Schedule.
//!
//! The Schedule's typed target is an application or one Compose service.
//! Executable command and script text is never readable, so both are left
//! unmanaged, and the Schedule is imported protected so it can neither be
//! replaced nor destroyed by the first plan. Importing never runs a Schedule.

use dokploy_config::{ScheduleDocument, ScheduleShellConfig};
use dokploy_sdk::ScheduleDetails;

use super::context::{EnvScope, ImportContext};
use super::*;

impl ImportContext {
    pub(super) fn add_schedule(
        &mut self,
        env: &EnvScope,
        service: &ResourceAddress,
        schedule: &ScheduleDetails,
        service_name: Option<String>,
        shell_type: ScheduleShellConfig,
        address: &ResourceAddress,
    ) -> Result<(), ImportError> {
        let (inputs, description, timezone) =
            imported_inputs(schedule, service, service_name.as_deref(), shell_type);
        self.environment(env)?.add_schedule(
            address.name().clone(),
            ScheduleDocument {
                name: schedule.name.clone(),
                target: service.clone(),
                service_name,
                cron_expression: schedule.cron_expression.clone(),
                shell_type,
                enabled: schedule.enabled,
                description,
                timezone,
                command: Field::Unmanaged,
                script: Field::Unmanaged,
                depends_on: Vec::new(),
                lifecycle: LifecycleDocument {
                    protect: Field::Set(true),
                    ..LifecycleDocument::default()
                },
            },
        )?;
        self.push(
            address,
            schedule.schedule_id.as_str(),
            true,
            inputs,
            env.address(),
            vec![service.clone()],
        )
    }
}

/// Builds the non-sensitive managed inputs and the configuration fields they mirror.
///
/// Description and timezone are owned only when the remote returns them, so the
/// first fresh plan converges for both present and absent values.
fn imported_inputs(
    schedule: &ScheduleDetails,
    target_address: &ResourceAddress,
    service_name: Option<&str>,
    shell_type: ScheduleShellConfig,
) -> (
    serde_json::Map<String, serde_json::Value>,
    Field<String>,
    Field<String>,
) {
    let mut inputs = serde_json::Map::new();
    inputs.insert(
        "target".to_owned(),
        serde_json::json!(target_address.to_string()),
    );
    if let Some(service_name) = service_name {
        inputs.insert("service_name".to_owned(), serde_json::json!(service_name));
    }
    inputs.insert("name".to_owned(), serde_json::json!(schedule.name));
    inputs.insert(
        "cron_expression".to_owned(),
        serde_json::json!(schedule.cron_expression),
    );
    inputs.insert(
        "shell_type".to_owned(),
        serde_json::json!(shell_type.as_str()),
    );
    inputs.insert("enabled".to_owned(), serde_json::json!(schedule.enabled));
    let description = match &schedule.description {
        Some(description) => {
            inputs.insert("description".to_owned(), serde_json::json!(description));
            Field::Set(description.clone())
        }
        None => Field::Unmanaged,
    };
    let timezone = match &schedule.timezone {
        Some(timezone) => {
            inputs.insert("timezone".to_owned(), serde_json::json!(timezone));
            Field::Set(timezone.clone())
        }
        None => Field::Unmanaged,
    };

    (inputs, description, timezone)
}
