//! Fresh, read-only Dokploy projections for declarative planning.

use std::collections::{BTreeMap, BTreeSet};

use dokploy_core::{
    ComparableValue, PropertyObservation, PropertyPath, PropertyUnknownReason, RemoteFailureKind,
    RemoteObservation, RemoteResource, RemoteState, RemoteStateError,
};
use dokploy_sdk::{Dokploy, Error as SdkError, ResponseField};
use dokploy_state::{InstanceIdentity, RemoteId, ResourceAddress, ResourceKind, StateFile};
use thiserror::Error;

use crate::desired::CompiledDesired;

/// Whether `project.all` is known to contain every project visible to reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectTopologyAuthority {
    /// Absence from the response proves that a project does not exist.
    Authoritative,
    /// Absence may be caused by permissions or another filtered projection.
    Partial,
}

/// A redaction-safe project projection failure.
#[derive(Debug, Error)]
pub enum DiscoverProjectsError {
    /// The configured client and durable state refer to different instances.
    #[error("DOKREM001: client and state instances differ")]
    InstanceMismatch,
    /// This project-only adapter received an address owned by another adapter.
    #[error("DOKREM002: project discovery does not support `{address}`")]
    UnsupportedResourceKind {
        /// The unsupported logical address.
        address: ResourceAddress,
    },
    /// More than one project has the same logical lookup name.
    #[error("DOKREM003: project topology contains a duplicate name")]
    DuplicateProjectName,
    /// More than one project has the same physical identity.
    #[error("DOKREM004: project topology contains a duplicate remote identity")]
    DuplicateProjectId,
    /// A project physical identity does not satisfy the state contract.
    #[error("DOKREM005: project topology contains an invalid remote identity")]
    InvalidProjectId,
    /// Core remote-state invariants rejected the projected observations.
    #[error("DOKREM006: project observations are ambiguous")]
    InvalidRemoteState(#[source] RemoteStateError),
}

/// Reads one fresh project topology and projects relevant logical addresses.
///
/// This adapter performs no mutations and does not retain a cache between
/// invocations.
pub async fn discover_projects(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    authority: ProjectTopologyAuthority,
) -> Result<RemoteState, DiscoverProjectsError> {
    let client_instance = InstanceIdentity::parse(client.base_url().as_str())
        .expect("the SDK exposes a normalized HTTP(S) URL without credentials");
    if &client_instance != state.instance() {
        return Err(DiscoverProjectsError::InstanceMismatch);
    }

    let desired = compiled.desired_state();
    let addresses: BTreeSet<_> = desired
        .resources()
        .keys()
        .chain(state.resources().keys())
        .chain(
            desired
                .moves()
                .iter()
                .flat_map(|directive| [directive.from(), directive.to()]),
        )
        .chain(
            desired
                .removals()
                .iter()
                .map(|directive| directive.address()),
        )
        .cloned()
        .collect();
    if let Some(address) = addresses
        .iter()
        .find(|address| address.kind() != ResourceKind::Project)
    {
        return Err(DiscoverProjectsError::UnsupportedResourceKind {
            address: address.clone(),
        });
    }
    let topology = client.projects().all().await;
    let failure = topology.as_ref().err().map(classify_sdk_error);
    let (projects_by_name, projects_by_id) = match &topology {
        Ok(topology) => index_projects(topology.projects())?,
        Err(_) => ProjectIndexes::default(),
    };

    let observations = addresses
        .iter()
        .map(|address| {
            let observation = if let Some(failure) = failure {
                RemoteObservation::Unavailable(failure)
            } else if let Some(project_match) =
                match_project(address, state, &projects_by_name, &projects_by_id)
            {
                let project = project_match.project();
                let mut properties = BTreeMap::new();
                if description_is_requested(address, compiled) {
                    let description = match &project.description {
                        ResponseField::NotReturned => {
                            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
                        }
                        ResponseField::Null => PropertyObservation::KnownAbsent,
                        ResponseField::Value(description) => PropertyObservation::Known(
                            ComparableValue::try_from_json(serde_json::json!(description))
                                .expect("a project description is comparable"),
                        ),
                    };
                    properties.insert(PropertyPath::Description, description);
                }
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new(project.project_id.as_str())
                        .expect("Dokploy project IDs are non-empty"),
                    properties,
                ))
            } else if authority == ProjectTopologyAuthority::Authoritative {
                RemoteObservation::Missing
            } else {
                RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
            };

            (address.clone(), observation)
        })
        .collect::<Vec<_>>();

    RemoteState::try_new(state.instance().clone(), observations)
        .map_err(DiscoverProjectsError::InvalidRemoteState)
}

