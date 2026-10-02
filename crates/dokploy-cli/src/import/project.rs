//! Stages 2 and 4 of project import: pure validation and pure construction.
//!
//! Neither stage performs I/O. Validation proves the crawled tree is internally
//! consistent and resolves every external name; construction then walks the tree
//! in one fixed order and appends each resource to an [`ImportContext`].

use std::collections::{BTreeMap, BTreeSet};

use dokploy_config::ScheduleShellConfig;
use dokploy_sdk::{ScheduleTarget, ShellType};
use dokploy_state::{ResourceAddress, ResourceKind};

use super::context::ImportContext;
use super::inventory::{RemoteEnvironment, RemoteProject, RemoteService, ServiceDetail};
use super::mount::imported_source;
use super::names::Names;
use super::{
    ImportError, ImportedAssociations, associations_from, server_from,
    validate_compose_import_authority,
};
use dokploy_config::SelectorKind;

/// Every external name the project refers to, resolved once from the directory.
pub(super) struct Resolved {
    associations: BTreeMap<String, ImportedAssociations>,
    servers: BTreeMap<String, Option<String>>,
    destinations: BTreeMap<String, String>,
}

impl Resolved {
    pub(super) fn destination(&self, backup_id: &str) -> Result<&str, ImportError> {
        self.destinations
            .get(backup_id)
            .map(String::as_str)
            .ok_or(ImportError::ExternalAssociation)
    }

    fn associations(&self, application_id: &str) -> Result<&ImportedAssociations, ImportError> {
        self.associations
            .get(application_id)
            .ok_or(ImportError::ExternalAssociation)
    }

    fn server(&self, service_id: &str) -> Result<Option<&str>, ImportError> {
        self.servers
            .get(service_id)
            .map(Option::as_deref)
            .ok_or(ImportError::ExternalAssociation)
    }
}

