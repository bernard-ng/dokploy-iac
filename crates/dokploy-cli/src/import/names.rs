//! Stage 3 of project import: deterministic logical-name allocation.
//!
//! Addresses are `kind.name`, so a collision only matters within one kind.
//! Allocation is pure and depends only on the remote tree:
//!
//! 1. Every resource gets a base name, the slug of its natural remote name.
//! 2. Candidates are grouped by `(kind, base name)`. In any group larger than
//!    one, **every** member is prefixed, so no resource gets a privileged name
//!    and the result does not depend on listing order. Services take their
//!    environment's name as the prefix; leaves take their parent service's final
//!    name, which actually tells siblings of one environment apart.
//! 3. A name that still collides gets `-2`, `-3`, … in remote-id order.
//!
//! Tiers are allocated in containment order (environments, services, leaves)
//! because a prefix is the already-final name of the parent.

use std::collections::{BTreeMap, BTreeSet};

use dokploy_state::{ResourceAddress, ResourceKind, ResourceName};

use super::inventory::RemoteProject;
use super::project::Resolved;
use super::{ImportError, logical_name};

/// Final addresses for every resource of one project.
pub(super) struct Names {
    pub(super) project: ResourceAddress,
    environments: BTreeMap<String, ResourceAddress>,
    resources: BTreeMap<(ResourceKind, String), ResourceAddress>,
    renamed: Vec<Renamed>,
}

/// An address that differs from the slug of its remote name.
pub(super) struct Renamed {
    pub(super) address: ResourceAddress,
    pub(super) remote_name: String,
}

impl Names {
    pub(super) fn environment(&self, remote_id: &str) -> Result<&ResourceAddress, ImportError> {
        self.environments
            .get(remote_id)
            .ok_or(ImportError::InvalidRemoteTopology)
    }

    pub(super) fn resource(
        &self,
        kind: ResourceKind,
        remote_id: &str,
    ) -> Result<&ResourceAddress, ImportError> {
        self.resources
            .get(&(kind, remote_id.to_owned()))
            .ok_or(ImportError::InvalidRemoteTopology)
    }

    pub(super) fn renamed(&self) -> &[Renamed] {
        &self.renamed
    }
}

struct Candidate {
    kind: ResourceKind,
    remote_id: String,
    /// The natural remote text the base name was derived from.
    label: String,
    base: String,
    /// The final name of the containing resource, used to separate a collision.
    prefix: Option<String>,
}

impl Candidate {
    fn new(
        kind: ResourceKind,
        remote_id: &str,
        label: &str,
        prefix: Option<&ResourceAddress>,
    ) -> Self {
        Self {
            kind,
            remote_id: remote_id.to_owned(),
            label: label.to_owned(),
            base: logical_name(label).as_str().to_owned(),
            prefix: prefix.map(|address| address.name().as_str().to_owned()),
        }
    }
}

