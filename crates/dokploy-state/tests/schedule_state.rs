use dokploy_state::{
    FingerprintKeyId, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath,
};
use serde_json::json;
use uuid::Uuid;

const COMMAND_CANARY: &str = "raw-schedule-command-canary";
const SCRIPT_CANARY: &str = "raw-schedule-script-canary";

fn fingerprint(seed: u8) -> SensitiveFingerprint {
    let key_id =
        FingerprintKeyId::new(Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").unwrap())
            .unwrap();
    SensitiveFingerprint::new_v1(key_id, [seed; 32])
}

fn state(
    managed: serde_json::Value,
    sensitive: SensitiveInputs,
    containment: Option<&str>,
) -> Result<ResourceState, dokploy_state::ResourceStateError> {
    ResourceState::try_new(
        ResourceKind::Schedule,
        RemoteId::new("schedule-1").unwrap(),
        false,
        ManagedInputs::try_from_json(managed).unwrap(),
        sensitive,
        containment.map(|value| value.parse::<ResourceAddress>().unwrap()),
        vec!["application.api".parse().unwrap()],
    )
}

fn managed() -> serde_json::Value {
    json!({
        "target": "application.api",
        "name": "nightly",
        "cron_expression": "0 3 * * *",
        "shell_type": "bash",
        "enabled": false
    })
}

#[test]
fn schedule_state_is_contained_by_its_environment_not_its_target() {
    assert!(
        state(
            managed(),
            SensitiveInputs::default(),
            Some("environment.production")
        )
        .is_ok()
    );
    assert!(
        state(
            managed(),
            SensitiveInputs::default(),
            Some("application.api")
        )
        .is_err()
    );
    assert!(state(managed(), SensitiveInputs::default(), None).is_err());
    assert_eq!(ResourceKind::Schedule.to_string(), "schedule");
    assert_eq!(
        "schedule".parse::<ResourceKind>().unwrap(),
        ResourceKind::Schedule
    );
    assert!(ResourceKind::Application.is_schedule_target());
    assert!(ResourceKind::Compose.is_schedule_target());
    for kind in [
        ResourceKind::Project,
        ResourceKind::Environment,
        ResourceKind::Postgres,
        ResourceKind::MySql,
        ResourceKind::MariaDb,
        ResourceKind::Mongo,
        ResourceKind::LibSql,
        ResourceKind::Redis,
        ResourceKind::Domain,
        ResourceKind::Port,
        ResourceKind::Mount,
        ResourceKind::Schedule,
    ] {
        assert!(!kind.is_schedule_target());
    }
}

#[test]
fn command_and_script_are_canonical_sensitive_paths_and_never_managed_state() {
    let command = SensitivePropertyPath::parse("command").expect("command is a sensitive path");
    let script = SensitivePropertyPath::parse("script").expect("script is a sensitive path");
    assert_eq!(command.to_string(), "command");
    assert_eq!(script.to_string(), "script");

    let sensitive = SensitiveInputs::try_from_entries([
        (command.clone(), fingerprint(7)),
        (script.clone(), fingerprint(8)),
    ])
    .unwrap();
    let resource = state(managed(), sensitive, Some("environment.production"))
        .expect("receipt-only command and script are valid state");
    let serialized = serde_json::to_string(&resource).unwrap();
    assert!(!serialized.contains(COMMAND_CANARY));
    assert!(!serialized.contains(SCRIPT_CANARY));
    assert!(serialized.contains("\"command\""));
    assert!(serialized.contains("\"script\""));

    for value in [
        json!({"command": COMMAND_CANARY}),
        json!({"script": SCRIPT_CANARY}),
        json!({"nested": {"command": COMMAND_CANARY}}),
        json!({"nested": {"script": SCRIPT_CANARY}}),
    ] {
        let error = ManagedInputs::try_from_json(value)
            .expect_err("executable text cannot enter non-sensitive managed state");
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains(COMMAND_CANARY));
        assert!(!rendered.contains(SCRIPT_CANARY));
    }
    // A clear carries no text. Whether a field may be cleared is its spec's `nullable`.
    for cleared in [json!({"command": null}), json!({"script": null})] {
        ManagedInputs::try_from_json(cleared).expect("a clear stores no text");
    }
}

#[test]
fn schedule_state_rejects_overlapping_receipt_and_managed_keys() {
    // The managed-input constructor already refuses these keys, so an overlap
    // can never be constructed through a valid ManagedInputs value.
    assert!(ManagedInputs::try_from_json(json!({"command": "x", "name": "n"})).is_err());
    assert!(ManagedInputs::try_from_json(json!({"script": "x", "name": "n"})).is_err());
    assert!(
        SensitiveInputs::try_from_entries([(
            SensitivePropertyPath::parse("command").unwrap(),
            fingerprint(1)
        )])
        .is_ok()
    );
}

#[test]
fn schedule_sensitive_paths_round_trip_through_state_serialization() {
    let sensitive = SensitiveInputs::try_from_entries([(
        SensitivePropertyPath::parse("command").unwrap(),
        fingerprint(3),
    )])
    .unwrap();
    let resource = state(managed(), sensitive, Some("environment.production")).unwrap();
    let encoded = serde_json::to_value(&resource).unwrap();
    assert!(
        encoded["sensitiveInputs"]
            .as_object()
            .is_some_and(|inputs| inputs.contains_key("command") && !inputs.contains_key("script")),
        "only the declared receipt is stored: {encoded}"
    );
}
