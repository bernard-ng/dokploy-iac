use std::str::FromStr;

use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateFile,
};
use semver::Version;
use serde_json::json;

#[test]
fn resource_addresses_have_one_canonical_text_form() {
    let address = ResourceAddress::from_str("application.api-server")
        .expect("a valid logical address must parse");

    assert_eq!(address.to_string(), "application.api-server");
    assert!(ResourceAddress::from_str("application").is_err());
    assert!(ResourceAddress::from_str("application.Api").is_err());
    assert!(ResourceAddress::from_str("application.api.extra").is_err());
    assert!(ResourceAddress::from_str("unknown.api").is_err());
}

#[test]
fn instance_identity_compares_normalized_base_urls() {
    let first = InstanceIdentity::parse("HTTPS://Deploy.Example.com:443/")
        .expect("a valid Dokploy URL must normalize");
    let second = InstanceIdentity::parse("https://deploy.example.com")
        .expect("the equivalent Dokploy URL must normalize");

    assert_eq!(first, second);
    assert_eq!(first.as_str(), "https://deploy.example.com/");
    assert_eq!(
        InstanceIdentity::parse("https://deploy.example.com/api/")
            .expect("an SDK API suffix must identify the same instance"),
        second
    );
    assert_eq!(
        InstanceIdentity::parse("https://deploy.example.com///")
            .expect("trailing root separators must normalize"),
        second
    );
    assert_eq!(
        InstanceIdentity::parse("https://deploy.example.com/dokploy///")
            .expect("trailing path separators must normalize")
            .as_str(),
        "https://deploy.example.com/dokploy"
    );
    assert!(InstanceIdentity::parse("ftp://deploy.example.com").is_err());
    assert!(InstanceIdentity::parse("https://user@deploy.example.com").is_err());
    assert!(InstanceIdentity::parse("https://deploy.example.com?tenant=other").is_err());
}

#[test]
fn managed_inputs_accept_objects_and_reject_secret_bearing_fields() {
    let inputs = ManagedInputs::try_from_json(json!({
        "description": "public",
        "source": { "branch": "main" }
    }))
    .expect("safe managed inputs must be accepted");

    assert_eq!(inputs.as_json()["source"]["branch"], "main");
    for key in [
        "databasePassword",
        "database_password",
        "api-key",
        "private_key",
        "build_secrets",
    ] {
        assert!(
            ManagedInputs::try_from_json(json!({ key: "secret" })).is_err(),
            "{key} must be recognized as secret-bearing"
        );
    }
    assert!(ManagedInputs::try_from_json(json!({ "nested": [{ "apiKey": "secret" }] })).is_err());
    assert!(ManagedInputs::try_from_json(json!("not-an-input-object")).is_err());
}

#[test]
fn remote_ids_are_non_empty_opaque_values() {
    let id = RemoteId::new("server-generated-id").expect("a remote identifier must be accepted");

    assert_eq!(id.as_str(), "server-generated-id");
    assert!(RemoteId::new("").is_err());
    assert!(RemoteId::new("  ").is_err());
}

#[test]
fn state_mutations_advance_one_lineage_serial() {
    let instance =
        InstanceIdentity::parse("https://deploy.example.com").expect("the instance must be valid");
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    let lineage = state.lineage();
    let address: ResourceAddress = "application.api".parse().expect("address must parse");
    let resource = resource_state(ResourceKind::Application, "application-1");

    assert_eq!(state.format_version(), 1);
    assert_eq!(state.serial(), 0);
    assert_eq!(state.revision().serial(), 0);
    assert_eq!(state.revision().lineage(), lineage);
    assert!(!lineage.is_nil());

    state
        .upsert_resource(address.clone(), resource)
        .expect("a matching resource must be inserted");
    assert_eq!(state.serial(), 1);
    assert_eq!(state.revision().serial(), 1);
    assert_eq!(state.lineage(), lineage);

    let missing: ResourceAddress = "application.missing".parse().expect("address must parse");
    assert_eq!(
        state
            .remove_resource(&missing)
            .expect("removing an absent resource is a no-op"),
        None
    );
    assert_eq!(state.serial(), 1);

    assert!(
        state
            .remove_resource(&address)
            .expect("the existing resource must be removed")
            .is_some()
    );
    assert_eq!(state.serial(), 2);
    assert_eq!(state.lineage(), lineage);
}

