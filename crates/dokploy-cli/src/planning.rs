//! Read-only composition of configuration, durable state, fresh discovery, and planning.

use std::fs;
use std::path::{Path, PathBuf};

use dokploy_core::{ConfigDigest, Plan, StoredState};
use dokploy_sdk::Dokploy;
use dokploy_state::{InstanceIdentity, RecoveryStatus, StateFile, StateStore};
use thiserror::Error;

use crate::desired::{CompileDesiredError, compile_desired_for_instance};
use crate::remote::DiscoverRemoteError;

/// A fresh plan paired with the remote evidence used to construct it.
pub struct PreparedPlan {
    plan: Plan,
    remote: dokploy_core::RemoteState,
    instance: InstanceIdentity,
}

impl PreparedPlan {
    /// Returns the deterministic public plan.
    #[must_use]
    pub const fn plan(&self) -> &Plan {
        &self.plan
    }

    /// Returns the fresh remote snapshot for keyed saved-plan binding.
    #[must_use]
    pub const fn remote(&self) -> &dokploy_core::RemoteState {
        &self.remote
    }

    /// Returns the normalized Dokploy instance used for planning.
    #[must_use]
    pub const fn instance(&self) -> &InstanceIdentity {
        &self.instance
    }

    /// Consumes the evidence wrapper and returns the public plan.
    #[must_use]
    pub fn into_plan(self) -> Plan {
        self.plan
    }
}

/// Builds one fresh, mutation-free plan for a configuration workspace.
pub async fn plan_workspace(
    client: &Dokploy,
    config_file: &Path,
) -> Result<Plan, PlanWorkspaceError> {
    Ok(prepare_workspace(client, config_file).await?.into_plan())
}

/// Builds one fresh plan while retaining its in-memory remote binding evidence.
pub async fn prepare_workspace(
    client: &Dokploy,
    config_file: &Path,
) -> Result<PreparedPlan, PlanWorkspaceError> {
    let loaded = dokploy_config::load_with_digest(config_file)?;
    let workspace = canonical_workspace(config_file)?;
    let instance = InstanceIdentity::parse(client.base_url().as_str())?;
    let store = StateStore::with_scope(&workspace, instance.clone(), loaded.config.scope())?;

    ensure_recovery_clean(&store)?;
    let before = store.inspect()?;
    let source_digest = ConfigDigest::parse(hex_digest(loaded.source_sha256))
        .expect("a SHA-256 digest is canonical lowercase hexadecimal");
    let compiled =
        compile_desired_for_instance(&loaded.config, source_digest, instance.clone(), &workspace)?;
    let stored = match before.as_ref() {
        Some(state) => StoredState::try_from_state(state)?,
        None => StoredState::absent(instance.clone()),
    };
    let discovery_state = before
        .clone()
        .unwrap_or_else(|| crate::scope::fresh_state(instance.clone(), store.scope()));
    let remote = crate::scope::discover(client, &compiled, &discovery_state).await?;

    ensure_recovery_clean(&store)?;
    let after = store.inspect()?;
    if state_revision(&before) != state_revision(&after) {
        return Err(PlanWorkspaceError::StateChanged);
    }

    let plan = dokploy_core::plan(compiled.desired_state(), &stored, &remote);

    Ok(PreparedPlan {
        plan,
        remote,
        instance,
    })
}

fn canonical_workspace(config_file: &Path) -> Result<PathBuf, PlanWorkspaceError> {
    let parent = config_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::canonicalize(parent).map_err(|source| PlanWorkspaceError::Workspace { source })
}

fn ensure_recovery_clean(store: &StateStore) -> Result<(), PlanWorkspaceError> {
    match store.recovery_status()? {
        RecoveryStatus::Clean => Ok(()),
        RecoveryStatus::RecoveryRequired(_) => Err(PlanWorkspaceError::RecoveryRequired),
    }
}

fn state_revision(state: &Option<StateFile>) -> Option<dokploy_state::StateRevision> {
    state.as_ref().map(StateFile::revision)
}

fn hex_digest(bytes: [u8; 32]) -> String {
    use std::fmt::Write;

    bytes
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a string cannot fail");
            hex
        })
}

/// A redaction-safe failure while composing a read-only plan.
#[derive(Debug, Error)]
pub enum PlanWorkspaceError {
    #[error("failed to load the declarative configuration")]
    Config(#[from] dokploy_config::ConfigFileError),
    #[error("failed to resolve the configuration workspace")]
    Workspace {
        #[source]
        source: std::io::Error,
    },
    #[error("the Dokploy client URL is not a valid instance identity")]
    Instance(#[from] dokploy_state::InstanceIdentityError),
    #[error("failed to inspect durable workspace state")]
    StateStore(#[from] dokploy_state::StateStoreError),
    #[error("durable state cannot be projected into planner input")]
    StoredState(#[from] dokploy_core::StoredStateError),
    #[error("desired configuration cannot be compiled safely")]
    Desired(#[from] CompileDesiredError),
    #[error("fresh Dokploy state cannot be discovered safely")]
    Remote(#[from] DiscoverRemoteError),
    #[error("workspace recovery is required before planning")]
    RecoveryRequired,
    #[error("durable state changed while the plan was being built")]
    StateChanged,
}