type ProjectIndexes<'a> = (
    BTreeMap<&'a str, &'a dokploy_sdk::ProjectDetails>,
    BTreeMap<&'a str, &'a dokploy_sdk::ProjectDetails>,
);

enum ProjectMatch<'a> {
    ManagedIdentity(&'a dokploy_sdk::ProjectDetails),
    ManagedNameCollision(&'a dokploy_sdk::ProjectDetails),
    UnmanagedName(&'a dokploy_sdk::ProjectDetails),
}

impl<'a> ProjectMatch<'a> {
    fn project(self) -> &'a dokploy_sdk::ProjectDetails {
        match self {
            Self::ManagedIdentity(project)
            | Self::ManagedNameCollision(project)
            | Self::UnmanagedName(project) => project,
        }
    }
}

fn match_project<'a>(
    address: &ResourceAddress,
    state: &StateFile,
    projects_by_name: &BTreeMap<&str, &'a dokploy_sdk::ProjectDetails>,
    projects_by_id: &BTreeMap<&str, &'a dokploy_sdk::ProjectDetails>,
) -> Option<ProjectMatch<'a>> {
    let Some(stored) = state.resource(address) else {
        return projects_by_name
            .get(address.name().as_str())
            .copied()
            .map(ProjectMatch::UnmanagedName);
    };
    if let Some(project) = projects_by_id.get(stored.remote_id().as_str()) {
        return Some(ProjectMatch::ManagedIdentity(project));
    }

    // The managed identity disappeared, but its old logical name is occupied.
    // Expose that replacement to the planner so its different physical ID
    // produces an identity-mismatch diagnostic; it must never be adopted.
    projects_by_name
        .get(address.name().as_str())
        .copied()
        .map(ProjectMatch::ManagedNameCollision)
}

fn index_projects(
    projects: &[dokploy_sdk::ProjectDetails],
) -> Result<ProjectIndexes<'_>, DiscoverProjectsError> {
    let mut by_name = BTreeMap::new();
    let mut by_id = BTreeMap::new();

    for project in projects {
        RemoteId::new(project.project_id.as_str())
            .map_err(|_| DiscoverProjectsError::InvalidProjectId)?;
        if by_name.insert(project.name.as_str(), project).is_some() {
            return Err(DiscoverProjectsError::DuplicateProjectName);
        }
        if by_id.insert(project.project_id.as_str(), project).is_some() {
            return Err(DiscoverProjectsError::DuplicateProjectId);
        }
    }

    Ok((by_name, by_id))
}

fn description_is_requested(address: &ResourceAddress, compiled: &CompiledDesired) -> bool {
    let desired = compiled.desired_state();
    let desired_address = desired
        .moves()
        .iter()
        .find(|directive| directive.from() == address)
        .map_or(address, |directive| directive.to());
    let Some(resource) = desired.resources().get(desired_address) else {
        return false;
    };

    resource
        .properties()
        .contains_key(&PropertyPath::Description)
        && !resource
            .ignored_changes()
            .contains(&PropertyPath::Description)
}

fn classify_sdk_error(error: &SdkError) -> RemoteFailureKind {
    match error {
        SdkError::Api(error) if matches!(error.status(), 401 | 403) => {
            RemoteFailureKind::Unauthorized
        }
        SdkError::Api(error)
            if matches!(error.status(), 408 | 425 | 429) || error.status() >= 500 =>
        {
            RemoteFailureKind::Unavailable
        }
        SdkError::Request { .. } | SdkError::OutcomeUnknown { .. } => {
            RemoteFailureKind::Unavailable
        }
        SdkError::InvalidRequest { .. }
        | SdkError::Api(_)
        | SdkError::UnexpectedResponse { .. }
        | SdkError::Decode { .. } => RemoteFailureKind::InvalidResponse,
    }
}
