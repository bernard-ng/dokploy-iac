//! Redaction-safe inspection of one workspace's durable state.

use std::io::{self, Write};
use std::path::Path;

use dokploy_state::{InstanceIdentity, ResourceAddress, StateStore, StateStoreError};
use thiserror::Error;

use crate::cli::StateCommand;

/// Executes one read-only state command without contacting Dokploy.
pub fn execute(
    config_file: &Path,
    instance: InstanceIdentity,
    command: StateCommand,
    output: &mut dyn Write,
) -> Result<(), StateCommandError> {
    let workspace = config_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let workspace = std::fs::canonicalize(workspace)
        .map_err(|source| StateCommandError::Workspace { source })?;
    let state = StateStore::new(&workspace, instance)?
        .inspect()?
        .ok_or(StateCommandError::Missing)?;

    match command {
        StateCommand::List => {
            for address in state.resources().keys() {
                writeln!(output, "{address}")?;
            }
        }
        StateCommand::Show { address } => {
            let address = address
                .parse::<ResourceAddress>()
                .map_err(|_| StateCommandError::InvalidAddress)?;
            let resource = state
                .resource(&address)
                .ok_or(StateCommandError::UnknownAddress)?;
            writeln!(output, "address: {address}")?;
            writeln!(output, "kind: {}", resource.kind())?;
            writeln!(output, "remote id: {}", resource.remote_id())?;
            writeln!(output, "protected: {}", resource.is_protected())?;
            match resource.containment() {
                Some(parent) => writeln!(output, "containment: {parent}")?,
                None => writeln!(output, "containment: none")?,
            }
            writeln!(output, "dependencies:")?;
            for dependency in resource.dependencies() {
                writeln!(output, "  - {dependency}")?;
            }
            writeln!(output, "managed fields:")?;
            let fields = resource
                .last_applied()
                .as_json()
                .as_object()
                .expect("managed inputs guarantee an object root");
            for field in fields.keys() {
                writeln!(output, "  - {field}")?;
            }
            writeln!(output, "sensitive fields:")?;
            for path in resource.sensitive_inputs().paths() {
                writeln!(output, "  - {path}")?;
            }
        }
    }

    Ok(())
}

/// A safe state inspection failure.
#[derive(Debug, Error)]
pub enum StateCommandError {
    #[error("failed to resolve the configuration workspace")]
    Workspace {
        #[source]
        source: io::Error,
    },
    #[error("failed to access durable workspace state")]
    Store(#[from] StateStoreError),
    #[error("the workspace has no durable state")]
    Missing,
    #[error("the logical resource address is invalid")]
    InvalidAddress,
    #[error("the logical resource address is not tracked")]
    UnknownAddress,
    #[error("failed to render durable state")]
    Output(#[from] io::Error),
}
