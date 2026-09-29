//! Fresh, read-only Dokploy projections for declarative planning.

use std::collections::{BTreeMap, BTreeSet};

use dokploy_core::{
    ComparableValue, PropertyObservation, PropertyPath, PropertyUnknownReason, RemoteFailureKind,
    RemoteObservation, RemoteResource, RemoteState, RemoteStateError,
};
use dokploy_sdk::{ApplicationEnvironmentShape, Dokploy, Error as SdkError, ResponseField};
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

/// Whether a fully paginated parent-scoped application search is complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationTopologyAuthority {
    /// Exhaustive absence below a proven environment proves nonexistence.
    Authoritative,
    /// Absence may be caused by role-dependent filtering.
    Partial,
}

/// Visibility assertions required by combined project, environment, and application discovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryAuthority {
    /// Completeness of `project.all`.
    pub projects: ProjectTopologyAuthority,
    /// Completeness of each `environment.byProjectId` collection.
    pub environments: EnvironmentTopologyAuthority,
    /// Completeness of each fully paginated `application.search` collection.
    pub applications: ApplicationTopologyAuthority,
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
    /// An application has no unambiguous containing environment.
    #[error("DOKREM012: application containment is unavailable")]
    ApplicationContainment,
    /// An application physical identity does not satisfy the state contract.
    #[error("DOKREM013: application topology contains an invalid remote identity")]
    InvalidApplicationId,
    /// More than one application has the same name within one environment.
    #[error("DOKREM014: application topology contains a duplicate scoped name")]
    DuplicateApplicationName,
    /// More than one application has the same physical identity.
    #[error("DOKREM015: application topology contains a duplicate remote identity")]
    DuplicateApplicationId,
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

/// Reads fresh project, environment, and application state into one planner snapshot.
///
/// This is the public discovery seam for the current remote-projection
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
    let applications = discover_application_observations(
        client,
        compiled,
        state,
        &observations,
        authority.applications,
    )
    .await?;
    observations.extend(applications);

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
                .removals()
                .iter()
                .filter(|directive| state.resource(directive.address()).is_some())
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
                .removals()
                .iter()
                .filter(|directive| state.resource(directive.address()).is_some())
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

async fn discover_application_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    authority: ApplicationTopologyAuthority,
) -> Result<Vec<(ResourceAddress, RemoteObservation)>, DiscoverRemoteError> {
    let desired = compiled.desired_state();
    let addresses: BTreeSet<_> = desired
        .resources()
        .keys()
        .chain(state.resources().keys())
        .chain(
            desired
                .removals()
                .iter()
                .filter(|directive| state.resource(directive.address()).is_some())
                .map(|directive| directive.address()),
        )
        .filter(|address| address.kind() == ResourceKind::Application)
        .cloned()
        .collect();
    let mut collections = BTreeMap::new();

    for address in &addresses {
        let mut parents = vec![application_parent_from_desired(address, compiled, state)?];
        if state.resource(address).is_some() {
            parents.push(application_parent_from_state(address, state)?);
        }
        parents.sort();
        parents.dedup();
        for parent in parents {
            let Some(environment_id) = trusted_environment_id(&parent, compiled, state, topology)
            else {
                continue;
            };
            if collections.contains_key(&environment_id) {
                continue;
            }
            let collection = client
                .applications()
                .by_environment(dokploy_sdk::EnvironmentId::new(&environment_id))
                .await;
            collections.insert(environment_id, collection);
        }
    }
    validate_application_collections(&collections)?;

    let mut seen_direct_ids = BTreeSet::new();
    let mut observations = Vec::new();
    for address in addresses {
        let observation = if let Some(stored) = state.resource(&address) {
            match client
                .applications()
                .get(dokploy_sdk::ApplicationId::new(stored.remote_id().as_str()))
                .await
            {
                Ok(application) => {
                    let remote_id = RemoteId::new(application.application_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidApplicationId)?;
                    if remote_id != *stored.remote_id() {
                        return Err(DiscoverRemoteError::InvalidApplicationId);
                    }
                    let current_parent = application_parent_from_state(&address, state)?;
                    let expected_environment_id =
                        trusted_environment_id(&current_parent, compiled, state, topology)
                            .ok_or(DiscoverRemoteError::ApplicationContainment)?;
                    if application.environment_id.as_str() != expected_environment_id {
                        return Err(DiscoverRemoteError::ApplicationContainment);
                    }
                    if !seen_direct_ids.insert(remote_id.clone()) {
                        return Err(DiscoverRemoteError::DuplicateApplicationId);
                    }
                    if let Some(collision) = observe_reparent_target(
                        &address,
                        &current_parent,
                        &remote_id,
                        compiled,
                        state,
                        topology,
                        &collections,
                        authority,
                    )? {
                        collision
                    } else {
                        RemoteObservation::Present(RemoteResource::new(
                            remote_id,
                            application_properties(&address, compiled, &application),
                        ))
                    }
                }
                Err(SdkError::Api(error)) if error.status() == 404 => {
                    let current_parent = application_parent_from_state(&address, state)?;
                    observe_missing_managed_application(
                        &address,
                        &current_parent,
                        compiled,
                        state,
                        topology,
                        &collections,
                        authority,
                    )?
                }
                Err(error) => RemoteObservation::Unavailable(classify_sdk_error(&error)),
            }
        } else {
            let parent = application_parent_from_desired(&address, compiled, state)?;
            observe_application_under_parent(
                &address,
                &parent,
                compiled,
                state,
                topology,
                &collections,
                authority,
            )?
        };
        observations.push((address, observation));
    }

    Ok(observations)
}

