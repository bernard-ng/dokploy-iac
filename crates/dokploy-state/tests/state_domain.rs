use std::collections::BTreeMap;
use std::str::FromStr;

use dokploy_state::{
    FingerprintKeyId, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath, StateError,
    StateFile,
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

    let mysql =
        ResourceAddress::from_str("mysql.primary").expect("a MySQL logical address must parse");
    assert_eq!(mysql.kind(), ResourceKind::MySql);
    assert_eq!(mysql.to_string(), "mysql.primary");

    for (value, kind) in [
        ("compose.web", ResourceKind::Compose),
        ("mariadb.primary", ResourceKind::MariaDb),
        ("mongo.documents", ResourceKind::Mongo),
        ("libsql.edge", ResourceKind::LibSql),
    ] {
        let address = ResourceAddress::from_str(value).expect("database address must parse");
        assert_eq!(address.kind(), kind);
        assert_eq!(address.to_string(), value);
        assert_eq!(
            kind.containment_parent_kind(),
            Some(ResourceKind::Environment)
        );
    }
}

#[test]
fn remaining_database_state_requires_environment_containment() {
    let environment: ResourceAddress = "environment.production"
        .parse()
        .expect("environment address must parse");

    for kind in [
        ResourceKind::Compose,
        ResourceKind::MariaDb,
        ResourceKind::Mongo,
        ResourceKind::LibSql,
    ] {
        assert!(
            ResourceState::try_new(
                kind,
                RemoteId::new(format!("{}-1", kind.as_str())).expect("remote ID must be valid"),
                false,
                ManagedInputs::try_from_json(json!({})).expect("managed inputs must be valid"),
                SensitiveInputs::default(),
                Some(environment.clone()),
                Vec::new(),
            )
            .is_ok()
        );
        assert!(
            ResourceState::try_new(
                kind,
                RemoteId::new(format!("{}-2", kind.as_str())).expect("remote ID must be valid"),
                false,
                ManagedInputs::try_from_json(json!({})).expect("managed inputs must be valid"),
                SensitiveInputs::default(),
                None,
                Vec::new(),
            )
            .is_err()
        );
    }
}

#[test]
fn port_state_requires_application_containment() {
    let application: ResourceAddress = "application.api".parse().unwrap();
    let environment: ResourceAddress = "environment.production".parse().unwrap();
    let new_state = |containment| {
        ResourceState::try_new(
            ResourceKind::Port,
            RemoteId::new("port-1").unwrap(),
            false,
            ManagedInputs::try_from_json(json!({
                "published_port": 8080,
                "target_port": 80,
                "publish_mode": "ingress",
                "protocol": "tcp"
            }))
            .unwrap(),
            SensitiveInputs::default(),
            containment,
            Vec::new(),
        )
    };

    assert!(new_state(Some(application)).is_ok());
    assert!(new_state(Some(environment)).is_err());
    assert!(new_state(None).is_err());
}

#[test]
fn redirect_and_security_state_require_application_containment() {
    let application: ResourceAddress = "application.api".parse().unwrap();
    let environment: ResourceAddress = "environment.production".parse().unwrap();

    for (kind, managed) in [
        (
            ResourceKind::Redirect,
            json!({"regex": "^/old", "replacement": "/new", "permanent": true}),
        ),
        (ResourceKind::Security, json!({"username": "admin"})),
    ] {
        let new_state = |containment| {
            ResourceState::try_new(
                kind,
                RemoteId::new("leaf-1").unwrap(),
                false,
                ManagedInputs::try_from_json(managed.clone()).unwrap(),
                SensitiveInputs::default(),
                containment,
                Vec::new(),
            )
        };

        assert!(new_state(Some(application.clone())).is_ok());
        assert!(new_state(Some(environment.clone())).is_err());
        assert!(new_state(None).is_err());
        assert_eq!(kind.as_str().parse::<ResourceKind>().unwrap(), kind);
        assert_eq!(
            kind.containment_parent_kind(),
            Some(ResourceKind::Application)
        );
    }
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
        "document",
        "composeFile",
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
        json!({ "root_password": null }),
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
        json!({ "root_password": "raw-root-password-canary" }),
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
        assert!(!debug.contains("raw-root-password-canary"));
        assert!(!debug.contains("raw-environment-canary"));
        assert!(!debug.contains("raw-env-canary"));
    }
}

