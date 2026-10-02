//! Server placement for Compose and database services is a create-only selector.

use std::collections::BTreeMap;

use dokploy_core::{
    ChangeKind, ComparableValue, ConfigDigest, DesiredResource, DesiredState, DesiredStateError,
    ExternalResolution, ExternalSelectorFailure, MutationContract, MutationMode, OwnedValue,
    PlanDiagnosticCode, PropertyMutation, PropertyObservation, PropertyPath, RemoteObservation,
    RemoteResource, RemoteState, ReplacementOrder, StoredState, plan,
};
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateFile,
};
use semver::Version;
use serde_json::json;

const CANARY_ID: &str = "external-id-canary-6a10";
const CANARY_NAME: &str = "edge-canary-name-2c";

const PLACED: [(ResourceKind, &str); 7] = [
    (ResourceKind::Compose, "compose.main"),
    (ResourceKind::Postgres, "postgres.main"),
    (ResourceKind::MySql, "mysql.main"),
    (ResourceKind::MariaDb, "mariadb.main"),
    (ResourceKind::Mongo, "mongo.main"),
    (ResourceKind::LibSql, "libsql.main"),
    (ResourceKind::Redis, "redis.main"),
];

#[test]
fn server_is_a_valid_selector_path_for_every_placed_kind_and_no_other_service_kind() {
    for (_, name) in PLACED {
        let desired = |value: OwnedValue| {
            DesiredState::try_new(
                digest(),
                BTreeMap::from([(
                    address(name),
                    DesiredResource::new(BTreeMap::from([(PropertyPath::Server, value)]))
                        .with_containment(Some(address("environment.production"))),
                )]),
            )
        };
        desired(named("edge-1")).expect("named server");
        desired(local()).expect("local server");
        for invalid in [
            OwnedValue::Null,
            named(""),
            named(" padded"),
            named("line\nbreak"),
            named(&"x".repeat(257)),
            OwnedValue::Value(value(json!("raw-external-id"))),
            OwnedValue::Value(value(json!({"id": "raw"}))),
            OwnedValue::Value(value(json!({"name": "a", "local": true}))),
        ] {
            assert!(desired(invalid).is_err(), "{name}");
        }

        // No other association applies: Compose and databases have no registry, build
        // server, or destination, so these remain invalid and cannot be smuggled in.
        for path in [
            PropertyPath::BuildServer,
            PropertyPath::Registry,
            PropertyPath::BuildRegistry,
            PropertyPath::RollbackRegistry,
            PropertyPath::Destination,
        ] {
            let error = DesiredState::try_new(
                digest(),
                BTreeMap::from([(
                    address(name),
                    DesiredResource::new(BTreeMap::from([(path, named("main"))]))
                        .with_containment(Some(address("environment.production"))),
                )]),
            )
            .expect_err("only server placement applies");
            assert!(matches!(
                error,
                DesiredStateError::InvalidPropertyPath { .. }
            ));
        }
    }
}

#[test]
fn resolution_attachment_accepts_server_placement_only() {
    for (kind, name) in PLACED {
        let resource = address(name);
        let base = || {
            RemoteState::try_new(instance(), [(resource.clone(), RemoteObservation::Missing)])
                .expect("remote state")
        };
        for resolution in [
            ExternalResolution::Local,
            ExternalResolution::Resolved(remote_id("server-1")),
            ExternalResolution::Unmatched,
        ] {
            base()
                .with_external_resolutions([((resource.clone(), PropertyPath::Server), resolution)])
                .unwrap_or_else(|error| panic!("{kind:?}: {error:?}"));
        }
        for invalid in [PropertyPath::Registry, PropertyPath::BuildServer] {
            assert!(
                base()
                    .with_external_resolutions([(
                        (resource.clone(), invalid),
                        ExternalResolution::Unmatched,
                    )])
                    .is_err()
            );
        }
    }
}

