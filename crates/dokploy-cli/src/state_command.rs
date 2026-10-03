//! Redaction-safe inspection and explicit repair of one workspace's durable state.

use dokploy_state::{
    DocumentId, ExpectedState, InstanceIdentity, ResourceAddress, StateError, StateFile,
    StateStore, StateStoreError,
};
use std::io::{self, Write};
use thiserror::Error;

use crate::cli::StateCommand;
use crate::workspace::Workspace;

/// Executes one local state command without contacting Dokploy.
pub fn execute(
    workspace: &Workspace,
    document: DocumentId,
    instance: InstanceIdentity,
    command: StateCommand,
    output: &mut dyn Write,
) -> Result<(), StateCommandError> {
    let store = StateStore::for_document(&workspace.directory, instance, document)?;
    let state = store.inspect()?.ok_or(StateCommandError::Missing)?;

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
        StateCommand::Mv { source, target } => {
            let source = parse_address(&source)?;
            let target = parse_address(&target)?;
            mutate(&store, state, |state| {
                state.move_resource(&source, target.clone())
            })?;
            writeln!(
                output,
                "Moved state from {source} to {target}. Dokploy was not changed."
            )?;
        }
        StateCommand::Rm { address } => {
            let address = parse_address(&address)?;
            if state.resource(&address).is_none() {
                return Err(StateCommandError::UnknownAddress);
            }
            mutate(&store, state, |state| {
                state.forget_resource(&address).map(|_| ())
            })?;
            writeln!(
                output,
                "Removed {address} from local state. Dokploy was not changed."
            )?;
        }
        StateCommand::Protect { address } => {
            set_protection(&store, state, &address, true, output)?;
        }
        StateCommand::Unprotect { address } => {
            set_protection(&store, state, &address, false, output)?;
        }
    }

    Ok(())
}

fn parse_address(value: &str) -> Result<ResourceAddress, StateCommandError> {
    value
        .parse::<ResourceAddress>()
        .map_err(|_| StateCommandError::InvalidAddress)
}

fn mutate(
    store: &StateStore,
    current: StateFile,
    operation: impl FnOnce(&mut StateFile) -> Result<(), StateError>,
) -> Result<(), StateCommandError> {
    let expected = ExpectedState::from_state(&current);
    let mut proposed = current;
    operation(&mut proposed)?;
    store.begin_write()?.checkpoint(expected, &proposed)?;

    Ok(())
}

fn set_protection(
    store: &StateStore,
    state: StateFile,
    raw_address: &str,
    protected: bool,
    output: &mut dyn Write,
) -> Result<(), StateCommandError> {
    let address = parse_address(raw_address)?;
    if state.resource(&address).is_none() {
        return Err(StateCommandError::UnknownAddress);
    }
    let expected = ExpectedState::from_state(&state);
    let mut proposed = state;
    let changed = proposed.set_resource_protection(&address, protected)?;
    if changed {
        store.begin_write()?.checkpoint(expected, &proposed)?;
    }
    let status = if protected {
        "protected"
    } else {
        "unprotected"
    };
    writeln!(output, "Resource {address} is {status}.")?;

    Ok(())
}

/// A safe state inspection failure.
#[derive(Debug, Error)]
pub enum StateCommandError {
    #[error("failed to access durable workspace state")]
    Store(#[from] StateStoreError),
    #[error("durable state mutation is invalid")]
    State(#[from] StateError),
    #[error("the workspace has no durable state")]
    Missing,
    #[error("the logical resource address is invalid")]
    InvalidAddress,
    #[error("the logical resource address is not tracked")]
    UnknownAddress,
    #[error("failed to render durable state")]
    Output(#[from] io::Error),
}