#[test]
fn sensitive_inputs_validate_paths_and_resource_state_rejects_ownership_overlap() {
    let receipt = fingerprint(0x5a);
    let password = SensitivePropertyPath::parse("password").expect("password path must parse");
    let root_password =
        SensitivePropertyPath::parse("root_password").expect("root password path must parse");
    let document =
        SensitivePropertyPath::parse("document").expect("Compose document path must parse");
    let environment =
        SensitivePropertyPath::parse("environment.API_TOKEN").expect("environment path must parse");
    let sensitive = SensitiveInputs::try_from_entries([
        (environment.clone(), receipt.clone()),
        (document.clone(), receipt.clone()),
        (password.clone(), receipt.clone()),
        (root_password.clone(), receipt),
    ])
    .expect("unique sensitive paths must be accepted");

    assert!(sensitive.fingerprint(&password).is_some());
    assert!(sensitive.fingerprint(&root_password).is_some());
    assert!(sensitive.fingerprint(&environment).is_some());
    assert!(sensitive.fingerprint(&document).is_some());
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
        "root_password.extra",
    ] {
        assert!(SensitivePropertyPath::parse(invalid).is_err());
    }

    for managed in [
        json!({ "password": null }),
        json!({ "root_password": null }),
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

    ResourceState::try_new(
        ResourceKind::MySql,
        RemoteId::new("mysql-1").expect("remote ID must be valid"),
        false,
        ManagedInputs::try_from_json(json!({ "password": null }))
            .expect("password clear must be valid"),
        SensitiveInputs::try_from_entries([(
            SensitivePropertyPath::parse("root_password").expect("path must parse"),
            fingerprint(0x7c),
        )])
        .expect("root password receipt must be valid"),
        Some(
            "environment.production"
                .parse()
                .expect("address must parse"),
        ),
        Vec::new(),
    )
    .expect("user and root password ownership are disjoint");
}