#[test]
fn a_changed_server_replaces_delete_before_create_and_an_unchanged_one_is_a_no_op() {
    for (kind, name) in PLACED {
        let resource = address(name);
        let instance = instance();
        let state = service_state(kind, &resource, &instance, "edge-1", false);
        let stored = StoredState::try_from_state(&state).expect("state projects");
        let build = |selected: OwnedValue, observed_server: serde_json::Value| {
            let desired = DesiredState::try_new(
                digest(),
                BTreeMap::from([(
                    resource.clone(),
                    DesiredResource::new(BTreeMap::from([(PropertyPath::Server, selected)]))
                        .with_containment(Some(address("environment.production"))),
                )]),
            )
            .expect("desired state");
            let remote = RemoteState::try_new_with_contracts(
                instance.clone(),
                [(
                    resource.clone(),
                    RemoteObservation::Present(RemoteResource::new(
                        remote_id("remote-1"),
                        BTreeMap::from([(
                            PropertyPath::Server,
                            PropertyObservation::Known(value(observed_server)),
                        )]),
                    )),
                )],
                [(resource.clone(), placed_contract())],
            )
            .expect("remote state")
            .with_external_resolutions([(
                (resource.clone(), PropertyPath::Server),
                ExternalResolution::Resolved(remote_id("server-new")),
            )])
            .expect("resolution");
            plan(&desired, &stored, &remote)
        };

        let unchanged = build(named("edge-1"), json!({"name": "edge-1"}));
        assert!(unchanged.applyable(), "{kind:?}");
        assert!(unchanged.changes().is_empty(), "{kind:?}");

        let replaced = build(named("edge-2"), json!({"name": "edge-1"}));
        assert!(
            replaced.applyable(),
            "{kind:?}: {:?}",
            replaced.diagnostics()
        );
        assert_eq!(replaced.changes().len(), 1, "{kind:?}");
        assert_eq!(
            replaced.changes()[0].kind(),
            ChangeKind::Replace,
            "{kind:?}"
        );
        assert_eq!(
            replaced.changes()[0].replacement_order(),
            Some(ReplacementOrder::DeleteBeforeCreate)
        );

        let to_local = build(local(), json!({"name": "edge-1"}));
        assert_eq!(
            to_local.changes()[0].kind(),
            ChangeKind::Replace,
            "{kind:?}"
        );

        // Out-of-band placement drift is detected through the selector vocabulary.
        let drifted = build(named("edge-1"), json!({"name": "edge-2"}));
        assert_eq!(drifted.drift().len(), 1, "{kind:?}");
        assert_eq!(drifted.changes()[0].kind(), ChangeKind::Replace);
    }
}

#[test]
fn a_protected_service_is_never_replaced_by_a_placement_change() {
    for (kind, name) in PLACED {
        let resource = address(name);
        let instance = instance();
        let state = service_state(kind, &resource, &instance, "edge-1", true);
        let stored = StoredState::try_from_state(&state).expect("state projects");
        let desired = DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                resource.clone(),
                DesiredResource::new(BTreeMap::from([(PropertyPath::Server, named("edge-2"))]))
                    .with_containment(Some(address("environment.production"))),
            )]),
        )
        .expect("desired state");
        let remote = RemoteState::try_new_with_contracts(
            instance.clone(),
            [(
                resource.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    remote_id("remote-1"),
                    BTreeMap::from([(
                        PropertyPath::Server,
                        PropertyObservation::Known(value(json!({"name": "edge-1"}))),
                    )]),
                )),
            )],
            [(resource.clone(), placed_contract())],
        )
        .expect("remote state")
        .with_external_resolutions([(
            (resource.clone(), PropertyPath::Server),
            ExternalResolution::Resolved(remote_id("server-new")),
        )])
        .expect("resolution");

        let plan = plan(&desired, &stored, &remote);

        assert!(!plan.applyable(), "{kind:?}");
        assert!(plan.changes().is_empty());
        assert_eq!(
            plan.diagnostics()[0].code(),
            PlanDiagnosticCode::ProtectedDelete
        );
    }
}

#[test]
fn an_unresolved_server_selector_blocks_the_create_with_a_value_free_diagnostic() {
    for (_, name) in PLACED {
        let resource = address(name);
        for (resolution, expected) in [
            (
                Some(ExternalResolution::Unmatched),
                ExternalSelectorFailure::Unmatched,
            ),
            (
                Some(ExternalResolution::Ambiguous),
                ExternalSelectorFailure::Ambiguous,
            ),
            (None, ExternalSelectorFailure::Unobserved),
        ] {
            let desired = DesiredState::try_new(
                digest(),
                BTreeMap::from([(
                    resource.clone(),
                    DesiredResource::new(BTreeMap::from([(
                        PropertyPath::Server,
                        named(CANARY_NAME),
                    )]))
                    .with_containment(Some(address("environment.production"))),
                )]),
            )
            .expect("desired state");
            let mut remote =
                RemoteState::try_new(instance(), [(resource.clone(), RemoteObservation::Missing)])
                    .expect("remote state");
            if let Some(resolution) = resolution {
                remote = remote
                    .with_external_resolutions([(
                        (resource.clone(), PropertyPath::Server),
                        resolution,
                    )])
                    .expect("resolution");
            }

            let plan = plan(&desired, &StoredState::absent(instance()), &remote);

            assert!(!plan.applyable(), "{name}");
            let diagnostic = &plan.diagnostics()[0];
            assert_eq!(diagnostic.code().as_str(), "DOKPLAN019");
            assert_eq!(diagnostic.selector_failure(), Some(expected));
            assert_eq!(diagnostic.property(), Some(&PropertyPath::Server));
            let json = String::from_utf8(plan.to_json_bytes()).unwrap();
            assert!(!json.contains(CANARY_NAME));
            assert!(!json.contains(CANARY_ID));
        }
    }
}

