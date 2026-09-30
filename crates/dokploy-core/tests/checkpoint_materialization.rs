use std::collections::BTreeMap;

use dokploy_core::{
    ComparableValue, ConfigDigest, DesiredResource, DesiredState, OwnedValue, PropertyPath,
    RemoteObservation, RemoteResource, RemoteState, SensitiveIntent, StoredState, plan,
};
use dokploy_state::{
    FingerprintKeyId, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, SensitiveFingerprint, SensitivePropertyPath, StateFile,
};
use semver::Version;
use uuid::Uuid;

#[test]
fn planned_project_create_materializes_the_exact_durable_resource() {
    let address: ResourceAddress = "project.platform".parse().expect("address is valid");
    let instance =
        InstanceIdentity::parse("https://deploy.example.com").expect("instance is valid");
    let desired = DesiredState::try_new(
        ConfigDigest::parse("a".repeat(64)).expect("digest is valid"),
        BTreeMap::from([(
            address.clone(),
            DesiredResource::new(BTreeMap::from([(
                PropertyPath::Description,
                OwnedValue::Value(
                    ComparableValue::try_from_json(serde_json::json!("Managed"))
                        .expect("description is comparable"),
                ),
            )])),
        )]),
    )
    .expect("desired state is valid");
    let stored = StoredState::absent(instance.clone());
    let remote = RemoteState::try_new(instance, [(address.clone(), RemoteObservation::Missing)])
        .expect("remote state is valid");
    let plan = plan(&desired, &stored, &remote);
    let checkpoint = plan.changes()[0]
        .checkpoint()
        .present()
        .expect("create has a present checkpoint");

    let state = checkpoint
        .materialize(
            &address,
            RemoteId::new("project-1").expect("remote ID is valid"),
        )
        .expect("checkpoint materializes");

    assert_eq!(state.remote_id().as_str(), "project-1");
    assert_eq!(state.kind(), address.kind());
    assert_eq!(
        state.last_applied().as_json(),
        &serde_json::json!({"description": "Managed"})
    );
    assert!(state.sensitive_inputs().is_empty());
}

#[test]
fn planned_sensitive_create_materializes_only_an_opaque_receipt() {
    let project: ResourceAddress = "project.platform".parse().expect("address is valid");
    let environment: ResourceAddress = "environment.production".parse().expect("address is valid");
    let redis: ResourceAddress = "redis.cache".parse().expect("address is valid");
    let instance =
        InstanceIdentity::parse("https://deploy.example.com").expect("instance is valid");
    let fingerprint = SensitiveFingerprint::new_v1(
        FingerprintKeyId::new(
            Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea6").expect("key ID is valid"),
        )
        .expect("key ID is non-nil"),
        [0xa5; 32],
    );
    let mut state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    state
        .upsert_resource(
            project.clone(),
            resource_state(ResourceKind::Project, "project-1", None),
        )
        .expect("project state is valid");
    state
        .upsert_resource(
            environment.clone(),
            resource_state(
                ResourceKind::Environment,
                "environment-1",
                Some(project.clone()),
            ),
        )
        .expect("environment state is valid");
    let desired = DesiredState::try_new(
        ConfigDigest::parse("b".repeat(64)).expect("digest is valid"),
        BTreeMap::from([
            (project.clone(), DesiredResource::new(BTreeMap::new())),
            (
                environment.clone(),
                DesiredResource::new(BTreeMap::new()).with_containment(Some(project.clone())),
            ),
            (
                redis.clone(),
                DesiredResource::new(BTreeMap::from([(
                    PropertyPath::Password,
                    OwnedValue::Sensitive(SensitiveIntent::from_fingerprint(fingerprint.clone())),
                )]))
                .with_containment(Some(environment.clone())),
            ),
        ]),
    )
    .expect("desired state is valid");
    let stored = StoredState::try_from_state(&state).expect("stored state projects");
    let remote = RemoteState::try_new(
        instance,
        [
            (
                project,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("project-1").expect("remote ID is valid"),
                    BTreeMap::new(),
                )),
            ),
            (
                environment,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("environment-1").expect("remote ID is valid"),
                    BTreeMap::new(),
                )),
            ),
            (redis.clone(), RemoteObservation::Missing),
        ],
    )
    .expect("remote state is valid");
    let plan = plan(&desired, &stored, &remote);
    let checkpoint = plan.changes()[0]
        .checkpoint()
        .present()
        .expect("create has a present checkpoint");

    let materialized = checkpoint
        .materialize(
            &redis,
            RemoteId::new("redis-1").expect("remote ID is valid"),
        )
        .expect("checkpoint materializes");

    assert_eq!(
        materialized.last_applied().as_json(),
        &serde_json::json!({})
    );
    assert_eq!(
        materialized
            .sensitive_inputs()
            .fingerprint(&SensitivePropertyPath::parse("password").expect("path is valid")),
        Some(&fingerprint)
    );
}

fn resource_state(
    kind: ResourceKind,
    remote_id: &str,
    containment: Option<ResourceAddress>,
) -> ResourceState {
    ResourceState::new(
        kind,
        RemoteId::new(remote_id).expect("remote ID is valid"),
        false,
        ManagedInputs::try_from_json(serde_json::json!({})).expect("inputs are valid"),
        containment,
        Vec::new(),
    )
}