#[test]
fn mysql_state_requires_environment_containment_and_round_trips() {
    let mysql = ResourceState::try_new(
        ResourceKind::MySql,
        RemoteId::new("mysql-1").expect("remote ID must be valid"),
        true,
        ManagedInputs::try_from_json(json!({ "database": "app", "username": "app" }))
            .expect("managed inputs must be valid"),
        SensitiveInputs::try_from_entries([
            (
                SensitivePropertyPath::parse("password").expect("path must parse"),
                fingerprint(0x81),
            ),
            (
                SensitivePropertyPath::parse("root_password").expect("path must parse"),
                fingerprint(0x82),
            ),
        ])
        .expect("distinct password receipts must be valid"),
        Some(
            "environment.production"
                .parse()
                .expect("address must parse"),
        ),
        Vec::new(),
    )
    .expect("MySQL state must accept environment containment");

    let encoded = serde_json::to_vec(&mysql).expect("MySQL state must serialize");
    assert!(
        String::from_utf8(encoded.clone())
            .expect("state JSON is UTF-8")
            .contains(r#""kind":"mysql""#)
    );
    let decoded: ResourceState =
        serde_json::from_slice(&encoded).expect("MySQL state must deserialize");

    assert_eq!(decoded, mysql);
    assert!(
        ResourceState::try_new(
            ResourceKind::MySql,
            RemoteId::new("mysql-2").expect("remote ID must be valid"),
            false,
            ManagedInputs::try_from_json(json!({})).expect("managed inputs must be valid"),
            SensitiveInputs::default(),
            None,
            Vec::new(),
        )
        .is_err()
    );
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
fn state_move_is_atomic_and_rewrites_all_logical_references() {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    let source: ResourceAddress = "project.legacy".parse().unwrap();
    let target: ResourceAddress = "project.main".parse().unwrap();
    let environment: ResourceAddress = "environment.production".parse().unwrap();
    let application: ResourceAddress = "application.api".parse().unwrap();
    let project = resource_state(ResourceKind::Project, "project-remote");
    let environment_state = ResourceState::new(
        ResourceKind::Environment,
        RemoteId::new("environment-remote").unwrap(),
        false,
        ManagedInputs::try_from_json(json!({})).unwrap(),
        Some(source.clone()),
        vec![source.clone(), target.clone()],
    );
    let application_state = ResourceState::new(
        ResourceKind::Application,
        RemoteId::new("application-remote").unwrap(),
        false,
        ManagedInputs::try_from_json(json!({})).unwrap(),
        Some(environment.clone()),
        vec![source.clone()],
    );
    state
        .upsert_resource(source.clone(), project.clone())
        .unwrap();
    state
        .upsert_resource(environment.clone(), environment_state)
        .unwrap();
    state
        .upsert_resource(application.clone(), application_state)
        .unwrap();
    let serial = state.serial();

    state
        .move_resource(&source, target.clone())
        .expect("one logical move must succeed atomically");

    assert_eq!(state.serial(), serial + 1);
    assert_eq!(state.resource(&source), None);
    assert_eq!(state.resource(&target), Some(&project));
    assert_eq!(
        state.resource(&environment).unwrap().containment(),
        Some(&target)
    );
    assert_eq!(
        state.resource(&environment).unwrap().dependencies(),
        std::slice::from_ref(&target)
    );
    assert_eq!(
        state.resource(&application).unwrap().dependencies(),
        std::slice::from_ref(&target)
    );
}

#[test]
fn state_move_rejects_missing_cross_kind_and_occupied_targets_without_mutation() {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    let source: ResourceAddress = "project.legacy".parse().unwrap();
    let occupied: ResourceAddress = "project.main".parse().unwrap();
    state
        .upsert_resource(
            source.clone(),
            resource_state(ResourceKind::Project, "legacy-remote"),
        )
        .unwrap();
    state
        .upsert_resource(
            occupied.clone(),
            resource_state(ResourceKind::Project, "main-remote"),
        )
        .unwrap();
    let original = state.clone();

    state
        .move_resource(&source, source.clone())
        .expect("an identical address is already satisfied");
    assert_eq!(state, original);
    assert!(matches!(
        state.move_resource(
            &"project.missing".parse().unwrap(),
            "project.new".parse().unwrap()
        ),
        Err(StateError::ResourceNotFound { .. })
    ));
    assert!(matches!(
        state.move_resource(&source, "application.api".parse().unwrap()),
        Err(StateError::MoveKindMismatch { .. })
    ));
    assert!(matches!(
        state.move_resource(&source, occupied),
        Err(StateError::ResourceAlreadyExists { .. })
    ));
    assert_eq!(state, original);
}

#[test]
fn protection_changes_advance_once_and_identical_values_are_noops() {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    let address: ResourceAddress = "postgres.main".parse().unwrap();
    state
        .upsert_resource(
            address.clone(),
            resource_state(ResourceKind::Postgres, "postgres-remote"),
        )
        .unwrap();
    let serial = state.serial();

    assert!(state.set_resource_protection(&address, true).unwrap());
    assert_eq!(state.serial(), serial + 1);
    assert!(state.resource(&address).unwrap().is_protected());
    assert!(!state.set_resource_protection(&address, true).unwrap());
    assert_eq!(state.serial(), serial + 1);
    assert!(matches!(
        state.set_resource_protection(&"postgres.missing".parse().unwrap(), true),
        Err(StateError::ResourceNotFound { .. })
    ));
}

#[test]
fn forget_is_state_only_idempotent_and_refuses_managed_dependents() {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    let project: ResourceAddress = "project.main".parse().unwrap();
    let environment: ResourceAddress = "environment.production".parse().unwrap();
    let application: ResourceAddress = "application.api".parse().unwrap();
    state
        .upsert_resource(
            project.clone(),
            resource_state(ResourceKind::Project, "project-remote"),
        )
        .unwrap();
    state
        .upsert_resource(
            environment.clone(),
            resource_state(ResourceKind::Environment, "environment-remote"),
        )
        .unwrap();
    state
        .upsert_resource(
            application.clone(),
            ResourceState::new(
                ResourceKind::Application,
                RemoteId::new("application-remote").unwrap(),
                false,
                ManagedInputs::try_from_json(json!({})).unwrap(),
                Some(environment.clone()),
                vec![project.clone()],
            ),
        )
        .unwrap();
    let original = state.clone();

    let error = state
        .forget_resource(&project)
        .expect_err("containment and dependency references must guard forget");
    assert_eq!(
        error,
        StateError::ResourceHasDependents {
            address: project.clone(),
            dependents: vec![application.clone(), environment.clone()],
        }
    );
    assert_eq!(state, original);

    state
        .set_resource_protection(&application, true)
        .expect("protection must not prevent state-only forget");
    state.forget_resource(&application).unwrap();
    let serial = state.serial();
    assert!(state.forget_resource(&application).unwrap().is_none());
    assert_eq!(state.serial(), serial);
    assert!(state.resource(&application).is_none());
}

#[test]
fn state_only_mutations_are_atomic_at_serial_overflow() {
    let address: ResourceAddress = "project.main".parse().unwrap();
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    state
        .upsert_resource(
            address.clone(),
            resource_state(ResourceKind::Project, "project-remote"),
        )
        .unwrap();
    let mut encoded = serde_json::to_value(&state).unwrap();
    encoded["serial"] = json!(u64::MAX);
    let saturated = StateFile::from_json_slice(&serde_json::to_vec(&encoded).unwrap()).unwrap();

    let mut moved = saturated.clone();
    assert_eq!(
        moved.move_resource(&address, "project.renamed".parse().unwrap()),
        Err(StateError::SerialOverflow)
    );
    assert_eq!(moved, saturated);

    let mut protected = saturated.clone();
    assert_eq!(
        protected.set_resource_protection(&address, true),
        Err(StateError::SerialOverflow)
    );
    assert_eq!(protected, saturated);

    let mut forgotten = saturated.clone();
    assert_eq!(
        forgotten.forget_resource(&address),
        Err(StateError::SerialOverflow)
    );
    assert_eq!(forgotten, saturated);
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

#[test]
fn imported_state_starts_at_serial_zero_with_a_fresh_lineage() {
    let project: ResourceAddress = "project.main".parse().unwrap();
    let environment: ResourceAddress = "environment.production".parse().unwrap();
    let resources = BTreeMap::from([
        (
            project.clone(),
            resource_state(ResourceKind::Project, "project-1"),
        ),
        (
            environment,
            resource_state(ResourceKind::Environment, "environment-1"),
        ),
    ]);

    let state = StateFile::new_with_resources(Version::new(0, 1, 0), instance(), resources)
        .expect("complete imported state is valid");

    assert_eq!(state.serial(), 0);
    assert!(!state.lineage().is_nil());
    assert_eq!(state.resources().len(), 2);
}

#[test]
fn imported_state_rejects_missing_containment_and_dependencies() {
    let environment: ResourceAddress = "environment.production".parse().unwrap();
    let missing_parent = BTreeMap::from([(
        environment.clone(),
        resource_state(ResourceKind::Environment, "environment-1"),
    )]);
    assert!(matches!(
        StateFile::new_with_resources(Version::new(0, 1, 0), instance(), missing_parent),
        Err(StateError::MissingResourceReference { .. })
    ));

    let project: ResourceAddress = "project.main".parse().unwrap();
    let project_state = ResourceState::new(
        ResourceKind::Project,
        RemoteId::new("project-1").unwrap(),
        false,
        ManagedInputs::try_from_json(json!({})).unwrap(),
        None,
        vec!["redis.missing".parse().unwrap()],
    );
    assert!(matches!(
        StateFile::new_with_resources(
            Version::new(0, 1, 0),
            instance(),
            BTreeMap::from([(project, project_state)])
        ),
        Err(StateError::MissingResourceReference { .. })
    ));
}

#[test]
fn imported_state_rejects_duplicate_physical_identities() {
    let resources = BTreeMap::from([
        (
            "project.first".parse().unwrap(),
            resource_state(ResourceKind::Project, "shared-id"),
        ),
        (
            "project.second".parse().unwrap(),
            resource_state(ResourceKind::Project, "shared-id"),
        ),
    ]);

    assert!(matches!(
        StateFile::new_with_resources(Version::new(0, 1, 0), instance(), resources),
        Err(StateError::DuplicateRemoteIdentity { .. })
    ));
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
