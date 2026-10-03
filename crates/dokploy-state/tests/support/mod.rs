//! Kinds for the state tests. The state layer knows no kind of its own: every kind is
//! registered with its scope and containment parent (what a kind spec declares), so the
//! tests register the small hierarchy they need.

#![allow(dead_code)]

use std::sync::OnceLock;

use dokploy_state::{ResourceKind, StateScope};

pub struct Kinds {
    pub project: ResourceKind,
    pub environment: ResourceKind,
    pub application: ResourceKind,
    pub compose: ResourceKind,
    pub postgres: ResourceKind,
    pub mysql: ResourceKind,
    pub mariadb: ResourceKind,
    pub mongo: ResourceKind,
    pub libsql: ResourceKind,
    pub redis: ResourceKind,
    pub domain: ResourceKind,
    pub port: ResourceKind,
    pub redirect: ResourceKind,
    pub security: ResourceKind,
    pub mount: ResourceKind,
    pub schedule: ResourceKind,
    pub backup: ResourceKind,
    pub tag: ResourceKind,
}

pub fn kinds() -> &'static Kinds {
    static KINDS: OnceLock<Kinds> = OnceLock::new();
    KINDS.get_or_init(|| {
        let register = |name, scope, parent: &[ResourceKind]| {
            ResourceKind::register(name, scope, parent).expect("the test kind registers")
        };
        let project = register("project", StateScope::Project, &[]);
        let environment = register("environment", StateScope::Project, &[project]);
        let application = register("application", StateScope::Project, &[environment]);
        Kinds {
            project,
            environment,
            application,
            compose: register("compose", StateScope::Project, &[environment]),
            postgres: register("postgres", StateScope::Project, &[environment]),
            mysql: register("mysql", StateScope::Project, &[environment]),
            mariadb: register("mariadb", StateScope::Project, &[environment]),
            mongo: register("mongo", StateScope::Project, &[environment]),
            libsql: register("libsql", StateScope::Project, &[environment]),
            redis: register("redis", StateScope::Project, &[environment]),
            domain: register("domain", StateScope::Project, &[environment]),
            port: register("port", StateScope::Project, &[application]),
            redirect: register("redirect", StateScope::Project, &[application]),
            security: register("security", StateScope::Project, &[application]),
            mount: register("mount", StateScope::Project, &[environment]),
            schedule: register("schedule", StateScope::Project, &[environment]),
            backup: register("backup", StateScope::Project, &[environment]),
            tag: register("tag", StateScope::Settings, &[]),
        }
    })
}

/// The document most tests track their state in.
pub fn project_document() -> dokploy_state::DocumentId {
    dokploy_state::DocumentId::Project(
        dokploy_state::ResourceName::new("shop").expect("a valid slug"),
    )
}
