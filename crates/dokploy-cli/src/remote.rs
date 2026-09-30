//! Fresh, read-only Dokploy projections for declarative planning.

use std::collections::{BTreeMap, BTreeSet};

use dokploy_core::{
    ComparableValue, MutationContract, MutationMode, PropertyMutation, PropertyObservation,
    PropertyPath, PropertyUnknownReason, RemoteFailureKind, RemoteObservation, RemoteResource,
    RemoteState, RemoteStateError, ReplacementOrder,
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

/// Whether a fully paginated parent-scoped Postgres search is complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostgresTopologyAuthority {
    /// Exhaustive absence below a proven environment proves nonexistence.
    Authoritative,
    /// Absence may be caused by role-dependent filtering.
    Partial,
}

/// Whether a fully paginated parent-scoped Redis search is complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedisTopologyAuthority {
    /// Exhaustive absence below a proven environment proves nonexistence.
    Authoritative,
    /// Absence may be caused by role-dependent filtering.
    Partial,
}

/// Whether an application-scoped domain collection is known to be complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainTopologyAuthority {
    /// Absence from the application collection proves nonexistence.
    Authoritative,
    /// Absence may be caused by role-dependent filtering.
    Partial,
}

/// Visibility assertions required by combined discovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryAuthority {
    /// Completeness of `project.all`.
    pub projects: ProjectTopologyAuthority,
    /// Completeness of each `environment.byProjectId` collection.
    pub environments: EnvironmentTopologyAuthority,
    /// Completeness of each fully paginated `application.search` collection.
    pub applications: ApplicationTopologyAuthority,
    /// Completeness of each fully paginated `postgres.search` collection.
    pub postgres: PostgresTopologyAuthority,
    /// Completeness of each fully paginated `redis.search` collection.
    pub redis: RedisTopologyAuthority,
    /// Completeness of each `domain.byApplicationId` collection.
    pub domains: DomainTopologyAuthority,
}

impl DiscoveryAuthority {
    /// Full-instance visibility required by the public reconciliation workflow.
    #[must_use]
    pub const fn reconciliation() -> Self {
        Self {
            projects: ProjectTopologyAuthority::Authoritative,
            environments: EnvironmentTopologyAuthority::Authoritative,
            applications: ApplicationTopologyAuthority::Authoritative,
            postgres: PostgresTopologyAuthority::Authoritative,
            redis: RedisTopologyAuthority::Authoritative,
            domains: DomainTopologyAuthority::Authoritative,
        }
    }
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
    /// A Postgres database has no unambiguous containing environment.
    #[error("DOKREM016: Postgres containment is unavailable")]
    PostgresContainment,
    /// A Postgres physical identity does not satisfy the state contract.
    #[error("DOKREM017: Postgres topology contains an invalid remote identity")]
    InvalidPostgresId,
    /// More than one Postgres database has the same name within one environment.
    #[error("DOKREM018: Postgres topology contains a duplicate scoped name")]
    DuplicatePostgresName,
    /// More than one Postgres database has the same physical identity.
    #[error("DOKREM019: Postgres topology contains a duplicate remote identity")]
    DuplicatePostgresId,
    /// A Postgres dependency change would require an unsupported remote reparent.
    #[error("DOKREM020: Postgres reparenting is not supported")]
    PostgresReparentUnsupported,
    /// Direct and collection Postgres reads contradict each other.
    #[error("DOKREM021: Postgres read endpoints returned conflicting topology")]
    PostgresTopologyConflict,
    /// A Redis database has no unambiguous containing environment.
    #[error("DOKREM022: Redis containment is unavailable")]
    RedisContainment,
    /// A Redis physical identity does not satisfy the state contract.
    #[error("DOKREM023: Redis topology contains an invalid remote identity")]
    InvalidRedisId,
    /// More than one Redis database has the same name within one environment.
    #[error("DOKREM024: Redis topology contains a duplicate scoped name")]
    DuplicateRedisName,
    /// More than one Redis database has the same physical identity.
    #[error("DOKREM025: Redis topology contains a duplicate remote identity")]
    DuplicateRedisId,
    /// A Redis dependency change would require an unsupported remote reparent.
    #[error("DOKREM026: Redis reparenting is not supported")]
    RedisReparentUnsupported,
    /// Direct and collection Redis reads contradict each other.
    #[error("DOKREM027: Redis read endpoints returned conflicting topology")]
    RedisTopologyConflict,
    /// A domain has no valid configured application binding.
    #[error("DOKREM028: domain application binding is unavailable")]
    DomainApplication,
    /// A domain physical identity does not satisfy the state contract.
    #[error("DOKREM029: domain topology contains an invalid remote identity")]
    InvalidDomainId,
    /// More than one domain has the same host within one application.
    #[error("DOKREM030: domain topology contains a duplicate application-scoped host")]
    DuplicateDomainHost,
    /// More than one logical address resolves to the same domain identity.
    #[error("DOKREM031: domain topology contains a duplicate remote identity")]
    DuplicateDomainId,
    /// Direct and collection Domain reads contradict each other.
    #[error("DOKREM032: Domain read endpoints returned conflicting topology")]
    DomainTopologyConflict,
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

