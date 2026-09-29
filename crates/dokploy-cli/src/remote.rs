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

/// Whether a parent-scoped environment collection is known to be complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentTopologyAuthority {
    /// Absence from a proven-present project's collection proves nonexistence.
    Authoritative,
    /// Absence may be caused by role-dependent filtering.
    Partial,
}

/// Visibility assertions required by combined project and environment discovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryAuthority {
    /// Completeness of `project.all`.
    pub projects: ProjectTopologyAuthority,
    /// Completeness of each `environment.byProjectId` collection.
    pub environments: EnvironmentTopologyAuthority,
}

/// A redaction-safe combined discovery failure.
#[derive(Debug, Error)]
pub enum DiscoverRemoteError {
    /// The configured client and durable state refer to different instances.
    #[error("DOKREM001: client and state instances differ")]
    InstanceMismatch,
    /// Project topology could not be projected safely.
    #[error(transparent)]
    Projects(#[from] DiscoverProjectsError),
    /// An environment has no unambiguous containing project.
    #[error("DOKREM007: environment containment is unavailable")]
    EnvironmentContainment,
    /// An environment physical identity does not satisfy the state contract.
    #[error("DOKREM008: environment topology contains an invalid remote identity")]
    InvalidEnvironmentId,
    /// More than one environment has the same name within one project.
    #[error("DOKREM009: environment topology contains a duplicate scoped name")]
    DuplicateEnvironmentName,
    /// More than one environment has the same physical identity.
    #[error("DOKREM010: environment topology contains a duplicate remote identity")]
    DuplicateEnvironmentId,
    /// Core remote-state invariants rejected the combined observations.
    #[error("DOKREM011: combined remote observations are ambiguous")]
    InvalidRemoteState(#[source] RemoteStateError),
}

/// A redaction-safe project projection failure.
#[derive(Debug, Error)]
pub enum DiscoverProjectsError {
    /// The configured client and durable state refer to different instances.
    #[error("DOKREM001: client and state instances differ")]
    InstanceMismatch,
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
    if !instances_match(client, state) {
        return Err(DiscoverProjectsError::InstanceMismatch);
    }

    let observations = discover_project_observations(client, compiled, state, authority).await?;

    RemoteState::try_new(state.instance().clone(), observations)
        .map_err(DiscoverProjectsError::InvalidRemoteState)
}

/// Reads fresh project and environment state into one planner snapshot.
///
/// This is the public discovery seam for the current project-and-environment
/// checkpoint. Every invocation performs fresh reads and retains no cache.
pub async fn discover_remote(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    authority: DiscoveryAuthority,
) -> Result<RemoteState, DiscoverRemoteError> {
    if !instances_match(client, state) {
        return Err(DiscoverRemoteError::InstanceMismatch);
    }

    let mut observations =
        discover_project_observations(client, compiled, state, authority.projects).await?;
    let environments = discover_environment_observations(
        client,
        compiled,
        state,
        &observations,
        authority.environments,
    )
    .await?;
    observations.extend(environments);

    RemoteState::try_new(state.instance().clone(), observations)
        .map_err(DiscoverRemoteError::InvalidRemoteState)
}

fn instances_match(client: &Dokploy, state: &StateFile) -> bool {
    let client_instance = InstanceIdentity::parse(client.base_url().as_str())
        .expect("the SDK exposes a normalized HTTP(S) URL without credentials");

    &client_instance == state.instance()
}

async fn discover_project_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    authority: ProjectTopologyAuthority,
) -> Result<Vec<(ResourceAddress, RemoteObservation)>, DiscoverProjectsError> {
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
        .filter(|address| address.kind() == ResourceKind::Project)
        .cloned()
        .collect();
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

    Ok(observations)
}

async fn discover_environment_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    projects: &[(ResourceAddress, RemoteObservation)],
    authority: EnvironmentTopologyAuthority,
) -> Result<Vec<(ResourceAddress, RemoteObservation)>, DiscoverRemoteError> {
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
        .filter(|address| address.kind() == ResourceKind::Environment)
        .cloned()
        .collect();
    let mut collections = BTreeMap::new();

    for address in &addresses {
        let parent = environment_parent(address, compiled, state)?;
        let Some(project_id) = observed_project_id(&parent, compiled, state, projects) else {
            continue;
        };
        if collections.contains_key(&project_id) {
            continue;
        }
        let collection = client
            .environments()
            .by_project(dokploy_sdk::ProjectId::new(&project_id))
            .await;
        collections.insert(project_id, collection);
    }
    validate_environment_collections(&collections)?;

    let mut seen_ids = BTreeSet::new();
    let mut observations = Vec::new();
    for address in addresses {
        let observation = if let Some(stored) = state.resource(&address) {
            match client
                .environments()
                .get(dokploy_sdk::EnvironmentId::new(stored.remote_id().as_str()))
                .await
            {
                Ok(environment) => {
                    let remote_id = RemoteId::new(environment.environment_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidEnvironmentId)?;
                    if remote_id != *stored.remote_id() {
                        return Err(DiscoverRemoteError::InvalidEnvironmentId);
                    }
                    let expected_parent = environment_parent(&address, compiled, state)?;
                    let expected_parent_id =
                        effective_project_id(&expected_parent, compiled, state, projects)?;
                    if environment.project_id.as_str() != expected_parent_id {
                        return Err(DiscoverRemoteError::EnvironmentContainment);
                    }
                    if !seen_ids.insert(remote_id.clone()) {
                        return Err(DiscoverRemoteError::DuplicateEnvironmentId);
                    }
                    RemoteObservation::Present(RemoteResource::new(
                        remote_id,
                        environment_properties(&address, compiled, &environment.description),
                    ))
                }
                Err(SdkError::Api(error)) if error.status() == 404 => {
                    let parent = environment_parent(&address, compiled, state)?;
                    match observed_project_id(&parent, compiled, state, projects)
                        .and_then(|project_id| collections.get(&project_id))
                    {
                        Some(Ok(collection)) => observe_environment_collection(
                            &address,
                            compiled,
                            collection,
                            authority,
                            &mut seen_ids,
                        )?,
                        Some(Err(error)) => {
                            RemoteObservation::Unavailable(classify_sdk_error(error))
                        }
                        None => match effective_project_observation(&parent, compiled, projects) {
                            Some(RemoteObservation::Missing) => RemoteObservation::Missing,
                            Some(RemoteObservation::Present(_))
                            | Some(RemoteObservation::Unavailable(_))
                            | None => {
                                RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
                            }
                        },
                    }
                }
                Err(error) => RemoteObservation::Unavailable(classify_sdk_error(&error)),
            }
        } else {
            let parent = environment_parent(&address, compiled, state)?;
            match effective_project_observation(&parent, compiled, projects) {
                Some(RemoteObservation::Missing) => RemoteObservation::Missing,
                Some(RemoteObservation::Unavailable(failure)) => {
                    RemoteObservation::Unavailable(*failure)
                }
                Some(RemoteObservation::Present(project)) => {
                    let project_id = project.remote_id().as_str();
                    match collections.get(project_id) {
                        Some(Ok(collection)) => observe_environment_collection(
                            &address,
                            compiled,
                            collection,
                            authority,
                            &mut seen_ids,
                        )?,
                        Some(Err(error)) => {
                            RemoteObservation::Unavailable(classify_sdk_error(error))
                        }
                        None => RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse),
                    }
                }
                None => RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse),
            }
        };
        observations.push((address, observation));
    }

    Ok(observations)
}

