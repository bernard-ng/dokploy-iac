//! Stage 1 of project import: read one project into a plain remote tree.
//!
//! This is the only module of the pipeline (with the final topology re-read)
//! that performs I/O. It builds no YAML and no state; it only reads every
//! resource below one project, sequentially and in a deterministic order, and
//! hands the result to the pure validation, naming, and build stages.
//!
//! Collections are the authority for leaves (Domain, Port, Redirect, Security,
//! Mount, Schedule, Backup): their items are the same typed records a direct
//! read returns, and reconciliation reads them through the same collections.

use std::collections::{BTreeMap, BTreeSet};

use dokploy_sdk::{
    ApplicationDetails, BackupDetails, BackupTarget, ComposeDetails, ComposeSearchItem, Dokploy,
    DomainDetails, EnvironmentDetails, EnvironmentId, EnvironmentSummary, LibSqlDetails,
    MariaDbDetails, MariaDbSearchItem, MongoDetails, MongoSearchItem, MountDetails, MySqlDetails,
    MySqlSearchItem, PortDetails, PostgresDetails, ProjectDetails, ProjectId, RedirectDetails,
    RedisDetails, ResponseField, ScheduleDetails, ScheduleTarget, SecurityDetails, ServiceTarget,
};
use dokploy_state::ResourceKind;

use super::ImportError;
use crate::external::ExternalDirectory;

/// Upper bound on resources read for one project, like the old interactive limit.
pub(super) const IMPORT_RESOURCE_LIMIT: usize = 10_000;

/// Upper bound on environments read for one project.
pub(super) const IMPORT_ENVIRONMENT_LIMIT: usize = 1_000;

/// One project and everything below it, exactly as Dokploy reported it.
pub(super) struct RemoteProject {
    pub(super) project: ProjectDetails,
    /// How many projects on the instance carry this project's name (itself included).
    ///
    /// Reconciliation finds the project by name, so a shared name could not plan.
    pub(super) projects_named_alike: usize,
    /// The project-scoped environment collection, kept for agreement with `project`.
    pub(super) listed_environments: Vec<EnvironmentSummary>,
    pub(super) topology: Topology,
    pub(super) environments: Vec<RemoteEnvironment>,
    pub(super) externals: ExternalDirectory,
}

pub(super) struct RemoteEnvironment {
    pub(super) details: EnvironmentDetails,
    /// Authoritative Compose collection kept for direct-vs-collection agreement.
    pub(super) compose_items: Vec<ComposeSearchItem>,
    pub(super) services: Vec<RemoteService>,
}

/// One service-level resource and all of its leaves.
pub(super) struct RemoteService {
    pub(super) detail: ServiceDetail,
    pub(super) domains: Vec<DomainDetails>,
    pub(super) ports: Vec<PortDetails>,
    pub(super) redirects: Vec<RedirectDetails>,
    pub(super) security: Vec<SecurityDetails>,
    pub(super) mounts: Vec<MountDetails>,
    /// Application Schedules, or every service's Schedules for a Compose.
    pub(super) schedules: Vec<ScheduleDetails>,
    pub(super) backups: Vec<BackupDetails>,
}

pub(super) enum ServiceDetail {
    Application(ApplicationDetails),
    Compose(ComposeDetails),
    Postgres(PostgresDetails),
    MySql(MySqlDetails),
    MariaDb(MariaDbDetails),
    Mongo(MongoDetails),
    LibSql(LibSqlDetails),
    Redis(RedisDetails),
}

impl ServiceDetail {
    pub(super) const fn kind(&self) -> ResourceKind {
        match self {
            Self::Application(_) => ResourceKind::Application,
            Self::Compose(_) => ResourceKind::Compose,
            Self::Postgres(_) => ResourceKind::Postgres,
            Self::MySql(_) => ResourceKind::MySql,
            Self::MariaDb(_) => ResourceKind::MariaDb,
            Self::Mongo(_) => ResourceKind::Mongo,
            Self::LibSql(_) => ResourceKind::LibSql,
            Self::Redis(_) => ResourceKind::Redis,
        }
    }

    pub(super) fn remote_id(&self) -> &str {
        match self {
            Self::Application(value) => value.application_id.as_str(),
            Self::Compose(value) => value.compose_id.as_str(),
            Self::Postgres(value) => value.postgres_id.as_str(),
            Self::MySql(value) => value.mysql_id.as_str(),
            Self::MariaDb(value) => value.mariadb_id.as_str(),
            Self::Mongo(value) => value.mongo_id.as_str(),
            Self::LibSql(value) => value.libsql_id.as_str(),
            Self::Redis(value) => value.redis_id.as_str(),
        }
    }

