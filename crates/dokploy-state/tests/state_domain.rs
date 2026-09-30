use std::str::FromStr;

use dokploy_state::{
    FingerprintKeyId, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath, StateFile,
};
use semver::Version;
use serde_json::json;
use uuid::Uuid;

#[test]
fn sensitive_fingerprint_has_one_canonical_receipt_format() {
    let key_id = FingerprintKeyId::new(
        Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").expect("UUID must parse"),
    )
    .expect("non-nil key ID must be valid");
    let fingerprint = SensitiveFingerprint::new_v1(key_id, [0xab; 32]);

    let encoded = serde_json::to_string(&fingerprint).expect("fingerprint must serialize");
    assert_eq!(
        encoded,
        concat!(
            r#"{"version":"hmac-sha256-v1","keyId":"0199a0c8-2351-7c31-8899-2c8f81983ea5","mac":""#,
            "abababababababababababababababababababababababababababababababab",
            r#""}"#
        )
    );
    let decoded: SensitiveFingerprint =
        serde_json::from_str(&encoded).expect("canonical receipt must deserialize");
    assert_eq!(decoded, fingerprint);
}

#[test]
fn sensitive_fingerprint_rejects_noncanonical_or_malformed_receipts() {
    let valid_key = "0199a0c8-2351-7c31-8899-2c8f81983ea5";
    let valid_mac = "abababababababababababababababababababababababababababababababab";

    for invalid in [
        format!(r#"{{"version":"hmac-sha256-v2","keyId":"{valid_key}","mac":"{valid_mac}"}}"#),
        format!(
            r#"{{"version":"hmac-sha256-v1","keyId":"00000000-0000-0000-0000-000000000000","mac":"{valid_mac}"}}"#
        ),
        format!(r#"{{"version":"hmac-sha256-v1","keyId":"not-a-uuid","mac":"{valid_mac}"}}"#),
        format!(
            r#"{{"version":"hmac-sha256-v1","keyId":"0199A0C8-2351-7C31-8899-2C8F81983EA5","mac":"{valid_mac}"}}"#
        ),
        format!(
            r#"{{"version":"hmac-sha256-v1","keyId":"{valid_key}","mac":"ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB"}}"#
        ),
        format!(r#"{{"version":"hmac-sha256-v1","keyId":"{valid_key}","mac":"ab"}}"#),
        format!(
            r#"{{"version":"hmac-sha256-v1","keyId":"{valid_key}","mac":"{valid_mac}","extra":true}}"#
        ),
    ] {
        assert!(
            serde_json::from_str::<SensitiveFingerprint>(&invalid).is_err(),
            "receipt must fail closed"
        );
    }
}

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
fn managed_inputs_store_only_canonical_sensitive_clears() {
    for allowed in [
        json!({ "password": null }),
        json!({ "environment": null }),
        json!({ "environment": {} }),
        json!({ "environment": { "API_TOKEN": null, "_INTERNAL": null } }),
    ] {
        let inputs = ManagedInputs::try_from_json(allowed.clone())
            .expect("canonical sensitive clears must be durable");
        assert_eq!(inputs.as_json(), &allowed);
    }

    for rejected in [
        json!({ "password": "raw-password-canary" }),
        json!({ "environment": "raw-environment-canary" }),
        json!({ "environment": { "API_TOKEN": "raw-env-canary" } }),
        json!({ "environment": { "lowercase": null } }),
        json!({ "nested": { "password": null } }),
    ] {
        let debug = format!(
            "{:?}",
            ManagedInputs::try_from_json(rejected)
                .expect_err("raw or noncanonical sensitive input must fail closed")
        );
        assert!(!debug.contains("raw-password-canary"));
        assert!(!debug.contains("raw-environment-canary"));
        assert!(!debug.contains("raw-env-canary"));
    }
}

#[test]
fn sensitive_inputs_validate_paths_and_resource_state_rejects_ownership_overlap() {
    let receipt = fingerprint(0x5a);
    let password = SensitivePropertyPath::parse("password").expect("password path must parse");
    let environment =
        SensitivePropertyPath::parse("environment.API_TOKEN").expect("environment path must parse");
    let sensitive = SensitiveInputs::try_from_entries([
        (environment.clone(), receipt.clone()),
        (password.clone(), receipt),
    ])
    .expect("unique sensitive paths must be accepted");

    assert!(sensitive.fingerprint(&password).is_some());
    assert!(sensitive.fingerprint(&environment).is_some());
    assert!(
        SensitiveInputs::try_from_entries([
            (password.clone(), fingerprint(0x01)),
            (password.clone(), fingerprint(0x02)),
        ])
        .is_err()
    );
    for invalid in [
        "environment",
        "environment.lowercase",
        "environment.API-TOKEN",
        "description",
        "password.extra",
    ] {
        assert!(SensitivePropertyPath::parse(invalid).is_err());
    }

    for managed in [
        json!({ "password": null }),
        json!({ "environment": null }),
        json!({ "environment": {} }),
        json!({ "environment": { "API_TOKEN": null } }),
    ] {
        let error = ResourceState::try_new(
            ResourceKind::Application,
            RemoteId::new("application-1").expect("remote ID must be valid"),
            false,
            ManagedInputs::try_from_json(managed).expect("clear must be valid"),
            sensitive.clone(),
            Some(
                "environment.production"
                    .parse()
                    .expect("address must parse"),
            ),
            Vec::new(),
        )
        .expect_err("managed and fingerprinted ownership must not overlap");
        assert!(!format!("{error:?}").contains("5a5a"));
    }

    ResourceState::try_new(
        ResourceKind::Application,
        RemoteId::new("application-1").expect("remote ID must be valid"),
        false,
        ManagedInputs::try_from_json(json!({ "environment": { "FEATURE_FLAG": null } }))
            .expect("clear must be valid"),
        SensitiveInputs::try_from_entries([(
            SensitivePropertyPath::parse("environment.API_TOKEN").expect("path must parse"),
            fingerprint(0x6b),
        )])
        .expect("sensitive input must be valid"),
        Some(
            "environment.production"
                .parse()
                .expect("address must parse"),
        ),
        Vec::new(),
    )
    .expect("different environment paths may own clear and receipt intents");
}

#[test]
fn sensitive_receipts_and_state_debug_output_are_redacted() {
    let fingerprint = fingerprint(0xcd);
    let inputs = SensitiveInputs::try_from_entries([(
        SensitivePropertyPath::parse("password").expect("path must parse"),
        fingerprint.clone(),
    )])
    .expect("sensitive input must be valid");
    let resource = ResourceState::try_new(
        ResourceKind::Postgres,
        RemoteId::new("remote-sensitive-canary").expect("remote ID must be valid"),
        false,
        ManagedInputs::try_from_json(json!({})).expect("managed inputs must be valid"),
        inputs.clone(),
        Some(
            "environment.production"
                .parse()
                .expect("address must parse"),
        ),
        Vec::new(),
    )
    .expect("disjoint inputs must be valid");
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    state
        .upsert_resource(
            "postgres.main".parse().expect("address must parse"),
            resource.clone(),
        )
        .expect("resource must be inserted");

    let encoded = serde_json::to_vec_pretty(&state).expect("state must serialize");
    assert_eq!(
        encoded,
        serde_json::to_vec_pretty(&state).expect("state serialization must be deterministic")
    );
    assert_eq!(
        StateFile::from_json_slice(&encoded).expect("sensitive state must round-trip"),
        state
    );
    assert!(
        !String::from_utf8(encoded)
            .expect("state JSON must be UTF-8")
            .contains("raw-sensitive-input-canary")
    );

    for debug in [
        format!("{fingerprint:?}"),
        format!("{inputs:?}"),
        format!("{resource:?}"),
        format!("{state:?}"),
    ] {
        assert!(!debug.contains("cdcd"));
        assert!(!debug.contains("remote-sensitive-canary"));
    }
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

    assert_eq!(state.format_version(), 3);
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

    let decoded = StateFile::from_json_slice(encoded.as_bytes()).expect("state must deserialize");
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
    unsupported["formatVersion"] = json!(4);
    let unsupported = serde_json::to_vec(&unsupported).expect("state JSON must serialize");
    assert!(StateFile::from_json_slice(&unsupported).is_err());

    let mut obsolete = serde_json::to_value(&state).expect("state must serialize");
    obsolete["formatVersion"] = json!(2);
    let obsolete = serde_json::to_vec(&obsolete).expect("state JSON must serialize");
    assert!(StateFile::from_json_slice(&obsolete).is_err());
}

#[test]
fn state_deserialization_rejects_unknown_fields_and_nil_lineage() {
    let instance =
        InstanceIdentity::parse("https://deploy.example.com").expect("the instance must be valid");
    let state = StateFile::new(Version::new(0, 1, 0), instance);

    let mut unknown = serde_json::to_value(&state).expect("state must serialize");
    unknown["unexpected"] = json!(true);
    let unknown = serde_json::to_vec(&unknown).expect("state JSON must serialize");
    assert!(StateFile::from_json_slice(&unknown).is_err());

    let mut nil_lineage = serde_json::to_value(&state).expect("state must serialize");
    nil_lineage["lineage"] = json!("00000000-0000-0000-0000-000000000000");
    let nil_lineage = serde_json::to_vec(&nil_lineage).expect("state JSON must serialize");
    assert!(StateFile::from_json_slice(&nil_lineage).is_err());
}

#[test]
fn state_deserialization_rejects_raw_sensitive_canaries() {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    state
        .upsert_resource(
            "postgres.main".parse().expect("address must parse"),
            ResourceState::new(
                ResourceKind::Postgres,
                RemoteId::new("postgres-1").expect("remote ID must be valid"),
                false,
                ManagedInputs::try_from_json(json!({})).expect("managed inputs must be valid"),
                Some(
                    "environment.production"
                        .parse()
                        .expect("address must parse"),
                ),
                Vec::new(),
            ),
        )
        .expect("resource must be inserted");
    let mut encoded = serde_json::to_value(&state).expect("state must serialize");
    encoded["resources"]["postgres.main"]["lastApplied"] =
        json!({ "password": "raw-state-password-canary" });
    let bytes = serde_json::to_vec(&encoded).expect("tampered state must serialize");

    let error =
        StateFile::from_json_slice(&bytes).expect_err("raw sensitive state input must fail closed");

    assert!(!format!("{error:?}").contains("raw-state-password-canary"));
}

#[test]
fn resource_state_deserialization_rejects_unknown_fields() {
    let resource = resource_state(ResourceKind::Application, "application-1");
    let mut encoded = serde_json::to_value(resource).expect("resource state must serialize");
    encoded["unexpected"] = json!(true);

    assert!(serde_json::from_value::<ResourceState>(encoded).is_err());
}

#[test]
fn resource_state_deserialization_requires_explicit_containment() {
    let resource = resource_state(ResourceKind::Application, "application-1");
    let mut encoded = serde_json::to_value(resource).expect("resource state must serialize");
    encoded
        .as_object_mut()
        .expect("resource state must be an object")
        .remove("containment");

    assert!(serde_json::from_value::<ResourceState>(encoded).is_err());

    let project = resource_state(ResourceKind::Project, "project-1");
    let encoded = serde_json::to_value(&project).expect("project state must serialize");
    assert_eq!(encoded["containment"], serde_json::Value::Null);
    assert_eq!(
        serde_json::from_value::<ResourceState>(encoded)
            .expect("explicit null project containment must deserialize"),
        project
    );
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
        Some(first.clone()),
        vec![second.clone(), first.clone(), second.clone()],
    );

    assert!(resource.is_protected());
    assert_eq!(resource.dependencies(), &[first, second]);
}

#[test]
fn resource_state_requires_kind_correct_containment() {
    let environment: ResourceAddress = "environment.production"
        .parse()
        .expect("address must parse");
    let project: ResourceAddress = "project.main".parse().expect("address must parse");

    let application = ResourceState::try_new(
        ResourceKind::Application,
        RemoteId::new("application-1").expect("remote ID must be valid"),
        false,
        ManagedInputs::try_from_json(json!({})).expect("inputs must be safe"),
        SensitiveInputs::default(),
        Some(environment.clone()),
        Vec::new(),
    )
    .expect("an application belongs to an environment");

    assert_eq!(application.containment(), Some(&environment));
    assert!(
        ResourceState::try_new(
            ResourceKind::Application,
            RemoteId::new("application-2").expect("remote ID must be valid"),
            false,
            ManagedInputs::try_from_json(json!({})).expect("inputs must be safe"),
            SensitiveInputs::default(),
            Some(project),
            Vec::new(),
        )
        .is_err()
    );
}

#[test]
fn state_debug_output_does_not_expose_managed_values_or_remote_ids() {
    let inputs = ManagedInputs::try_from_json(json!({
        "description": "managed-input-canary"
    }))
    .expect("inputs must be safe");
    let resource = ResourceState::new(
        ResourceKind::Application,
        RemoteId::new("remote-id-canary").expect("remote ID must be valid"),
        false,
        inputs.clone(),
        Some(
            "environment.production"
                .parse()
                .expect("address must parse"),
        ),
        Vec::new(),
    );
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    state
        .upsert_resource(
            "application.api".parse().expect("address must parse"),
            resource.clone(),
        )
        .expect("resource must be inserted");

    for debug in [
        format!("{inputs:?}"),
        format!("{resource:?}"),
        format!("{state:?}"),
    ] {
        assert!(!debug.contains("managed-input-canary"));
        assert!(!debug.contains("remote-id-canary"));
    }
}

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://deploy.example.com").expect("instance must be valid")
}

fn resource_state(kind: ResourceKind, remote_id: &str) -> ResourceState {
    let containment = match kind.containment_parent_kind() {
        None => None,
        Some(ResourceKind::Project) => {
            Some("project.main".parse().expect("containment must parse"))
        }
        Some(ResourceKind::Environment) => Some(
            "environment.production"
                .parse()
                .expect("containment must parse"),
        ),
        Some(_) => unreachable!("the current model has only two containment parent kinds"),
    };
    ResourceState::new(
        kind,
        RemoteId::new(remote_id).expect("remote ID must be valid"),
        false,
        ManagedInputs::try_from_json(json!({ "description": "managed" }))
            .expect("inputs must be safe"),
        containment,
        Vec::new(),
    )
}

fn fingerprint(byte: u8) -> SensitiveFingerprint {
    SensitiveFingerprint::new_v1(
        FingerprintKeyId::new(
            Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").expect("UUID must parse"),
        )
        .expect("key ID must be valid"),
        [byte; 32],
    )
}
