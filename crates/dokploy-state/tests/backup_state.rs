use dokploy_state::{
    ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState, SensitiveInputs,
};
use serde_json::json;

const CANARY: &str = "backup-destination-access-key-canary";

fn state(
    managed: serde_json::Value,
    containment: Option<&str>,
) -> Result<ResourceState, dokploy_state::ResourceStateError> {
    ResourceState::try_new(
        ResourceKind::Backup,
        RemoteId::new("backup-1").unwrap(),
        false,
        ManagedInputs::try_from_json(managed).unwrap(),
        SensitiveInputs::default(),
        containment.map(|value| value.parse::<ResourceAddress>().unwrap()),
        vec!["postgres.main".parse().unwrap()],
    )
}

#[test]
fn backup_state_is_contained_by_its_environment_not_its_target() {
    let managed = json!({
        "target": "postgres.main",
        "destination": { "name": "offsite" },
        "schedule": "0 3 * * *",
        "prefix": "nightly",
        "database": "app",
        "enabled": false,
        "keep_latest": 7,
        "include_encryption_key": false
    });

    assert!(state(managed.clone(), Some("environment.production")).is_ok());
    assert!(state(managed.clone(), Some("postgres.main")).is_err());
    assert!(state(managed, None).is_err());
    assert_eq!(ResourceKind::Backup.to_string(), "backup");
    assert_eq!(
        "backup".parse::<ResourceKind>().unwrap(),
        ResourceKind::Backup
    );
    assert_eq!(
        ResourceKind::Backup.containment_parent_kind(),
        Some(ResourceKind::Environment)
    );
}

#[test]
fn backup_target_union_is_the_five_sdk_database_kinds() {
    for kind in [
        ResourceKind::Postgres,
        ResourceKind::MySql,
        ResourceKind::MariaDb,
        ResourceKind::Mongo,
        ResourceKind::LibSql,
    ] {
        assert!(kind.is_backup_target(), "{kind}");
    }
    for kind in [
        ResourceKind::Project,
        ResourceKind::Environment,
        ResourceKind::Application,
        ResourceKind::Compose,
        ResourceKind::Redis,
        ResourceKind::Domain,
        ResourceKind::Port,
        ResourceKind::Redirect,
        ResourceKind::Security,
        ResourceKind::Mount,
        ResourceKind::Backup,
    ] {
        assert!(!kind.is_backup_target(), "{kind}");
    }
}

#[test]
fn destination_credentials_can_never_enter_managed_backup_state() {
    for managed in [
        json!({"destination": { "accessKey": CANARY }}),
        json!({"destination": { "secretAccessKey": CANARY }}),
        json!({"nested": { "access_key": CANARY }}),
    ] {
        let error = ManagedInputs::try_from_json(managed)
            .expect_err("credential-bearing keys cannot enter managed state");
        assert!(!error.to_string().contains(CANARY));
    }
    assert!(
        ManagedInputs::try_from_json(json!({
            "destination": { "name": "offsite" },
            "prefix": "nightly"
        }))
        .is_ok()
    );
}