    pub(super) fn name(&self) -> &str {
        match self {
            Self::Application(value) => &value.name,
            Self::Compose(value) => &value.name,
            Self::Postgres(value) => &value.name,
            Self::MySql(value) => &value.name,
            Self::MariaDb(value) => &value.name,
            Self::Mongo(value) => &value.name,
            Self::LibSql(value) => &value.name,
            Self::Redis(value) => &value.name,
        }
    }

    pub(super) fn environment_id(&self) -> &EnvironmentId {
        match self {
            Self::Application(value) => &value.environment_id,
            Self::Compose(value) => &value.environment_id,
            Self::Postgres(value) => &value.environment_id,
            Self::MySql(value) => &value.environment_id,
            Self::MariaDb(value) => &value.environment_id,
            Self::Mongo(value) => &value.environment_id,
            Self::LibSql(value) => &value.environment_id,
            Self::Redis(value) => &value.environment_id,
        }
    }

    pub(super) fn service_target(&self) -> ServiceTarget {
        match self {
            Self::Application(value) => ServiceTarget::Application(value.application_id.clone()),
            Self::Compose(value) => ServiceTarget::Compose(value.compose_id.clone()),
            Self::Postgres(value) => ServiceTarget::Postgres(value.postgres_id.clone()),
            Self::MySql(value) => ServiceTarget::MySql(value.mysql_id.clone()),
            Self::MariaDb(value) => ServiceTarget::MariaDb(value.mariadb_id.clone()),
            Self::Mongo(value) => ServiceTarget::Mongo(value.mongo_id.clone()),
            Self::LibSql(value) => ServiceTarget::LibSql(value.libsql_id.clone()),
            Self::Redis(value) => ServiceTarget::Redis(value.redis_id.clone()),
        }
    }

    /// Redis is the only database without Backups; applications and Compose have none.
    pub(super) fn backup_target(&self) -> Option<BackupTarget> {
        match self {
            Self::Postgres(value) => Some(BackupTarget::Postgres(value.postgres_id.clone())),
            Self::MySql(value) => Some(BackupTarget::MySql(value.mysql_id.clone())),
            Self::MariaDb(value) => Some(BackupTarget::MariaDb(value.mariadb_id.clone())),
            Self::Mongo(value) => Some(BackupTarget::Mongo(value.mongo_id.clone())),
            Self::LibSql(value) => Some(BackupTarget::LibSql(value.libsql_id.clone())),
            Self::Application(_) | Self::Compose(_) | Self::Redis(_) => None,
        }
    }

    /// Whether the service declares a server placement (checked for resolvability).
    pub(super) fn server_id(&self) -> &ResponseField<dokploy_sdk::ServerId> {
        match self {
            Self::Application(value) => &value.server_id,
            Self::Compose(value) => &value.server_id,
            Self::Postgres(value) => &value.server_id,
            Self::MySql(value) => &value.server_id,
            Self::MariaDb(value) => &value.server_id,
            Self::Mongo(value) => &value.server_id,
            Self::LibSql(value) => &value.server_id,
            Self::Redis(value) => &value.server_id,
        }
    }
}

/// Everything about project membership that must not change during a crawl.
///
/// Volatile fields such as deployment status are deliberately excluded, so only
/// a real change in which resources exist (or are named) invalidates an import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Topology {
    project: (String, ResponseField<String>),
    environments: BTreeMap<String, EnvironmentMembers>,
}

