//! Document scope shared by every workspace command.
//!
//! A project document and a settings document can sit side by side, each with
//! its own durable state. Every command that plans, applies, recovers, or edits
//! state follows the scope of the document it was pointed at.

use dokploy_state::{InstanceIdentity, StateFile, StateScope};

use crate::desired::CompiledDesired;
use crate::remote::{
    DiscoverRemoteError, DiscoveryAuthority, TagTopologyAuthority, discover_remote,
    discover_settings_remote,
};

/// Starts an empty state lineage for one scope.
pub(crate) fn fresh_state(instance: InstanceIdentity, scope: StateScope) -> StateFile {
    StateFile::new_in_scope(
        env!("CARGO_PKG_VERSION")
            .parse()
            .expect("crate version is valid semver"),
        instance,
        scope,
    )
}

/// Reads fresh remote state for the resources of one scope.
pub(crate) async fn discover(
    client: &dokploy_sdk::Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<dokploy_core::RemoteState, DiscoverRemoteError> {
    match state.scope() {
        StateScope::Project => {
            discover_remote(
                client,
                compiled,
                state,
                DiscoveryAuthority::reconciliation(),
            )
            .await
        }
        StateScope::Settings => {
            discover_settings_remote(client, compiled, state, TagTopologyAuthority::Authoritative)
                .await
        }
    }
}