#[allow(clippy::too_many_arguments)]
fn observe_missing_managed_application(
    address: &ResourceAddress,
    current_parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &BTreeMap<String, Result<dokploy_sdk::ApplicationCollection, dokploy_sdk::Error>>,
    authority: ApplicationTopologyAuthority,
) -> Result<RemoteObservation, DiscoverRemoteError> {
    let current = observe_application_under_parent(
        address,
        current_parent,
        compiled,
        state,
        topology,
        collections,
        authority,
    )?;
    let missing_remote_id = state
        .resource(address)
        .expect("the managed 404 path requires durable resource state")
        .remote_id();
    let current = normalize_missing_identity(current, missing_remote_id);
    let desired_parent = application_parent_from_desired(address, compiled, state)?;
    let current_environment_id = trusted_environment_id(current_parent, compiled, state, topology);
    let desired_environment_id = trusted_environment_id(&desired_parent, compiled, state, topology);
    if desired_parent == *current_parent
        || (current_environment_id.is_some() && current_environment_id == desired_environment_id)
    {
        return Ok(current);
    }
    let desired = observe_application_under_parent(
        address,
        &desired_parent,
        compiled,
        state,
        topology,
        collections,
        authority,
    )?;
    let desired = normalize_missing_identity(desired, missing_remote_id);

    match (current, desired) {
        (RemoteObservation::Present(resource), _) | (_, RemoteObservation::Present(resource)) => {
            Ok(RemoteObservation::Present(resource))
        }
        (RemoteObservation::Unavailable(failure), _)
        | (_, RemoteObservation::Unavailable(failure)) => {
            Ok(RemoteObservation::Unavailable(failure))
        }
        (RemoteObservation::Missing, RemoteObservation::Missing) => Ok(RemoteObservation::Missing),
    }
}

fn normalize_missing_identity(
    observation: RemoteObservation,
    missing_remote_id: &RemoteId,
) -> RemoteObservation {
    match observation {
        RemoteObservation::Present(resource) if resource.remote_id() == missing_remote_id => {
            RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
        }
        observation => observation,
    }
}