fn validate_environment_collections(
    collections: &BTreeMap<String, Result<dokploy_sdk::EnvironmentCollection, dokploy_sdk::Error>>,
) -> Result<(), DiscoverRemoteError> {
    let mut global_ids = BTreeSet::new();
    for collection in collections
        .values()
        .filter_map(|result| result.as_ref().ok())
    {
        let mut scoped_names = BTreeSet::new();
        for environment in collection.environments() {
            let remote_id = RemoteId::new(environment.environment_id.as_str())
                .map_err(|_| DiscoverRemoteError::InvalidEnvironmentId)?;
            if !global_ids.insert(remote_id) {
                return Err(DiscoverRemoteError::DuplicateEnvironmentId);
            }
            if !scoped_names.insert(environment.name.as_str()) {
                return Err(DiscoverRemoteError::DuplicateEnvironmentName);
            }
        }
    }

    Ok(())
}

fn observe_environment_collection(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    collection: &dokploy_sdk::EnvironmentCollection,
    authority: EnvironmentTopologyAuthority,
    seen_ids: &mut BTreeSet<RemoteId>,
) -> Result<RemoteObservation, DiscoverRemoteError> {
    let mut by_name = BTreeMap::new();
    let mut collection_ids = BTreeSet::new();
    for environment in collection.environments() {
        let remote_id = RemoteId::new(environment.environment_id.as_str())
            .map_err(|_| DiscoverRemoteError::InvalidEnvironmentId)?;
        if !collection_ids.insert(remote_id) {
            return Err(DiscoverRemoteError::DuplicateEnvironmentId);
        }
        if by_name
            .insert(environment.name.as_str(), environment)
            .is_some()
        {
            return Err(DiscoverRemoteError::DuplicateEnvironmentName);
        }
    }
    if let Some(environment) = by_name.get(address.name().as_str()) {
        let remote_id = RemoteId::new(environment.environment_id.as_str())
            .map_err(|_| DiscoverRemoteError::InvalidEnvironmentId)?;
        if !seen_ids.insert(remote_id.clone()) {
            return Err(DiscoverRemoteError::DuplicateEnvironmentId);
        }
        return Ok(RemoteObservation::Present(RemoteResource::new(
            remote_id,
            environment_properties(address, compiled, &environment.description),
        )));
    }
    if authority == EnvironmentTopologyAuthority::Authoritative {
        Ok(RemoteObservation::Missing)
    } else {
        Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        ))
    }
}