#[test]
fn state_rejects_resource_kind_and_instance_mismatches() {
    let instance =
        InstanceIdentity::parse("https://deploy.example.com").expect("the instance must be valid");
    let mut state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    let address: ResourceAddress = "postgres.main".parse().expect("address must parse");

    assert!(
        state
            .upsert_resource(
                address,
                resource_state(ResourceKind::Application, "application-1"),
            )
            .is_err()
    );
    assert_eq!(state.serial(), 0);

    state
        .ensure_instance(&instance)
        .expect("the owning instance must match");
    let other = InstanceIdentity::parse("https://staging.example.com")
        .expect("the second instance must be valid");
    assert!(state.ensure_instance(&other).is_err());
}

#[test]
fn state_serialization_is_deterministic_and_round_trips_invariants() {
    let instance = InstanceIdentity::parse("https://deploy.example.com/base/")
        .expect("the instance must be valid");
    let mut state = StateFile::new(Version::new(0, 1, 0), instance);
    let later: ResourceAddress = "application.zeta".parse().expect("address must parse");
    let earlier: ResourceAddress = "application.alpha".parse().expect("address must parse");

    state
        .upsert_resource(
            later.clone(),
            resource_state(ResourceKind::Application, "application-zeta"),
        )
        .expect("the first resource must be inserted");
    state
        .upsert_resource(
            earlier.clone(),
            resource_state(ResourceKind::Application, "application-alpha"),
        )
        .expect("the second resource must be inserted");

    let encoded = serde_json::to_string_pretty(&state).expect("state must serialize");
    let earlier_position = encoded
        .find("application.alpha")
        .expect("the earlier address must be serialized");
    let later_position = encoded
        .find("application.zeta")
        .expect("the later address must be serialized");
    assert!(earlier_position < later_position);

    let decoded: StateFile = serde_json::from_str(&encoded).expect("state must deserialize");
    assert_eq!(decoded, state);
    assert_eq!(decoded.serial(), 2);
    assert_eq!(decoded.cli_version(), &Version::new(0, 1, 0));
    assert_eq!(
        decoded
            .resource(&earlier)
            .expect("resource must exist")
            .kind(),
        ResourceKind::Application
    );

    let mut unsupported = serde_json::to_value(&state).expect("state must serialize");
    unsupported["formatVersion"] = json!(2);
    assert!(serde_json::from_value::<StateFile>(unsupported).is_err());
}

#[test]
fn state_deserialization_rejects_unknown_fields_and_nil_lineage() {
    let instance =
        InstanceIdentity::parse("https://deploy.example.com").expect("the instance must be valid");
    let state = StateFile::new(Version::new(0, 1, 0), instance);

    let mut unknown = serde_json::to_value(&state).expect("state must serialize");
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<StateFile>(unknown).is_err());

    let mut nil_lineage = serde_json::to_value(&state).expect("state must serialize");
    nil_lineage["lineage"] = json!("00000000-0000-0000-0000-000000000000");
    assert!(serde_json::from_value::<StateFile>(nil_lineage).is_err());
}

#[test]
fn resource_state_deserialization_rejects_unknown_fields() {
    let resource = resource_state(ResourceKind::Application, "application-1");
    let mut encoded = serde_json::to_value(resource).expect("resource state must serialize");
    encoded["unexpected"] = json!(true);

    assert!(serde_json::from_value::<ResourceState>(encoded).is_err());
}

#[test]
fn resource_state_canonicalizes_dependencies() {
    let first: ResourceAddress = "environment.production"
        .parse()
        .expect("address must parse");
    let second: ResourceAddress = "project.main".parse().expect("address must parse");
    let resource = ResourceState::new(
        ResourceKind::Application,
        RemoteId::new("application-1").expect("remote ID must be valid"),
        true,
        ManagedInputs::try_from_json(json!({})).expect("inputs must be safe"),
        vec![second.clone(), first.clone(), second.clone()],
    );

    assert!(resource.is_protected());
    assert_eq!(resource.dependencies(), &[first, second]);
}

fn resource_state(kind: ResourceKind, remote_id: &str) -> ResourceState {
    ResourceState::new(
        kind,
        RemoteId::new(remote_id).expect("remote ID must be valid"),
        false,
        ManagedInputs::try_from_json(json!({ "description": "managed" }))
            .expect("inputs must be safe"),
        Vec::new(),
    )
}