#[allow(clippy::too_many_arguments)]
fn observe_reparent_target(
    address: &ResourceAddress,
    current_parent: &ResourceAddress,
    current_remote_id: &RemoteId,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &BTreeMap<String, Result<dokploy_sdk::ApplicationCollection, dokploy_sdk::Error>>,
    authority: ApplicationTopologyAuthority,
) -> Result<Option<RemoteObservation>, DiscoverRemoteError> {
    let desired_parent = application_parent_from_desired(address, compiled, state)?;
    let Some(current_environment_id) =
        trusted_environment_id(current_parent, compiled, state, topology)
    else {
        return Ok(Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        )));
    };
    match effective_environment_observation(&desired_parent, compiled, topology) {
        Some(RemoteObservation::Missing) => return Ok(None),
        Some(RemoteObservation::Unavailable(failure)) => {
            return Ok(Some(RemoteObservation::Unavailable(*failure)));
        }
        Some(RemoteObservation::Present(_)) => {}
        None => {
            return Ok(Some(RemoteObservation::Unavailable(
                RemoteFailureKind::InvalidResponse,
            )));
        }
    }
    let Some(desired_environment_id) =
        trusted_environment_id(&desired_parent, compiled, state, topology)
    else {
        return Ok(Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        )));
    };
    if desired_environment_id == current_environment_id {
        return Ok(None);
    }

    match collections.get(&desired_environment_id) {
        Some(Ok(collection)) => {
            if let Some(application) = collection
                .applications()
                .iter()
                .find(|application| application.name == address.name().as_str())
            {
                let remote_id = RemoteId::new(application.application_id.as_str())
                    .map_err(|_| DiscoverRemoteError::InvalidApplicationId)?;
                if &remote_id == current_remote_id {
                    return Err(DiscoverRemoteError::ApplicationContainment);
                }
                return Ok(Some(RemoteObservation::Present(RemoteResource::new(
                    remote_id,
                    BTreeMap::new(),
                ))));
            }
            if authority == ApplicationTopologyAuthority::Authoritative {
                Ok(None)
            } else {
                Ok(Some(RemoteObservation::Unavailable(
                    RemoteFailureKind::InvalidResponse,
                )))
            }
        }
        Some(Err(error)) => Ok(Some(RemoteObservation::Unavailable(classify_sdk_error(
            error,
        )))),
        None => Ok(Some(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        ))),
    }
}

fn validate_application_collections(
    collections: &BTreeMap<String, Result<dokploy_sdk::ApplicationCollection, dokploy_sdk::Error>>,
) -> Result<(), DiscoverRemoteError> {
    let mut global_ids = BTreeSet::new();
    for (environment_id, collection) in collections
        .iter()
        .filter_map(|(id, result)| result.as_ref().ok().map(|collection| (id, collection)))
    {
        let mut scoped_names = BTreeSet::new();
        for application in collection.applications() {
            let remote_id = RemoteId::new(application.application_id.as_str())
                .map_err(|_| DiscoverRemoteError::InvalidApplicationId)?;
            if application.environment_id.as_str() != environment_id {
                return Err(DiscoverRemoteError::ApplicationContainment);
            }
            if !global_ids.insert(remote_id) {
                return Err(DiscoverRemoteError::DuplicateApplicationId);
            }
            if !scoped_names.insert(application.name.as_str()) {
                return Err(DiscoverRemoteError::DuplicateApplicationName);
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_application_under_parent(
    address: &ResourceAddress,
    parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &BTreeMap<String, Result<dokploy_sdk::ApplicationCollection, dokploy_sdk::Error>>,
    authority: ApplicationTopologyAuthority,
) -> Result<RemoteObservation, DiscoverRemoteError> {
    match effective_environment_observation(parent, compiled, topology) {
        Some(RemoteObservation::Missing) => return Ok(RemoteObservation::Missing),
        Some(RemoteObservation::Unavailable(failure)) => {
            return Ok(RemoteObservation::Unavailable(*failure));
        }
        Some(RemoteObservation::Present(_)) => {}
        None => {
            return Ok(RemoteObservation::Unavailable(
                RemoteFailureKind::InvalidResponse,
            ));
        }
    }
    let Some(environment_id) = trusted_environment_id(parent, compiled, state, topology) else {
        return Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        ));
    };
    match collections.get(&environment_id) {
        Some(Ok(collection)) => {
            let matches = collection
                .applications()
                .iter()
                .filter(|application| application.name == address.name().as_str())
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Err(DiscoverRemoteError::DuplicateApplicationName);
            }
            if let Some(application) = matches.first() {
                let remote_id = RemoteId::new(application.application_id.as_str())
                    .map_err(|_| DiscoverRemoteError::InvalidApplicationId)?;
                return Ok(RemoteObservation::Present(RemoteResource::new(
                    remote_id,
                    BTreeMap::new(),
                )));
            }
            if authority == ApplicationTopologyAuthority::Authoritative {
                Ok(RemoteObservation::Missing)
            } else {
                Ok(RemoteObservation::Unavailable(
                    RemoteFailureKind::InvalidResponse,
                ))
            }
        }
        Some(Err(error)) => Ok(RemoteObservation::Unavailable(classify_sdk_error(error))),
        None => Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        )),
    }
}