    remote_state_with_contracts(state.instance().clone(), observations)
        .map_err(DiscoverProjectsError::InvalidRemoteState)
}

/// Reads fresh project, environment, application, Postgres, and Redis state into one snapshot.
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
    let postgres =
        discover_postgres_observations(client, compiled, state, &observations, authority.postgres)
            .await?;
    observations.extend(postgres);
    let redis =
        discover_redis_observations(client, compiled, state, &observations, authority.redis)
            .await?;
    observations.extend(redis);
    let domains =
        discover_domain_observations(client, compiled, state, &observations, authority.domains)
            .await?;
    observations.extend(domains);

    remote_state_with_contracts(state.instance().clone(), observations)
        .map_err(DiscoverRemoteError::InvalidRemoteState)
}

fn remote_state_with_contracts(
    instance: InstanceIdentity,
    observations: Vec<(ResourceAddress, RemoteObservation)>,
) -> Result<RemoteState, RemoteStateError> {
    let contracts = observations
        .iter()
        .map(|(address, _)| (address.clone(), mutation_contract(address.kind())))
        .collect::<Vec<_>>();
    RemoteState::try_new_with_contracts(instance, observations, contracts)
}

fn mutation_contract(kind: ResourceKind) -> MutationContract {
    let in_place = PropertyMutation::new(MutationMode::InPlace, MutationMode::InPlace);
    let set_only = PropertyMutation::new(MutationMode::InPlace, MutationMode::Unsupported);
    match kind {
        ResourceKind::Project => MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
            .with_property(PropertyPath::Description, in_place),
        ResourceKind::Environment => {
            MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
                .with_property(PropertyPath::Description, in_place)
                .with_containment(MutationMode::Replace)
        }
        ResourceKind::Application => {
            MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
                .with_default_property(in_place)
                .with_containment(MutationMode::InPlace)
        }
        ResourceKind::Postgres => MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
            .requiring(PropertyPath::Database)
            .requiring(PropertyPath::Username)
            .requiring(PropertyPath::Password)
            .with_property(PropertyPath::Database, set_only)
            .with_property(PropertyPath::Username, set_only)
            .with_property(PropertyPath::Password, set_only)
            .with_containment(MutationMode::StateOnly),
        ResourceKind::Redis => MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
            .requiring(PropertyPath::Password)
            .with_property(PropertyPath::Password, set_only)
            .with_containment(MutationMode::StateOnly),
        ResourceKind::Domain => MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
            .requiring(PropertyPath::Host)
            .requiring(PropertyPath::Application)
            .with_property(PropertyPath::Host, set_only)
            .with_property(
                PropertyPath::Application,
                PropertyMutation::new(MutationMode::Replace, MutationMode::Unsupported),
            )
            .with_containment(MutationMode::StateOnly),
    }
}