/// An environment's name and the `(kind, remote id)` of every resource in it.
type EnvironmentMembers = (String, BTreeSet<(&'static str, String)>);

/// The project-level reads that define membership, kept so the crawl can use them.
pub(super) struct TopologyReads {
    pub(super) project: ProjectDetails,
    pub(super) environments: Vec<EnvironmentSummary>,
    composes: BTreeMap<EnvironmentId, Vec<ComposeSearchItem>>,
    mysql: BTreeMap<EnvironmentId, Vec<MySqlSearchItem>>,
    mariadb: BTreeMap<EnvironmentId, Vec<MariaDbSearchItem>>,
    mongo: BTreeMap<EnvironmentId, Vec<MongoSearchItem>>,
}

impl TopologyReads {
    pub(super) fn fingerprint(&self) -> Topology {
        let mut environments = BTreeMap::new();
        for environment in &self.project.environments {
            let mut members = BTreeSet::new();
            for value in &environment.applications {
                members.insert(("application", value.application_id.as_str().to_owned()));
            }
            for value in &environment.postgres {
                members.insert(("postgres", value.postgres_id.as_str().to_owned()));
            }
            for value in &environment.redis {
                members.insert(("redis", value.redis_id.as_str().to_owned()));
            }
            for value in &environment.libsql {
                members.insert(("libsql", value.libsql_id.as_str().to_owned()));
            }
            let id = &environment.environment_id;
            for value in self.composes.get(id).into_iter().flatten() {
                members.insert(("compose", value.compose_id.as_str().to_owned()));
            }
            for value in self.mysql.get(id).into_iter().flatten() {
                members.insert(("mysql", value.mysql_id.as_str().to_owned()));
            }
            for value in self.mariadb.get(id).into_iter().flatten() {
                members.insert(("mariadb", value.mariadb_id.as_str().to_owned()));
            }
            for value in self.mongo.get(id).into_iter().flatten() {
                members.insert(("mongo", value.mongo_id.as_str().to_owned()));
            }
            environments.insert(id.as_str().to_owned(), (environment.name.clone(), members));
        }

        Topology {
            project: (self.project.name.clone(), self.project.description.clone()),
            environments,
        }
    }
}

/// Reads the project and the authoritative membership collections of its environments.
pub(super) async fn read_topology(
    client: &Dokploy,
    project_id: &ProjectId,
) -> Result<TopologyReads, ImportError> {
    let project = client.projects().get(project_id.clone()).await?;
    if project.project_id != *project_id {
        return Err(ImportError::InvalidRemoteTopology);
    }
    if project.environments.len() > IMPORT_ENVIRONMENT_LIMIT {
        return Err(ImportError::TooLarge);
    }
    let environments = client
        .environments()
        .by_project(project_id.clone())
        .await?
        .environments()
        .to_vec();

    let mut reads = TopologyReads {
        project,
        environments,
        composes: BTreeMap::new(),
        mysql: BTreeMap::new(),
        mariadb: BTreeMap::new(),
        mongo: BTreeMap::new(),
    };
    for environment in &reads.project.environments {
        let id = environment.environment_id.clone();
        reads.composes.insert(
            id.clone(),
            client
                .composes()
                .by_environment(id.clone())
                .await?
                .composes()
                .to_vec(),
        );
        reads.mysql.insert(
            id.clone(),
            client
                .mysql()
                .by_environment(id.clone())
                .await?
                .mysql()
                .to_vec(),
        );
        reads.mariadb.insert(
            id.clone(),
            client
                .mariadb()
                .by_environment(id.clone())
                .await?
                .mariadb()
                .to_vec(),
        );
        reads.mongo.insert(
            id.clone(),
            client.mongo().by_environment(id).await?.mongo().to_vec(),
        );
    }

    Ok(reads)
}

/// Counts every resource read so a hostile or enormous project fails closed.
struct Budget(usize);

impl Budget {
    fn spend(&mut self, count: usize) -> Result<(), ImportError> {
        self.0 = self
            .0
            .checked_add(count)
            .filter(|total| *total <= IMPORT_RESOURCE_LIMIT)
            .ok_or(ImportError::TooLarge)?;

        Ok(())
    }
}

/// Reads one whole project. Sequential and deterministic by design.
pub(super) async fn crawl(
    client: &Dokploy,
    project_id: &ProjectId,
) -> Result<RemoteProject, ImportError> {
    let reads = read_topology(client, project_id).await?;
    let topology = reads.fingerprint();
    let mut budget = Budget(0);
    let projects_named_alike = client
        .projects()
        .all()
        .await?
        .projects()
        .iter()
        .filter(|candidate| candidate.name == reads.project.name)
        .count();

    let mut environments = Vec::new();
    for summary in &reads.project.environments {
        let id = &summary.environment_id;
        let details = client.environments().get(id.clone()).await?;
        if details.environment_id != *id || details.project_id != *project_id {
            return Err(ImportError::InvalidRemoteTopology);
        }

        let mut services = Vec::new();

        let mut ids = summary
            .applications
            .iter()
            .map(|value| value.application_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        for application_id in ids {
            let application = client.applications().get(application_id.clone()).await?;
            if application.application_id != application_id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let mut service = service(client, ServiceDetail::Application(application)).await?;
            service.domains = client
                .domains()
                .by_application(application_id.clone())
                .await?
                .domains()
                .to_vec();
            service.ports = client
                .ports()
                .by_application(application_id.clone())
                .await?
                .ports()
                .to_vec();
            service.redirects = client
                .redirects()
                .by_application(application_id.clone())
                .await?
                .redirects()
                .to_vec();
            service.security = client
                .security()
                .by_application(application_id.clone())
                .await?
                .entries()
                .to_vec();
            service.schedules = client
                .schedules()
                .by_target(ScheduleTarget::Application(application_id))
                .await?
                .schedules()
                .to_vec();
            budget.spend(service.leaf_count() + 1)?;
            services.push(service);
        }

        let mut ids = reads.composes[id]
            .iter()
            .map(|value| value.compose_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        for compose_id in ids {
            let compose = client.composes().get(compose_id.clone()).await?;
            if compose.compose_id != compose_id {
                return Err(ImportError::InvalidRemoteTopology);
            }
            let mut service = service(client, ServiceDetail::Compose(compose)).await?;
            service.schedules = client
                .schedules()
                .by_compose(compose_id)
                .await?
                .schedules()
                .to_vec();
            budget.spend(service.leaf_count() + 1)?;
            services.push(service);
        }

        macro_rules! databases {
            ($ids:expr, $client:ident, $accessor:ident, $id_field:ident, $variant:ident) => {{
                let mut ids = $ids;
                ids.sort();
                for database_id in ids {
                    let database = client.$accessor().get(database_id.clone()).await?;
                    if database.$id_field != database_id {
                        return Err(ImportError::InvalidRemoteTopology);
                    }
                    let service = service(client, ServiceDetail::$variant(database)).await?;
                    budget.spend(service.leaf_count() + 1)?;
                    services.push(service);
                }
            }};
        }
        databases!(
            summary
                .postgres
                .iter()
                .map(|value| value.postgres_id.clone())
                .collect::<Vec<_>>(),
            client,
            postgres,
            postgres_id,
            Postgres
        );
        databases!(
            reads.mysql[id]
                .iter()
                .map(|value| value.mysql_id.clone())
                .collect::<Vec<_>>(),
            client,
            mysql,
            mysql_id,
            MySql
        );
        databases!(
            reads.mariadb[id]
                .iter()
                .map(|value| value.mariadb_id.clone())
                .collect::<Vec<_>>(),
            client,
            mariadb,
            mariadb_id,
            MariaDb
        );
        databases!(
            reads.mongo[id]
                .iter()
                .map(|value| value.mongo_id.clone())
                .collect::<Vec<_>>(),
            client,
            mongo,
            mongo_id,
            Mongo
        );
        databases!(
            summary
                .libsql
                .iter()
                .map(|value| value.libsql_id.clone())
                .collect::<Vec<_>>(),
            client,
            libsql,
            libsql_id,
            LibSql
        );
        databases!(
            summary
                .redis
                .iter()
                .map(|value| value.redis_id.clone())
                .collect::<Vec<_>>(),
            client,
            redis,
            redis_id,
            Redis
        );

        environments.push(RemoteEnvironment {
            details,
            compose_items: reads.composes[id].clone(),
            services,
        });
    }

    // The external directory is loaded once per crawl, and only for the kinds
    // the project actually uses, so a project without associations adds no reads.
    let externals = ExternalDirectory::load(client, &required_externals(&environments)).await;

    Ok(RemoteProject {
        project: reads.project,
        projects_named_alike,
        listed_environments: reads.environments,
        topology,
        environments,
        externals,
    })
}

/// Reads the Mounts and Backups every service kind can have.
async fn service(client: &Dokploy, detail: ServiceDetail) -> Result<RemoteService, ImportError> {
    let mounts = client
        .mounts()
        .by_target(detail.service_target())
        .await?
        .mounts()
        .to_vec();
    let backups = match detail.backup_target() {
        Some(target) => client.backups().by_target(target).await?.backups().to_vec(),
        None => Vec::new(),
    };

    Ok(RemoteService {
        detail,
        domains: Vec::new(),
        ports: Vec::new(),
        redirects: Vec::new(),
        security: Vec::new(),
        mounts,
        schedules: Vec::new(),
        backups,
    })
}

impl RemoteService {
    fn leaf_count(&self) -> usize {
        self.domains.len()
            + self.ports.len()
            + self.redirects.len()
            + self.security.len()
            + self.mounts.len()
            + self.schedules.len()
            + self.backups.len()
    }
}

fn required_externals(
    environments: &[RemoteEnvironment],
) -> BTreeSet<dokploy_config::SelectorKind> {
    use dokploy_config::SelectorKind;

    let mut kinds = BTreeSet::new();
    for service in environments
        .iter()
        .flat_map(|environment| &environment.services)
    {
        if matches!(service.detail.server_id(), ResponseField::Value(_)) {
            kinds.insert(SelectorKind::Server);
        }
        if let ServiceDetail::Application(application) = &service.detail {
            if matches!(application.build_server_id, ResponseField::Value(_)) {
                kinds.insert(SelectorKind::Server);
            }
            if matches!(application.registry_id, ResponseField::Value(_))
                || matches!(application.build_registry_id, ResponseField::Value(_))
                || matches!(application.rollback_registry_id, ResponseField::Value(_))
            {
                kinds.insert(SelectorKind::Registry);
            }
        }
        if !service.backups.is_empty() {
            kinds.insert(SelectorKind::Destination);
        }
    }

    kinds
}