fn application_parent_from_state(
    address: &ResourceAddress,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    let resource = state
        .resource(address)
        .ok_or(DiscoverRemoteError::ApplicationContainment)?;
    let mut parents = resource
        .dependencies()
        .iter()
        .filter(|dependency| dependency.kind() == ResourceKind::Environment);
    let parent = parents
        .next()
        .ok_or(DiscoverRemoteError::ApplicationContainment)?;
    if parents.next().is_some() {
        return Err(DiscoverRemoteError::ApplicationContainment);
    }

    Ok(parent.clone())
}

fn application_parent_from_desired(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    if let Some(parent) = compiled.bindings().parent_of(address) {
        if parent.kind() == ResourceKind::Environment {
            return Ok(parent.clone());
        }
        return Err(DiscoverRemoteError::ApplicationContainment);
    }
    if let Some(target) = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.from() == address)
        .map(|directive| directive.to())
    {
        if let Some(parent) = compiled.bindings().parent_of(target) {
            if parent.kind() == ResourceKind::Environment {
                return Ok(parent.clone());
            }
            return Err(DiscoverRemoteError::ApplicationContainment);
        }
        if state.resource(target).is_some() {
            return application_parent_from_state(target, state);
        }
    }
    let source = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == address)
        .map(|directive| directive.from());
    if let Some(source) = source {
        return application_parent_from_state(source, state);
    }
    application_parent_from_state(address, state)
}

fn trusted_environment_id(
    parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
) -> Option<String> {
    let effective = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == parent)
        .map_or(parent, |directive| directive.from());
    let stored = state.resource(effective)?;
    match observation(topology, effective) {
        Some(RemoteObservation::Present(environment))
            if stored.remote_id() == environment.remote_id() =>
        {
            Some(environment.remote_id().as_str().to_owned())
        }
        Some(
            RemoteObservation::Present(_)
            | RemoteObservation::Missing
            | RemoteObservation::Unavailable(_),
        )
        | None => None,
    }
}

fn effective_environment_observation<'a>(
    parent: &ResourceAddress,
    compiled: &CompiledDesired,
    topology: &'a [(ResourceAddress, RemoteObservation)],
) -> Option<&'a RemoteObservation> {
    let effective = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == parent)
        .map_or(parent, |directive| directive.from());
    observation(topology, effective)
}

fn observation<'a>(
    topology: &'a [(ResourceAddress, RemoteObservation)],
    address: &ResourceAddress,
) -> Option<&'a RemoteObservation> {
    topology
        .iter()
        .find_map(|(candidate, observation)| (candidate == address).then_some(observation))
}