async fn discover_domain_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    authority: DomainTopologyAuthority,
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
        .filter(|address| address.kind() == ResourceKind::Domain)
        .cloned()
        .collect();
    let mut application_bindings = BTreeMap::new();
    let mut collections = BTreeMap::new();

    for address in &addresses {
        let application = domain_application(address, compiled, state)?;
        let application_id = trusted_application_id(&application, compiled, state, topology);
        if let Some(application_id) = application_id.as_ref()
            && !collections.contains_key(application_id)
        {
            let collection = client
                .domains()
                .by_application(dokploy_sdk::ApplicationId::new(application_id))
                .await;
            collections.insert(application_id.clone(), collection);
        }
        application_bindings.insert(address.clone(), (application, application_id));
    }
    validate_domain_collections(&collections)?;

    let mut seen_ids = BTreeSet::new();
    let mut observations = Vec::new();
    for address in addresses {
        let (application, application_id) = application_bindings
            .get(&address)
            .expect("every domain receives an application binding");
        let observation = if let Some(stored) = state.resource(&address) {
            match client
                .domains()
                .get(dokploy_sdk::DomainId::new(stored.remote_id().as_str()))
                .await
            {
                Ok(domain) => {
                    let remote_id = RemoteId::new(domain.domain_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidDomainId)?;
                    if remote_id != *stored.remote_id()
                        || domain
                            .application_id
                            .as_ref()
                            .map(dokploy_sdk::ApplicationId::as_str)
                            != application_id.as_deref()
                    {
                        return Err(DiscoverRemoteError::DomainTopologyConflict);
                    }
                    validate_direct_domain_against_collection(
                        &domain,
                        application_id.as_deref(),
                        &collections,
                        authority,
                    )?;
                    if !seen_ids.insert(remote_id.clone()) {
                        return Err(DiscoverRemoteError::DuplicateDomainId);
                    }
                    RemoteObservation::Present(RemoteResource::new(
                        remote_id,
                        domain_properties(&address, compiled, &domain, application),
                    ))
                }
                Err(SdkError::Api(error)) if error.status() == 404 => {
                    observe_domain_under_application(
                        &address,
                        application,
                        application_id.as_deref(),
                        compiled,
                        topology,
                        &collections,
                        authority,
                    )?
                }
                Err(error) => RemoteObservation::Unavailable(classify_sdk_error(&error)),
            }
        } else {
            observe_domain_under_application(
                &address,
                application,
                application_id.as_deref(),
                compiled,
                topology,
                &collections,
                authority,
            )?
        };
        observations.push((address, observation));
    }

    Ok(observations)
}

fn validate_domain_collections(
    collections: &BTreeMap<String, Result<dokploy_sdk::DomainCollection, dokploy_sdk::Error>>,
) -> Result<(), DiscoverRemoteError> {
    let mut global_ids = BTreeSet::new();
    for (application_id, collection) in collections
        .iter()
        .filter_map(|(id, result)| result.as_ref().ok().map(|collection| (id, collection)))
    {
        let mut scoped_hosts = BTreeSet::new();
        for domain in collection.domains() {
            let remote_id = RemoteId::new(domain.domain_id.as_str())
                .map_err(|_| DiscoverRemoteError::InvalidDomainId)?;
            if domain
                .application_id
                .as_ref()
                .map(dokploy_sdk::ApplicationId::as_str)
                != Some(application_id.as_str())
            {
                return Err(DiscoverRemoteError::DomainApplication);
            }
            if !global_ids.insert(remote_id) {
                return Err(DiscoverRemoteError::DuplicateDomainId);
            }
            if !scoped_hosts.insert(domain.host.as_str()) {
                return Err(DiscoverRemoteError::DuplicateDomainHost);
            }
        }
    }

    Ok(())
}

