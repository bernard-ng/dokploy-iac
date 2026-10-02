//! Protected import of one existing Mount.
//!
//! File content is write-only upstream and is therefore left unmanaged; the
//! Mount is imported protected so it can neither be replaced nor destroyed by
//! the first plan.

use dokploy_config::{MountDocument, MountSourceConfig};
use dokploy_sdk::{MountDetails, MountType};

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

pub(super) fn imported_source(
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