/// Allocates the final address of every resource in the project.
pub(super) fn allocate(project: &RemoteProject, resolved: &Resolved) -> Result<Names, ImportError> {
    let mut names = Names {
        project: ResourceAddress::new(ResourceKind::Project, logical_name(&project.project.name)),
        environments: BTreeMap::new(),
        resources: BTreeMap::new(),
        renamed: Vec::new(),
    };

    // Environments.
    let candidates = project
        .environments
        .iter()
        .map(|environment| {
            Candidate::new(
                ResourceKind::Environment,
                environment.details.environment_id.as_str(),
                &environment.details.name,
                None,
            )
        })
        .collect::<Vec<_>>();
    for (candidate, address) in assign(&candidates, &mut names.renamed)? {
        names
            .environments
            .insert(candidate.remote_id.clone(), address);
    }

    // Services, prefixed with their environment on collision.
    let mut candidates = Vec::new();
    for environment in &project.environments {
        let environment_address = names.environment(environment.details.environment_id.as_str())?;
        for service in &environment.services {
            candidates.push(Candidate::new(
                service.detail.kind(),
                service.detail.remote_id(),
                service.detail.name(),
                Some(environment_address),
            ));
        }
    }
    for (candidate, address) in assign(&candidates, &mut names.renamed)? {
        names
            .resources
            .insert((candidate.kind, candidate.remote_id.clone()), address);
    }

    // Leaves, prefixed with their parent service on collision.
    let mut candidates = Vec::new();
    for service in project
        .environments
        .iter()
        .flat_map(|environment| &environment.services)
    {
        let parent = names.resource(service.detail.kind(), service.detail.remote_id())?;
        let parent_name = service.detail.name();
        for domain in &service.domains {
            candidates.push(Candidate::new(
                ResourceKind::Domain,
                domain.domain_id.as_str(),
                &domain.host,
                Some(parent),
            ));
        }
        for port in &service.ports {
            candidates.push(Candidate::new(
                ResourceKind::Port,
                port.port_id.as_str(),
                &format!(
                    "{}-{}",
                    port.published_port,
                    super::port_protocol_label(port.protocol)
                ),
                Some(parent),
            ));
        }
        for redirect in &service.redirects {
            candidates.push(Candidate::new(
                ResourceKind::Redirect,
                redirect.redirect_id.as_str(),
                &redirect.regex,
                Some(parent),
            ));
        }
        for entry in &service.security {
            candidates.push(Candidate::new(
                ResourceKind::Security,
                entry.security_id.as_str(),
                &entry.username,
                Some(parent),
            ));
        }
        for mount in &service.mounts {
            candidates.push(Candidate::new(
                ResourceKind::Mount,
                mount.mount_id.as_str(),
                &mount.mount_path,
                Some(parent),
            ));
        }
        for schedule in &service.schedules {
            candidates.push(Candidate::new(
                ResourceKind::Schedule,
                schedule.schedule_id.as_str(),
                &schedule.name,
                Some(parent),
            ));
        }
        for backup in &service.backups {
            let destination = resolved.destination(backup.backup_id.as_str())?;
            let tail = if backup.prefix.is_empty() {
                destination
            } else {
                backup.prefix.as_str()
            };
            candidates.push(Candidate::new(
                ResourceKind::Backup,
                backup.backup_id.as_str(),
                &format!("{parent_name} {tail}"),
                Some(parent),
            ));
        }
    }
    for (candidate, address) in assign(&candidates, &mut names.renamed)? {
        names
            .resources
            .insert((candidate.kind, candidate.remote_id.clone()), address);
    }

    Ok(names)
}

/// Allocates one tier. Returns each candidate with its final address.
fn assign<'a>(
    candidates: &'a [Candidate],
    renamed: &mut Vec<Renamed>,
) -> Result<Vec<(&'a Candidate, ResourceAddress)>, ImportError> {
    let finals = allocate_tier(candidates);
    let mut assigned = Vec::with_capacity(candidates.len());
    for (candidate, name) in candidates.iter().zip(finals) {
        let name = ResourceName::new(name)?;
        let address = ResourceAddress::new(candidate.kind, name);
        if address.name().as_str() != candidate.base {
            renamed.push(Renamed {
                address: address.clone(),
                remote_name: candidate.label.clone(),
            });
        }
        assigned.push((candidate, address));
    }

    Ok(assigned)
}