fn validate_direct_domain_against_collection(
    domain: &dokploy_sdk::DomainDetails,
    application_id: Option<&str>,
    collections: &BTreeMap<String, Result<dokploy_sdk::DomainCollection, dokploy_sdk::Error>>,
    authority: DomainTopologyAuthority,
) -> Result<(), DiscoverRemoteError> {
    let Some(application_id) = application_id else {
        return Err(DiscoverRemoteError::DomainApplication);
    };
    let Some(collection) = collections.get(application_id) else {
        return Err(DiscoverRemoteError::DomainApplication);
    };
    match collection {
        Ok(collection) => {
            let matching = collection
                .domains()
                .iter()
                .find(|item| item.domain_id == domain.domain_id);
            if let Some(matching) = matching {
                if matching.host != domain.host {
                    return Err(DiscoverRemoteError::DomainTopologyConflict);
                }
                Ok(())
            } else if authority == DomainTopologyAuthority::Authoritative {
                Err(DiscoverRemoteError::DomainTopologyConflict)
            } else {
                Ok(())
            }
        }
        Err(_) => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn observe_domain_under_application(
    address: &ResourceAddress,
    application: &ResourceAddress,
    application_id: Option<&str>,
    compiled: &CompiledDesired,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &BTreeMap<String, Result<dokploy_sdk::DomainCollection, dokploy_sdk::Error>>,
    authority: DomainTopologyAuthority,
) -> Result<RemoteObservation, DiscoverRemoteError> {
    match observation(topology, application) {
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
    let Some(application_id) = application_id else {
        return Ok(RemoteObservation::Unavailable(
            RemoteFailureKind::InvalidResponse,
        ));
    };
    match collections.get(application_id) {
        Some(Ok(collection)) => {
            let Some(host) = desired_domain_host(address, compiled) else {
                return Ok(RemoteObservation::Unavailable(
                    RemoteFailureKind::InvalidResponse,
                ));
            };
            let matches = collection
                .domains()
                .iter()
                .filter(|domain| domain.host == host)
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Err(DiscoverRemoteError::DuplicateDomainHost);
            }
            if let Some(domain) = matches.first() {
                let remote_id = RemoteId::new(domain.domain_id.as_str())
                    .map_err(|_| DiscoverRemoteError::InvalidDomainId)?;
                return Ok(RemoteObservation::Present(RemoteResource::new(
                    remote_id,
                    domain_properties(address, compiled, domain, application),
                )));
            }
            if authority == DomainTopologyAuthority::Authoritative {
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

fn domain_application(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    if let Some(application) = compiled.bindings().domain_application(address) {
        return Ok(application.clone());
    }
    let resource = state
        .resource(address)
        .ok_or(DiscoverRemoteError::DomainApplication)?;
    let application = resource
        .last_applied()
        .as_json()
        .get("application")
        .and_then(serde_json::Value::as_str)
        .ok_or(DiscoverRemoteError::DomainApplication)?;
    let application: ResourceAddress = application
        .parse()
        .map_err(|_| DiscoverRemoteError::DomainApplication)?;
    if application.kind() != ResourceKind::Application {
        return Err(DiscoverRemoteError::DomainApplication);
    }

    Ok(application)
}

fn trusted_application_id(
    application: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
) -> Option<String> {
    let effective = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == application)
        .map_or(application, |directive| {
            if state.resource(application).is_some() {
                application
            } else {
                directive.from()
            }
        });
    let stored = state.resource(effective)?;
    match observation(topology, effective) {
        Some(RemoteObservation::Present(remote)) if stored.remote_id() == remote.remote_id() => {
            Some(remote.remote_id().as_str().to_owned())
        }
        _ => None,
    }
}

fn desired_domain_host(address: &ResourceAddress, compiled: &CompiledDesired) -> Option<String> {
    compiled.bindings().domain_host(address).map(str::to_owned)
}

fn domain_properties(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    domain: &dokploy_sdk::DomainDetails,
    application: &ResourceAddress,
) -> BTreeMap<PropertyPath, PropertyObservation> {
    let mut properties = BTreeMap::new();
    let Some(desired) = desired_resource_for_observation(address, compiled) else {
        return properties;
    };

    for path in desired.properties().keys() {
        if desired.ignored_changes().contains(path) {
            continue;
        }
        let value = match path {
            PropertyPath::Host => serde_json::json!(domain.host),
            PropertyPath::Application => serde_json::json!(application.to_string()),
            _ => continue,
        };
        properties.insert(
            path.clone(),
            PropertyObservation::Known(
                ComparableValue::try_from_json(value).expect("domain values are non-null"),
            ),
        );
    }

    properties
}

async fn discover_postgres_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    authority: PostgresTopologyAuthority,
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
        .filter(|address| address.kind() == ResourceKind::Postgres)
        .cloned()
        .collect();
    let mut collections = BTreeMap::new();

    for address in &addresses {
        let mut parents = vec![postgres_parent_from_desired(address, compiled, state)?];
        if state.resource(address).is_some() {
            let current_parent = postgres_parent_from_state(address, state)?;
            validate_postgres_parent_change(
                address,
                &current_parent,
                &parents[0],
                compiled,
                state,
                topology,
            )?;
            parents.push(current_parent);
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
                .postgres()
                .by_environment(dokploy_sdk::EnvironmentId::new(&environment_id))
                .await;
            collections.insert(environment_id, collection);
        }
    }
    validate_postgres_collections(&collections)?;

    let mut seen_direct_ids = BTreeSet::new();
    let mut observations = Vec::new();
    for address in addresses {
        let observation = if let Some(stored) = state.resource(&address) {
            let current_parent = postgres_parent_from_state(&address, state)?;
            match client
                .postgres()
                .get(dokploy_sdk::PostgresId::new(stored.remote_id().as_str()))
                .await
            {
                Ok(postgres) => {
                    let remote_id = RemoteId::new(postgres.postgres_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidPostgresId)?;
                    if remote_id != *stored.remote_id() {
                        return Err(DiscoverRemoteError::InvalidPostgresId);
                    }
                    let expected_environment_id =
                        trusted_environment_id(&current_parent, compiled, state, topology)
                            .ok_or(DiscoverRemoteError::PostgresContainment)?;
                    if postgres.environment_id.as_str() != expected_environment_id {
                        return Err(DiscoverRemoteError::PostgresContainment);
                    }
                    validate_direct_postgres_against_collection(
                        &postgres,
                        &expected_environment_id,
                        &collections,
                        authority,
                    )?;
                    if !seen_direct_ids.insert(remote_id.clone()) {
                        return Err(DiscoverRemoteError::DuplicatePostgresId);
                    }
                    RemoteObservation::Present(RemoteResource::new(
                        remote_id,
                        postgres_properties(&address, compiled, &postgres),
                    ))
                }
                Err(SdkError::Api(error)) if error.status() == 404 => {
                    if postgres_collections_contain_id(stored.remote_id(), &collections) {
                        RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
                    } else {
                        let observed = observe_postgres_under_parent(
                            &address,
                            &current_parent,
                            compiled,
                            state,
                            topology,
                            &collections,
                            authority,
                        )?;
                        normalize_missing_identity(observed, stored.remote_id())
                    }
                }
                Err(error) => RemoteObservation::Unavailable(classify_sdk_error(&error)),
            }
        } else {
            let parent = postgres_parent_from_desired(&address, compiled, state)?;
            observe_postgres_under_parent(
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

async fn discover_redis_observations(
    client: &Dokploy,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    authority: RedisTopologyAuthority,
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
        .filter(|address| address.kind() == ResourceKind::Redis)
        .cloned()
        .collect();
    let mut collections = BTreeMap::new();

    for address in &addresses {
        let mut parents = vec![redis_parent_from_desired(address, compiled, state)?];
        if state.resource(address).is_some() {
            let current_parent = redis_parent_from_state(address, state)?;
            validate_redis_parent_change(
                address,
                &current_parent,
                &parents[0],
                compiled,
                state,
                topology,
            )?;
            parents.push(current_parent);
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
                .redis()
                .by_environment(dokploy_sdk::EnvironmentId::new(&environment_id))
                .await;
            collections.insert(environment_id, collection);
        }
    }
    validate_redis_collections(&collections)?;

    let mut seen_direct_ids = BTreeSet::new();
    let mut observations = Vec::new();
    for address in addresses {
        let observation = if let Some(stored) = state.resource(&address) {
            let current_parent = redis_parent_from_state(&address, state)?;
            match client
                .redis()
                .get(dokploy_sdk::RedisId::new(stored.remote_id().as_str()))
                .await
            {
                Ok(redis) => {
                    let remote_id = RemoteId::new(redis.redis_id.as_str())
                        .map_err(|_| DiscoverRemoteError::InvalidRedisId)?;
                    if remote_id != *stored.remote_id() {
                        return Err(DiscoverRemoteError::InvalidRedisId);
                    }
                    let expected_environment_id =
                        trusted_environment_id(&current_parent, compiled, state, topology)
                            .ok_or(DiscoverRemoteError::RedisContainment)?;
                    if redis.environment_id.as_str() != expected_environment_id {
                        return Err(DiscoverRemoteError::RedisContainment);
                    }
                    validate_direct_redis_against_collection(
                        &redis,
                        &expected_environment_id,
                        &collections,
                        authority,
                    )?;
                    if !seen_direct_ids.insert(remote_id.clone()) {
                        return Err(DiscoverRemoteError::DuplicateRedisId);
                    }
                    RemoteObservation::Present(RemoteResource::new(
                        remote_id,
                        redis_properties(&address, compiled),
                    ))
                }
                Err(SdkError::Api(error)) if error.status() == 404 => {
                    if redis_collections_contain_id(stored.remote_id(), &collections) {
                        RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
                    } else {
                        let observed = observe_redis_under_parent(
                            &address,
                            &current_parent,
                            compiled,
                            state,
                            topology,
                            &collections,
                            authority,
                        )?;
                        normalize_missing_identity(observed, stored.remote_id())
                    }
                }
                Err(error) => RemoteObservation::Unavailable(classify_sdk_error(&error)),
            }
        } else {
            let parent = redis_parent_from_desired(&address, compiled, state)?;
            observe_redis_under_parent(
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
    match effective_environment_observation(&desired_parent, compiled, state, topology) {
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
    match effective_environment_observation(parent, compiled, state, topology) {
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
    let parent = resource
        .containment()
        .ok_or(DiscoverRemoteError::ApplicationContainment)?;
    if parent.kind() != ResourceKind::Environment {
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
    let effective = effective_environment_address(parent, compiled, state);
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
    state: &StateFile,
    topology: &'a [(ResourceAddress, RemoteObservation)],
) -> Option<&'a RemoteObservation> {
    let effective = effective_environment_address(parent, compiled, state);

    observation(topology, effective)
}

fn effective_environment_address<'a>(
    parent: &'a ResourceAddress,
    compiled: &'a CompiledDesired,
    state: &StateFile,
) -> &'a ResourceAddress {
    compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == parent)
        .map_or(parent, |directive| {
            if state.resource(parent).is_some() {
                parent
            } else {
                directive.from()
            }
        })
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

fn postgres_properties(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    postgres: &dokploy_sdk::PostgresDetails,
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
            PropertyPath::Database => observe_string_field(&postgres.database_name),
            PropertyPath::Username => observe_string_field(&postgres.database_user),
            PropertyPath::Password => {
                PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)
            }
            PropertyPath::Description
            | PropertyPath::Replicas
            | PropertyPath::Source
            | PropertyPath::SourceRepository
            | PropertyPath::SourceBranch
            | PropertyPath::Environment
            | PropertyPath::EnvironmentVariable(_)
            | PropertyPath::Host
            | PropertyPath::Application
            | PropertyPath::DeploymentStatus => continue,
        };
        properties.insert(path.clone(), observed);
    }

    properties
}

fn validate_postgres_collections(
    collections: &BTreeMap<String, Result<dokploy_sdk::PostgresCollection, dokploy_sdk::Error>>,
) -> Result<(), DiscoverRemoteError> {
    let mut global_ids = BTreeSet::new();
    for (environment_id, collection) in collections
        .iter()
        .filter_map(|(id, result)| result.as_ref().ok().map(|collection| (id, collection)))
    {
        let mut scoped_names = BTreeSet::new();
        for postgres in collection.postgres() {
            let remote_id = RemoteId::new(postgres.postgres_id.as_str())
                .map_err(|_| DiscoverRemoteError::InvalidPostgresId)?;
            if postgres.environment_id.as_str() != environment_id {
                return Err(DiscoverRemoteError::PostgresContainment);
            }
            if !global_ids.insert(remote_id) {
                return Err(DiscoverRemoteError::DuplicatePostgresId);
            }
            if !scoped_names.insert(postgres.name.as_str()) {
                return Err(DiscoverRemoteError::DuplicatePostgresName);
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_postgres_under_parent(
    address: &ResourceAddress,
    parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &BTreeMap<String, Result<dokploy_sdk::PostgresCollection, dokploy_sdk::Error>>,
    authority: PostgresTopologyAuthority,
) -> Result<RemoteObservation, DiscoverRemoteError> {
    match effective_environment_observation(parent, compiled, state, topology) {
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
            if let Some(postgres) = collection
                .postgres()
                .iter()
                .find(|postgres| postgres.name == address.name().as_str())
            {
                let remote_id = RemoteId::new(postgres.postgres_id.as_str())
                    .map_err(|_| DiscoverRemoteError::InvalidPostgresId)?;
                return Ok(RemoteObservation::Present(RemoteResource::new(
                    remote_id,
                    BTreeMap::new(),
                )));
            }
            if authority == PostgresTopologyAuthority::Authoritative {
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

fn validate_direct_postgres_against_collection(
    postgres: &dokploy_sdk::PostgresDetails,
    environment_id: &str,
    collections: &BTreeMap<String, Result<dokploy_sdk::PostgresCollection, dokploy_sdk::Error>>,
    authority: PostgresTopologyAuthority,
) -> Result<(), DiscoverRemoteError> {
    if collections
        .iter()
        .any(|(candidate_environment_id, result)| {
            candidate_environment_id != environment_id
                && result.as_ref().is_ok_and(|collection| {
                    collection
                        .postgres()
                        .iter()
                        .any(|item| item.postgres_id == postgres.postgres_id)
                })
        })
    {
        return Err(DiscoverRemoteError::PostgresTopologyConflict);
    }
    let Some(collection) = collections.get(environment_id) else {
        return Err(DiscoverRemoteError::PostgresTopologyConflict);
    };
    let Ok(collection) = collection else {
        return Ok(());
    };
    let matching = collection
        .postgres()
        .iter()
        .find(|item| item.postgres_id == postgres.postgres_id);
    match matching {
        Some(item) if item.name == postgres.name => Ok(()),
        Some(_) => Err(DiscoverRemoteError::PostgresTopologyConflict),
        None if authority == PostgresTopologyAuthority::Authoritative => {
            Err(DiscoverRemoteError::PostgresTopologyConflict)
        }
        None => Ok(()),
    }
}

fn postgres_collections_contain_id(
    remote_id: &RemoteId,
    collections: &BTreeMap<String, Result<dokploy_sdk::PostgresCollection, dokploy_sdk::Error>>,
) -> bool {
    collections.values().any(|result| {
        result.as_ref().is_ok_and(|collection| {
            collection
                .postgres()
                .iter()
                .any(|postgres| postgres.postgres_id.as_str() == remote_id.as_str())
        })
    })
}

fn validate_postgres_parent_change(
    address: &ResourceAddress,
    current_parent: &ResourceAddress,
    desired_parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
) -> Result<(), DiscoverRemoteError> {
    if current_parent == desired_parent {
        return Ok(());
    }
    let current_environment_id = trusted_environment_id(current_parent, compiled, state, topology);
    let desired_environment_id = trusted_environment_id(desired_parent, compiled, state, topology);
    if current_environment_id.is_some() && current_environment_id == desired_environment_id {
        return Ok(());
    }
    if desired_resource_for_observation(address, compiled).is_none() {
        return Ok(());
    }

    Err(DiscoverRemoteError::PostgresReparentUnsupported)
}

fn postgres_parent_from_state(
    address: &ResourceAddress,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    let resource = state
        .resource(address)
        .ok_or(DiscoverRemoteError::PostgresContainment)?;
    let parent = resource
        .containment()
        .ok_or(DiscoverRemoteError::PostgresContainment)?;
    if parent.kind() != ResourceKind::Environment {
        return Err(DiscoverRemoteError::PostgresContainment);
    }

    Ok(parent.clone())
}

fn postgres_parent_from_desired(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    if let Some(parent) = compiled.bindings().parent_of(address) {
        if parent.kind() == ResourceKind::Environment {
            return Ok(parent.clone());
        }
        return Err(DiscoverRemoteError::PostgresContainment);
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
            return Err(DiscoverRemoteError::PostgresContainment);
        }
        if state.resource(target).is_some() {
            return postgres_parent_from_state(target, state);
        }
    }
    let source = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == address)
        .map(|directive| directive.from());
    if let Some(source) = source {
        return postgres_parent_from_state(source, state);
    }
    postgres_parent_from_state(address, state)
}

fn redis_properties(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
) -> BTreeMap<PropertyPath, PropertyObservation> {
    let mut properties = BTreeMap::new();
    let Some(desired) = desired_resource_for_observation(address, compiled) else {
        return properties;
    };

    if desired.properties().contains_key(&PropertyPath::Password)
        && !desired.ignored_changes().contains(&PropertyPath::Password)
    {
        properties.insert(
            PropertyPath::Password,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        );
    }

    properties
}

fn validate_redis_collections(
    collections: &BTreeMap<String, Result<dokploy_sdk::RedisCollection, dokploy_sdk::Error>>,
) -> Result<(), DiscoverRemoteError> {
    let mut global_ids = BTreeSet::new();
    for (environment_id, collection) in collections
        .iter()
        .filter_map(|(id, result)| result.as_ref().ok().map(|collection| (id, collection)))
    {
        let mut scoped_names = BTreeSet::new();
        for redis in collection.redis() {
            let remote_id = RemoteId::new(redis.redis_id.as_str())
                .map_err(|_| DiscoverRemoteError::InvalidRedisId)?;
            if redis.environment_id.as_str() != environment_id {
                return Err(DiscoverRemoteError::RedisContainment);
            }
            if !global_ids.insert(remote_id) {
                return Err(DiscoverRemoteError::DuplicateRedisId);
            }
            if !scoped_names.insert(redis.name.as_str()) {
                return Err(DiscoverRemoteError::DuplicateRedisName);
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_redis_under_parent(
    address: &ResourceAddress,
    parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
    collections: &BTreeMap<String, Result<dokploy_sdk::RedisCollection, dokploy_sdk::Error>>,
    authority: RedisTopologyAuthority,
) -> Result<RemoteObservation, DiscoverRemoteError> {
    match effective_environment_observation(parent, compiled, state, topology) {
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
            if let Some(redis) = collection
                .redis()
                .iter()
                .find(|redis| redis.name == address.name().as_str())
            {
                let remote_id = RemoteId::new(redis.redis_id.as_str())
                    .map_err(|_| DiscoverRemoteError::InvalidRedisId)?;
                return Ok(RemoteObservation::Present(RemoteResource::new(
                    remote_id,
                    BTreeMap::new(),
                )));
            }
            if authority == RedisTopologyAuthority::Authoritative {
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

fn validate_direct_redis_against_collection(
    redis: &dokploy_sdk::RedisDetails,
    environment_id: &str,
    collections: &BTreeMap<String, Result<dokploy_sdk::RedisCollection, dokploy_sdk::Error>>,
    authority: RedisTopologyAuthority,
) -> Result<(), DiscoverRemoteError> {
    if collections
        .iter()
        .any(|(candidate_environment_id, result)| {
            candidate_environment_id != environment_id
                && result.as_ref().is_ok_and(|collection| {
                    collection
                        .redis()
                        .iter()
                        .any(|item| item.redis_id == redis.redis_id)
                })
        })
    {
        return Err(DiscoverRemoteError::RedisTopologyConflict);
    }
    let Some(collection) = collections.get(environment_id) else {
        return Err(DiscoverRemoteError::RedisTopologyConflict);
    };
    let Ok(collection) = collection else {
        return Ok(());
    };
    let matching = collection
        .redis()
        .iter()
        .find(|item| item.redis_id == redis.redis_id);
    match matching {
        Some(item) if item.name == redis.name => Ok(()),
        Some(_) => Err(DiscoverRemoteError::RedisTopologyConflict),
        None if authority == RedisTopologyAuthority::Authoritative => {
            Err(DiscoverRemoteError::RedisTopologyConflict)
        }
        None => Ok(()),
    }
}

fn redis_collections_contain_id(
    remote_id: &RemoteId,
    collections: &BTreeMap<String, Result<dokploy_sdk::RedisCollection, dokploy_sdk::Error>>,
) -> bool {
    collections.values().any(|result| {
        result.as_ref().is_ok_and(|collection| {
            collection
                .redis()
                .iter()
                .any(|redis| redis.redis_id.as_str() == remote_id.as_str())
        })
    })
}

fn validate_redis_parent_change(
    address: &ResourceAddress,
    current_parent: &ResourceAddress,
    desired_parent: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
    topology: &[(ResourceAddress, RemoteObservation)],
) -> Result<(), DiscoverRemoteError> {
    if current_parent == desired_parent {
        return Ok(());
    }
    let current_environment_id = trusted_environment_id(current_parent, compiled, state, topology);
    let desired_environment_id = trusted_environment_id(desired_parent, compiled, state, topology);
    if current_environment_id.is_some() && current_environment_id == desired_environment_id {
        return Ok(());
    }
    if desired_resource_for_observation(address, compiled).is_none() {
        return Ok(());
    }

    Err(DiscoverRemoteError::RedisReparentUnsupported)
}

fn redis_parent_from_state(
    address: &ResourceAddress,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    let resource = state
        .resource(address)
        .ok_or(DiscoverRemoteError::RedisContainment)?;
    let parent = resource
        .containment()
        .ok_or(DiscoverRemoteError::RedisContainment)?;
    if parent.kind() != ResourceKind::Environment {
        return Err(DiscoverRemoteError::RedisContainment);
    }

    Ok(parent.clone())
}

fn redis_parent_from_desired(
    address: &ResourceAddress,
    compiled: &CompiledDesired,
    state: &StateFile,
) -> Result<ResourceAddress, DiscoverRemoteError> {
    if let Some(parent) = compiled.bindings().parent_of(address) {
        if parent.kind() == ResourceKind::Environment {
            return Ok(parent.clone());
        }
        return Err(DiscoverRemoteError::RedisContainment);
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
            return Err(DiscoverRemoteError::RedisContainment);
        }
        if state.resource(target).is_some() {
            return redis_parent_from_state(target, state);
        }
    }
    let source = compiled
        .desired_state()
        .moves()
        .iter()
        .find(|directive| directive.to() == address)
        .map(|directive| directive.from());
    if let Some(source) = source {
        return redis_parent_from_state(source, state);
    }
    redis_parent_from_state(address, state)
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
    let parent = resource
        .containment()
        .ok_or(DiscoverRemoteError::EnvironmentContainment)?;
    if parent.kind() != ResourceKind::Project {
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