/// Stage 2: proves the tree is consistent and resolves its external names.
///
/// Any failure aborts the whole import.
pub(super) fn validate(project: &RemoteProject) -> Result<Resolved, ImportError> {
    if project.project.name.is_empty() {
        return Err(ImportError::InvalidRemoteTopology);
    }
    if project.projects_named_alike != 1 {
        return Err(if project.projects_named_alike > 1 {
            ImportError::DuplicateName { kind: "project" }
        } else {
            ImportError::InvalidRemoteTopology
        });
    }

    // The nested topology and the project-scoped collection must name the same environments.
    let nested = project
        .project
        .environments
        .iter()
        .map(|environment| {
            (
                environment.environment_id.as_str(),
                environment.name.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let listed = project
        .listed_environments
        .iter()
        .map(|environment| {
            (
                environment.environment_id.as_str(),
                environment.name.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if nested.len() != project.project.environments.len()
        || listed.len() != project.listed_environments.len()
        || nested != listed
        || project.environments.len() != nested.len()
    {
        return Err(ImportError::InvalidRemoteTopology);
    }

    // Reconciliation tells same-kind siblings apart only by name.
    let mut environment_names = BTreeSet::new();
    for environment in &project.environments {
        if !environment_names.insert(environment.details.name.as_str()) {
            return Err(ImportError::DuplicateName {
                kind: "environment",
            });
        }
    }

    let mut seen = BTreeSet::new();
    let mut resolved = Resolved {
        associations: BTreeMap::new(),
        servers: BTreeMap::new(),
        destinations: BTreeMap::new(),
    };
    for environment in &project.environments {
        let id = &environment.details.environment_id;
        if environment.details.name.is_empty()
            || nested.get(id.as_str()) != Some(&environment.details.name.as_str())
        {
            return Err(ImportError::InvalidRemoteTopology);
        }
        let mut service_names = BTreeSet::new();
        for service in &environment.services {
            if !service_names.insert((service.detail.kind(), service.detail.name())) {
                return Err(ImportError::DuplicateName {
                    kind: super::kind_label(service.detail.kind()),
                });
            }
            validate_service(project, environment, service, &mut seen, &mut resolved)?;
        }
    }

    Ok(resolved)
}

fn validate_service(
    project: &RemoteProject,
    environment: &RemoteEnvironment,
    service: &RemoteService,
    seen: &mut BTreeSet<(ResourceKind, String)>,
    resolved: &mut Resolved,
) -> Result<(), ImportError> {
    let detail = &service.detail;
    if detail.name().is_empty()
        || detail.remote_id().is_empty()
        || *detail.environment_id() != environment.details.environment_id
        || !seen.insert((detail.kind(), detail.remote_id().to_owned()))
    {
        return Err(ImportError::InvalidRemoteTopology);
    }
    let id = detail.remote_id().to_owned();

    match detail {
        ServiceDetail::Application(application) => {
            resolved.associations.insert(
                id.clone(),
                associations_from(&project.externals, application)?,
            );
            for domain in &service.domains {
                if domain.application_id.as_ref() != Some(&application.application_id) {
                    return Err(ImportError::InvalidRemoteTopology);
                }
            }
            for port in &service.ports {
                if port.application_id != application.application_id {
                    return Err(ImportError::InvalidRemoteTopology);
                }
            }
            // A Redirect is found again by its regex and a Security entry by its
            // username, so a repeated key could never be matched to one record.
            let mut regexes = BTreeSet::new();
            for redirect in &service.redirects {
                if redirect.application_id != application.application_id
                    || !regexes.insert(redirect.regex.as_str())
                {
                    return Err(ImportError::InvalidRemoteTopology);
                }
            }
            let mut usernames = BTreeSet::new();
            for entry in &service.security {
                if entry.application_id != application.application_id
                    || !usernames.insert(entry.username.as_str())
                {
                    return Err(ImportError::InvalidRemoteTopology);
                }
            }
            for schedule in &service.schedules {
                if schedule.target
                    != ScheduleTarget::Application(application.application_id.clone())
                {
                    return Err(ImportError::InvalidRemoteTopology);
                }
            }
        }
        ServiceDetail::Compose(compose) => {
            validate_compose_import_authority(compose, &environment.compose_items)?;
            resolved.servers.insert(
                id.clone(),
                server_from(&project.externals, &compose.server_id)?,
            );
            // The configuration model keys a Compose's Schedules by one service
            // name (DOKCFG055), and the planner's target-scoped read rejects a
            // collection that spans several. Such a project cannot plan clean.
            let mut services = BTreeSet::new();
            for schedule in &service.schedules {
                match &schedule.target {
                    ScheduleTarget::Compose {
                        compose_id,
                        service_name,
                    } if *compose_id == compose.compose_id => {
                        services.insert(service_name.as_str());
                    }
                    _ => return Err(ImportError::InvalidRemoteTopology),
                }
            }
            if services.len() > 1 {
                return Err(ImportError::ComposeSchedulesSpanServices);
            }
        }
        ServiceDetail::Postgres(_)
        | ServiceDetail::MySql(_)
        | ServiceDetail::MariaDb(_)
        | ServiceDetail::Mongo(_)
        | ServiceDetail::LibSql(_)
        | ServiceDetail::Redis(_) => {
            resolved.servers.insert(
                id.clone(),
                server_from(&project.externals, detail.server_id())?,
            );
        }
    }

    let target = detail.service_target();
    for mount in &service.mounts {
        if mount.target != target {
            return Err(ImportError::InvalidRemoteTopology);
        }
    }
    for backup in &service.backups {
        if Some(&backup.target) != detail.backup_target().as_ref() {
            return Err(ImportError::InvalidRemoteTopology);
        }
        let destination = project
            .externals
            .unique_name_of(SelectorKind::Destination, backup.destination_id.as_str())
            .ok_or(ImportError::ExternalAssociation)?;
        resolved
            .destinations
            .insert(backup.backup_id.as_str().to_owned(), destination.to_owned());
    }

    // Leaf identities are unique across the whole project, not only per service.
    let leaves = service
        .domains
        .iter()
        .map(|value| (ResourceKind::Domain, value.domain_id.as_str()))
        .chain(
            service
                .ports
                .iter()
                .map(|value| (ResourceKind::Port, value.port_id.as_str())),
        )
        .chain(
            service
                .redirects
                .iter()
                .map(|value| (ResourceKind::Redirect, value.redirect_id.as_str())),
        )
        .chain(
            service
                .security
                .iter()
                .map(|value| (ResourceKind::Security, value.security_id.as_str())),
        )
        .chain(
            service
                .mounts
                .iter()
                .map(|value| (ResourceKind::Mount, value.mount_id.as_str())),
        )
        .chain(
            service
                .schedules
                .iter()
                .map(|value| (ResourceKind::Schedule, value.schedule_id.as_str())),
        )
        .chain(
            service
                .backups
                .iter()
                .map(|value| (ResourceKind::Backup, value.backup_id.as_str())),
        );
    for (kind, leaf_id) in leaves {
        if leaf_id.is_empty() || !seen.insert((kind, leaf_id.to_owned())) {
            return Err(ImportError::InvalidRemoteTopology);
        }
    }

    Ok(())
}

/// What stage 4 produced: the accumulated workspace and a per-environment census.
pub(super) struct Built {
    pub(super) context: ImportContext,
    pub(super) census: Vec<EnvironmentCensus>,
}

/// How many resources of each kind one environment contributed.
pub(super) struct EnvironmentCensus {
    pub(super) address: ResourceAddress,
    pub(super) counts: BTreeMap<ResourceKind, usize>,
}

/// Stage 4: walks the tree in a fixed order and appends every resource.
///
/// Order is environments by address, then services by kind and remote id, then
/// each service's leaves. Services are added before leaves because a leaf is
/// written into its already-present parent.
pub(super) fn build(
    project: &RemoteProject,
    names: &Names,
    resolved: &Resolved,
) -> Result<Built, ImportError> {
    let mut context =
        ImportContext::new(&project.project, project.tags.as_deref(), &names.project)?;
    let mut census = Vec::new();

    let mut environments = project.environments.iter().collect::<Vec<_>>();
    environments.sort_by(|left, right| {
        let left_name = names.environment(left.details.environment_id.as_str());
        let right_name = names.environment(right.details.environment_id.as_str());
        left_name.ok().cmp(&right_name.ok())
    });

    for environment in environments {
        let address = names
            .environment(environment.details.environment_id.as_str())?
            .clone();
        let scope = context.add_environment(&environment.details, address.clone())?;
        let mut counts = BTreeMap::from([(ResourceKind::Environment, 1_usize)]);
        let mut count = |kind| *counts.entry(kind).or_default() += 1;

        let mut services = environment.services.iter().collect::<Vec<_>>();
        services
            .sort_by_key(|service| (kind_rank(service.detail.kind()), service.detail.remote_id()));

        for service in &services {
            let service_address =
                names.resource(service.detail.kind(), service.detail.remote_id())?;
            match &service.detail {
                ServiceDetail::Application(application) => context.add_application(
                    &scope,
                    application,
                    resolved.associations(service.detail.remote_id())?,
                    service_address,
                )?,
                ServiceDetail::Compose(compose) => context.add_compose(
                    &scope,
                    compose,
                    resolved.server(service.detail.remote_id())?,
                    service_address,
                )?,
                ServiceDetail::Postgres(database) => context.add_postgres(
                    &scope,
                    database,
                    resolved.server(service.detail.remote_id())?,
                    service_address,
                )?,
                ServiceDetail::MySql(database) => context.add_mysql(
                    &scope,
                    database,
                    resolved.server(service.detail.remote_id())?,
                    service_address,
                )?,
                ServiceDetail::MariaDb(database) => context.add_mariadb(
                    &scope,
                    database,
                    resolved.server(service.detail.remote_id())?,
                    service_address,
                )?,
                ServiceDetail::Mongo(database) => context.add_mongo(
                    &scope,
                    database,
                    resolved.server(service.detail.remote_id())?,
                    service_address,
                )?,
                ServiceDetail::LibSql(database) => context.add_libsql(
                    &scope,
                    database,
                    resolved.server(service.detail.remote_id())?,
                    service_address,
                )?,
                ServiceDetail::Redis(database) => context.add_redis(
                    &scope,
                    database,
                    resolved.server(service.detail.remote_id())?,
                    service_address,
                )?,
            }
            count(service.detail.kind());
        }

        for service in &services {
            let service_address = names
                .resource(service.detail.kind(), service.detail.remote_id())?
                .clone();
            let leaf = |kind, id: &str| names.resource(kind, id).cloned();

            for domain in sorted(&service.domains, |value| value.domain_id.as_str()) {
                let address = leaf(ResourceKind::Domain, domain.domain_id.as_str())?;
                context.add_domain(&scope, &service_address, domain, &address)?;
                count(ResourceKind::Domain);
            }
            for port in sorted(&service.ports, |value| value.port_id.as_str()) {
                let address = leaf(ResourceKind::Port, port.port_id.as_str())?;
                context.add_port(&scope, &service_address, port, &address)?;
                count(ResourceKind::Port);
            }
            for redirect in sorted(&service.redirects, |value| value.redirect_id.as_str()) {
                let address = leaf(ResourceKind::Redirect, redirect.redirect_id.as_str())?;
                context.add_redirect(&scope, &service_address, redirect, &address)?;
                count(ResourceKind::Redirect);
            }
            for entry in sorted(&service.security, |value| value.security_id.as_str()) {
                let address = leaf(ResourceKind::Security, entry.security_id.as_str())?;
                context.add_security(&scope, &service_address, entry, &address)?;
                count(ResourceKind::Security);
            }
            for mount in sorted(&service.mounts, |value| value.mount_id.as_str()) {
                let address = leaf(ResourceKind::Mount, mount.mount_id.as_str())?;
                let (source, inputs) = imported_source(mount)?;
                context.add_mount(&scope, &service_address, mount, source, inputs, &address)?;
                count(ResourceKind::Mount);
            }
            for schedule in sorted(&service.schedules, |value| value.schedule_id.as_str()) {
                let address = leaf(ResourceKind::Schedule, schedule.schedule_id.as_str())?;
                let service_name = match &schedule.target {
                    ScheduleTarget::Application(_) => None,
                    ScheduleTarget::Compose { service_name, .. } => Some(service_name.clone()),
                };
                let shell_type = match schedule.shell_type {
                    ShellType::Bash => ScheduleShellConfig::Bash,
                    ShellType::Sh => ScheduleShellConfig::Sh,
                };
                context.add_schedule(
                    &scope,
                    &service_address,
                    schedule,
                    service_name,
                    shell_type,
                    &address,
                )?;
                count(ResourceKind::Schedule);
            }
            for backup in sorted(&service.backups, |value| value.backup_id.as_str()) {
                let address = leaf(ResourceKind::Backup, backup.backup_id.as_str())?;
                // A null flag cannot be written as a boolean, so it is never guessed.
                let enabled = backup.enabled.ok_or(ImportError::InvalidRemoteTopology)?;
                context.add_backup(
                    &scope,
                    &service_address,
                    backup,
                    resolved.destination(backup.backup_id.as_str())?.to_owned(),
                    enabled,
                    &address,
                )?;
                count(ResourceKind::Backup);
            }
        }

        census.push(EnvironmentCensus { address, counts });
    }

    Ok(Built { context, census })
}

fn sorted<T>(items: &[T], id: impl Fn(&T) -> &str) -> Vec<&T> {
    let mut items = items.iter().collect::<Vec<_>>();
    items.sort_by(|left, right| id(left).cmp(id(right)));
    items
}

/// The fixed kind order of the walk.
fn kind_rank(kind: ResourceKind) -> u8 {
    match kind {
        ResourceKind::Application => 0,
        ResourceKind::Compose => 1,
        ResourceKind::Postgres => 2,
        ResourceKind::MySql => 3,
        ResourceKind::MariaDb => 4,
        ResourceKind::Mongo => 5,
        ResourceKind::LibSql => 6,
        _ => 7,
    }
}