fn observed_project_id(
    parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    projects: &[(ResourceAddress, RemoteObservation)],
) -> Option<String> {
    let effective = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == parent)
        .map_or(parent, |directive| directive.from());

    match project_observation(projects, effective) {
        Some(RemoteObservation::Present(project))
            if state
                .resource(effective)
                .is_none_or(|stored| stored.remote_id() == project.remote_id()) =>
        {
            Some(project.remote_id().as_str().to_owned())
        }
        Some(RemoteObservation::Missing | RemoteObservation::Unavailable(_)) | None => None,
        Some(RemoteObservation::Present(_)) => None,
    }
}

fn project_observation<'a>(
    projects: &'a [(ResourceAddress, RemoteObservation)],
    address: &ResourceAddress,
) -> Option<&'a RemoteObservation> {
    projects
        .iter()
        .find_map(|(candidate, observation)| (candidate == address).then_some(observation))
}

fn effective_project_observation<'a>(
    parent: &ResourceAddress,
    compiled: &CompiledDesired,
    projects: &'a [(ResourceAddress, RemoteObservation)],
) -> Option<&'a RemoteObservation> {
    let effective = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == parent)
        .map_or(parent, |directive| directive.from());

    project_observation(projects, effective)
}

fn environment_parent(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    if let Some(parent) = compiled.bindings().parent_of(address) {
        return Ok(parent.clone());
    }
    let Some(resource) = state.resource(address) else {
        return Err(DiscoverRemoteError::EnvironmentContainment);
    };
    let mut parents = resource
        .dependencies()
        .iter()
        .filter(|dependency| dependency.kind() == ResourceKind::Project);
    let parent = parents
        .next()
        .ok_or(DiscoverRemoteError::EnvironmentContainment)?;
    if parents.next().is_some() {
        return Err(DiscoverRemoteError::EnvironmentContainment);
    }

    Ok(parent.clone())
}

fn effective_project_id<'a>(
    parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &'a StateFile,
    projects: &'a [(ResourceAddress, RemoteObservation)],
) -> Result<&'a str, DiscoverRemoteError> {
    let effective = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == parent)
        .map_or(parent, |directive| directive.from());
    match project_observation(projects, effective) {
        Some(RemoteObservation::Present(project)) => {
            if let Some(stored) = state.resource(effective)
                && stored.remote_id().as_str() != project.remote_id().as_str()
            {
                return Err(DiscoverRemoteError::EnvironmentContainment);
            }
            Ok(project.remote_id().as_str())
        }
        Some(RemoteObservation::Missing | RemoteObservation::Unavailable(_)) | None => {
            Err(DiscoverRemoteError::EnvironmentContainment)
        }
    }
}

fn environment_properties<T>(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    description: &ResponseField<T>,
) -> BTreeMap<PropertyPath, PropertyObservation>
where
    T: AsRef<str>,
{
    let mut properties = BTreeMap::new();
    if description_is_requested(address, compiled) {
        let observation = match description {
            ResponseField::NotReturned => {
                PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
            }
            ResponseField::Null => PropertyObservation::KnownAbsent,
            ResponseField::Value(value) => PropertyObservation::Known(
                ComparableValue::try_from_json(serde_json::json!(value.as_ref()))
                    .expect("an environment description is comparable"),
            ),
        };
        properties.insert(PropertyPath::Description, observation);
    }
    properties
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