#[test]
fn binding_receipts_bind_the_resolved_server_of_every_placed_kind() {
    let key = [7_u8; 32];
    for (_, name) in PLACED {
        let resource = address(name);
        let with = |resolution: ExternalResolution| {
            RemoteState::try_new(instance(), [(resource.clone(), RemoteObservation::Missing)])
                .expect("remote state")
                .with_external_resolutions([((resource.clone(), PropertyPath::Server), resolution)])
                .expect("resolution")
                .binding_receipt(&key)
        };
        let resolved = with(ExternalResolution::Resolved(remote_id("server-1")));
        assert_eq!(
            resolved,
            with(ExternalResolution::Resolved(remote_id("server-1")))
        );
        for different in [
            with(ExternalResolution::Resolved(remote_id("server-2"))),
            with(ExternalResolution::Local),
            with(ExternalResolution::Unmatched),
            with(ExternalResolution::Ambiguous),
        ] {
            assert_ne!(resolved, different, "{name}");
        }
    }
}

#[test]
fn stored_and_remote_server_values_fail_closed_on_malformed_shapes() {
    for (kind, name) in PLACED {
        let resource = address(name);
        for malformed in [
            json!("raw-external-id"),
            json!({"id": "raw"}),
            json!({"name": "a", "local": true}),
            json!({"name": ""}),
        ] {
            let mut state = StateFile::new(Version::new(0, 1, 0), instance());
            state
                .upsert_resource(
                    resource.clone(),
                    ResourceState::new(
                        kind,
                        remote_id("remote-1"),
                        false,
                        ManagedInputs::try_from_json(json!({ "server": malformed })).unwrap(),
                        Some(address("environment.production")),
                        Vec::new(),
                    ),
                )
                .unwrap();
            assert!(
                StoredState::try_from_state(&state).is_err(),
                "{kind:?} {malformed}"
            );

            let observation = RemoteState::try_new(
                instance(),
                [(
                    resource.clone(),
                    RemoteObservation::Present(RemoteResource::new(
                        remote_id("remote-1"),
                        BTreeMap::from([(
                            PropertyPath::Server,
                            PropertyObservation::Known(value(malformed.clone())),
                        )]),
                    )),
                )],
            );
            assert!(observation.is_err(), "{kind:?} {malformed}");
        }
    }
}

fn placed_contract() -> MutationContract {
    MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
        .allowing_on_create(PropertyPath::Server)
        .with_property(
            PropertyPath::Server,
            PropertyMutation::new(MutationMode::Replace, MutationMode::Unsupported),
        )
        .with_containment(MutationMode::StateOnly)
}

fn service_state(
    kind: ResourceKind,
    address: &ResourceAddress,
    instance: &InstanceIdentity,
    server: &str,
    protected: bool,
) -> StateFile {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    state
        .upsert_resource(
            address.clone(),
            ResourceState::new(
                kind,
                remote_id("remote-1"),
                protected,
                ManagedInputs::try_from_json(json!({ "server": { "name": server } }))
                    .expect("managed inputs"),
                Some(self::address("environment.production")),
                Vec::new(),
            ),
        )
        .expect("state insert");
    state
}

fn named(name: &str) -> OwnedValue {
    OwnedValue::Value(value(json!({ "name": name })))
}

fn local() -> OwnedValue {
    OwnedValue::Value(value(json!({ "local": true })))
}

fn value(value: serde_json::Value) -> ComparableValue {
    ComparableValue::try_from_json(value).expect("value is comparable")
}

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
}

fn digest() -> ConfigDigest {
    ConfigDigest::parse("0".repeat(64)).expect("digest is valid")
}

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://deploy.example.test").expect("instance is valid")
}

fn remote_id(value: &str) -> RemoteId {
    RemoteId::new(value).expect("remote ID is valid")
}