/// The pure heart of allocation: one final name per candidate, in input order.
fn allocate_tier(candidates: &[Candidate]) -> Vec<String> {
    let mut group_sizes = BTreeMap::<(ResourceKind, &str), usize>::new();
    for candidate in candidates {
        *group_sizes
            .entry((candidate.kind, candidate.base.as_str()))
            .or_default() += 1;
    }
    let desired = candidates
        .iter()
        .map(|candidate| {
            match (
                &candidate.prefix,
                group_sizes[&(candidate.kind, candidate.base.as_str())],
            ) {
                (Some(prefix), size) if size > 1 => format!("{prefix}-{}", candidate.base),
                _ => candidate.base.clone(),
            }
        })
        .collect::<Vec<_>>();

    let mut desired_sizes = BTreeMap::<(ResourceKind, &str), usize>::new();
    for (candidate, name) in candidates.iter().zip(&desired) {
        *desired_sizes
            .entry((candidate.kind, name.as_str()))
            .or_default() += 1;
    }

    // Names that are already unique are final, so a suffix can never take one.
    let mut taken = BTreeSet::<(ResourceKind, String)>::new();
    let mut finals = vec![String::new(); candidates.len()];
    let mut pending = Vec::new();
    for (index, (candidate, name)) in candidates.iter().zip(&desired).enumerate() {
        if desired_sizes[&(candidate.kind, name.as_str())] == 1 {
            taken.insert((candidate.kind, name.clone()));
            finals[index] = name.clone();
        } else {
            pending.push(index);
        }
    }

    // Residual collisions: the lowest remote id keeps the name; the rest get a counter.
    pending.sort_by(|left, right| {
        (
            candidates[*left].kind,
            &desired[*left],
            &candidates[*left].remote_id,
        )
            .cmp(&(
                candidates[*right].kind,
                &desired[*right],
                &candidates[*right].remote_id,
            ))
    });
    for index in pending {
        let kind = candidates[index].kind;
        let mut name = desired[index].clone();
        let mut counter = 2_u32;
        while taken.contains(&(kind, name.clone())) {
            name = format!("{}-{counter}", desired[index]);
            counter += 1;
        }
        taken.insert((kind, name.clone()));
        finals[index] = name;
    }

    finals
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(kind: ResourceKind, id: &str, name: &str, environment: &str) -> Candidate {
        let environment = ResourceAddress::new(
            ResourceKind::Environment,
            ResourceName::new(environment).unwrap(),
        );
        Candidate::new(kind, id, name, Some(&environment))
    }

    #[test]
    fn unique_names_are_the_slug_of_the_remote_name() {
        let names = allocate_tier(&[
            service(ResourceKind::Application, "a", "API Server", "production"),
            service(ResourceKind::Application, "b", "worker", "production"),
        ]);

        assert_eq!(names, ["api-server", "worker"]);
    }

    #[test]
    fn a_collision_prefixes_every_member_and_ignores_listing_order() {
        let forward = allocate_tier(&[
            service(ResourceKind::Application, "a", "api", "production"),
            service(ResourceKind::Application, "b", "api", "staging"),
        ]);
        let reverse = allocate_tier(&[
            service(ResourceKind::Application, "b", "api", "staging"),
            service(ResourceKind::Application, "a", "api", "production"),
        ]);

        assert_eq!(forward, ["production-api", "staging-api"]);
        assert_eq!(reverse, ["staging-api", "production-api"]);
    }

    #[test]
    fn the_same_name_in_different_kinds_is_not_a_collision() {
        let names = allocate_tier(&[
            service(ResourceKind::Application, "a", "api", "production"),
            service(ResourceKind::Postgres, "b", "api", "production"),
        ]);

        assert_eq!(names, ["api", "api"]);
    }

    #[test]
    fn a_name_that_still_collides_is_numbered_in_remote_id_order() {
        let names = allocate_tier(&[
            service(ResourceKind::Application, "id-3", "api", "production"),
            service(ResourceKind::Application, "id-1", "api", "production"),
            service(ResourceKind::Application, "id-2", "api", "production"),
        ]);

        assert_eq!(
            names,
            ["production-api-3", "production-api", "production-api-2"]
        );
    }

    #[test]
    fn a_suffix_never_takes_a_name_another_resource_already_owns() {
        let names = allocate_tier(&[
            service(ResourceKind::Application, "id-1", "api", "production"),
            service(ResourceKind::Application, "id-2", "api", "production"),
            service(
                ResourceKind::Application,
                "id-9",
                "production api 2",
                "staging",
            ),
        ]);

        assert_eq!(names[2], "production-api-2");
        assert_eq!(names[0], "production-api");
        assert_eq!(names[1], "production-api-3");
    }

    #[test]
    fn a_resource_without_a_prefix_falls_back_to_a_counter() {
        let environment = |id: &str| Candidate::new(ResourceKind::Environment, id, "Prod", None);

        assert_eq!(
            allocate_tier(&[environment("b"), environment("a")]),
            ["prod-2", "prod"]
        );
    }
}