fn application_properties(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    application: &dokploy_sdk::ApplicationDetails,
) -> BTreeMap<PropertyPath, PropertyObservation> {
    let mut properties = BTreeMap::new();
    let Some(desired) = desired_resource_for_observation(address, compiled) else {
        return properties;
    };

    for path in desired.properties().keys() {
        if desired.ignored_changes().contains(path) {
            continue;
        }
        let observed = match path {
            PropertyPath::Description => observe_string_field(&application.description),
            PropertyPath::Replicas => observe_u32_field(&application.replicas),
            PropertyPath::Source => observe_source_root(application),
            PropertyPath::SourceRepository => {
                observe_github_source_child(application, &application.repository)
            }
            PropertyPath::SourceBranch => {
                observe_github_source_child(application, &application.branch)
            }
            PropertyPath::Environment => observe_environment_root(&application.environment),
            PropertyPath::EnvironmentVariable(_) => {
                observe_environment_child(&application.environment)
            }
            PropertyPath::Database
            | PropertyPath::Username
            | PropertyPath::Password
            | PropertyPath::Host
            | PropertyPath::Application
            | PropertyPath::DeploymentStatus => continue,
        };
        properties.insert(path.clone(), observed);
    }

    properties
}

fn desired_resource_for_observation<'a>(
    address: &ResourceAddress,
    compiled: &'a CompiledDesired,
) -> Option<&'a dokploy_core::DesiredResource> {
    let desired_address = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.from() == address)
        .map_or(address, |directive| directive.to());
    compiled.desired_state().resources().get(desired_address)
}

fn observe_string_field(field: &ResponseField<String>) -> PropertyObservation {
    match field {
        ResponseField::NotReturned => {
            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
        }
        ResponseField::Null => PropertyObservation::KnownAbsent,
        ResponseField::Value(value) => PropertyObservation::Known(
            ComparableValue::try_from_json(serde_json::json!(value))
                .expect("a concrete string response field is comparable"),
        ),
    }
}

fn observe_u32_field(field: &ResponseField<u32>) -> PropertyObservation {
    match field {
        ResponseField::NotReturned => {
            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
        }
        ResponseField::Null => PropertyObservation::KnownAbsent,
        ResponseField::Value(value) => PropertyObservation::Known(
            ComparableValue::try_from_json(serde_json::json!(value))
                .expect("a concrete integer response field is comparable"),
        ),
    }
}

fn observe_source_root(application: &dokploy_sdk::ApplicationDetails) -> PropertyObservation {
    match &application.source_type {
        ResponseField::NotReturned => {
            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
        }
        ResponseField::Null
            if matches!(&application.repository, ResponseField::Value(_))
                || matches!(&application.branch, ResponseField::Value(_)) =>
        {
            PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse)
        }
        ResponseField::Null => PropertyObservation::KnownAbsent,
        ResponseField::Value(value) => PropertyObservation::Known(
            ComparableValue::try_from_json(serde_json::json!(value))
                .expect("a source type is comparable"),
        ),
    }
}

fn observe_github_source_child(
    application: &dokploy_sdk::ApplicationDetails,
    field: &ResponseField<String>,
) -> PropertyObservation {
    match &application.source_type {
        ResponseField::Value(source_type) if source_type == "github" => observe_string_field(field),
        ResponseField::NotReturned => {
            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
        }
        ResponseField::Null | ResponseField::Value(_) => {
            PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse)
        }
    }
}

fn observe_environment_root(
    field: &ResponseField<ApplicationEnvironmentShape>,
) -> PropertyObservation {
    match field {
        ResponseField::NotReturned => {
            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
        }
        ResponseField::Null => PropertyObservation::KnownAbsent,
        ResponseField::Value(ApplicationEnvironmentShape::Empty) => PropertyObservation::Known(
            ComparableValue::try_from_json(serde_json::json!({}))
                .expect("an empty environment is comparable"),
        ),
        ResponseField::Value(ApplicationEnvironmentShape::Opaque) => PropertyObservation::Known(
            ComparableValue::try_from_json(serde_json::json!({"opaque": true}))
                .expect("an opaque environment shape is comparable"),
        ),
    }
}

fn observe_environment_child(
    field: &ResponseField<ApplicationEnvironmentShape>,
) -> PropertyObservation {
    match field {
        ResponseField::Null | ResponseField::Value(ApplicationEnvironmentShape::Empty) => {
            PropertyObservation::KnownAbsent
        }
        ResponseField::Value(ApplicationEnvironmentShape::Opaque) => {
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)
        }
        ResponseField::NotReturned => {
            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
        }
    }
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
