use dokploy_state::{
    FingerprintKeyId, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath,
};
use serde_json::json;
use uuid::Uuid;

const CONTENT_CANARY: &str = "raw-mount-content-canary";

fn state(
    managed: serde_json::Value,
    sensitive: SensitiveInputs,
    containment: Option<&str>,
) -> Result<ResourceState, dokploy_state::ResourceStateError> {
    ResourceState::try_new(
        ResourceKind::Mount,
        RemoteId::new("mount-1").unwrap(),
        false,
        ManagedInputs::try_from_json(managed).unwrap(),
        sensitive,
        containment.map(|value| value.parse::<ResourceAddress>().unwrap()),
        vec!["application.api".parse().unwrap()],
    )
}

#[test]
fn mount_state_is_contained_by_its_environment_not_its_target() {
    let managed = json!({
        "target": "application.api",
        "mount_type": "volume",
        "mount_path": "/data",
        "volume_name": "api-data"
    });

    assert!(
        state(
            managed.clone(),
            SensitiveInputs::default(),
            Some("environment.production")
        )
        .is_ok()
    );
    assert!(
        state(
            managed.clone(),
            SensitiveInputs::default(),
            Some("application.api")
        )
        .is_err()
    );
    assert!(state(managed, SensitiveInputs::default(), None).is_err());
    assert_eq!(ResourceKind::Mount.to_string(), "mount");
    assert_eq!(
        "mount".parse::<ResourceKind>().unwrap(),
        ResourceKind::Mount
    );
    assert!(ResourceKind::Application.is_mount_target());
    assert!(ResourceKind::Redis.is_mount_target());
    for kind in [
        ResourceKind::Project,
        ResourceKind::Environment,
        ResourceKind::Domain,
        ResourceKind::Port,
        ResourceKind::Mount,
    ] {
        assert!(!kind.is_mount_target());
    }
}

#[test]
fn file_content_is_a_canonical_sensitive_path_and_never_managed_state() {
    let path = SensitivePropertyPath::parse("content").expect("content is a sensitive path");
    assert_eq!(path.to_string(), "content");

    let key_id =
        FingerprintKeyId::new(Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").unwrap())
            .unwrap();
    let sensitive =
        SensitiveInputs::try_from_entries([(path, SensitiveFingerprint::new_v1(key_id, [7; 32]))])
            .unwrap();
    let resource = state(
        json!({"target": "application.api", "mount_type": "file", "mount_path": "/x", "file_path": "x"}),
        sensitive,
        Some("environment.production"),
    )
    .expect("receipt-only content is valid state");
    let serialized = serde_json::to_string(&resource).unwrap();
    assert!(!serialized.contains(CONTENT_CANARY));
    assert!(
        serialized.contains("\"content\""),
        "only the opaque receipt path is stored"
    );

    for managed in [
        json!({"content": CONTENT_CANARY}),
        json!({"content": null}),
        json!({"nested": {"content": CONTENT_CANARY}}),
    ] {
        let error = ManagedInputs::try_from_json(managed)
            .expect_err("content bytes cannot enter non-sensitive managed state");
        assert!(!error.to_string().contains(CONTENT_CANARY));
    }
}
