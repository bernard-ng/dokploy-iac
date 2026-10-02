use std::collections::BTreeMap;

use dokploy_core::{
    ChangeKind, ComparableValue, ConfigDigest, DesiredResource, DesiredState, DesiredStateError,
    ExternalResolution, ExternalSelectorFailure, MutationContract, MutationMode, OwnedValue,
    PlanDiagnosticCode, PropertyMutation, PropertyObservation, PropertyPath, PropertyUnknownReason,
    RemoteFailureKind, RemoteObservation, RemoteResource, RemoteState, RemoteStateError,
    ReplacementOrder, StoredState, StoredStateError, plan,
};
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateFile,
};
use semver::Version;
use serde_json::json;

const CANARY_ID: &str = "external-id-canary-8f21";
const CANARY_NAME: &str = "edge-canary-name";

const SELECTOR_PATHS: [(PropertyPath, &str); 5] = [
    (PropertyPath::Server, "server"),
    (PropertyPath::BuildServer, "build_server"),
    (PropertyPath::Registry, "registry"),
    (PropertyPath::BuildRegistry, "build_registry"),
    (PropertyPath::RollbackRegistry, "rollback_registry"),
];

#[test]
fn selector_paths_use_config_compatible_strings_and_only_apply_to_applications_and_server_placement()
 {
    for (path, expected) in SELECTOR_PATHS {
        assert_eq!(path.to_string(), expected);
        assert_eq!(expected.parse::<PropertyPath>().expect("parses"), path);
        assert!(path.is_external_selector());
        assert!(!path.is_sensitive());
        assert!(!path.is_lifecycle_only());
    }
    assert!(!PropertyPath::Application.is_external_selector());
    assert!(!PropertyPath::Host.is_external_selector());

    let application = address("application.api");
    let compose = address("compose.stack");
    for (path, _) in SELECTOR_PATHS {
        DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                application.clone(),
                DesiredResource::new(BTreeMap::from([(path.clone(), selector_for(&path))]))
                    .with_containment(Some(address("environment.production"))),
            )]),
        )
        .expect("application selectors are valid");
        let result = DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                compose.clone(),
                DesiredResource::new(BTreeMap::from([(path.clone(), selector_for(&path))]))
                    .with_containment(Some(address("environment.production"))),
            )]),
        );
        if path == PropertyPath::Server {
            result.expect("server placement is valid for Compose");
        } else {
            assert!(matches!(
                result.expect_err("only server placement applies to Compose"),
                DesiredStateError::InvalidPropertyPath { .. }
            ));
        }
    }
}

#[test]
fn desired_selector_values_are_closed_stable_selectors() {
    let application = address("application.api");
    let valid = |path: PropertyPath, value: OwnedValue| {
        DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                application.clone(),
                DesiredResource::new(BTreeMap::from([(path, value)]))
                    .with_containment(Some(address("environment.production"))),
            )]),
        )
    };

    valid(PropertyPath::Server, named("edge-1")).expect("named server");
    valid(PropertyPath::Server, local()).expect("local server");
    valid(PropertyPath::BuildServer, named("builder")).expect("named build server");
    valid(PropertyPath::BuildServer, OwnedValue::Null).expect("cleared build server");
    valid(PropertyPath::Registry, OwnedValue::Null).expect("cleared registry");
    valid(PropertyPath::RollbackRegistry, named("main")).expect("named registry");

    for (path, value) in [
        (PropertyPath::Server, OwnedValue::Null),
        (PropertyPath::Registry, local()),
        (PropertyPath::BuildServer, local()),
        (PropertyPath::Server, named("")),
        (PropertyPath::Server, named(" padded")),
        (PropertyPath::Server, named("line\nbreak")),
        (PropertyPath::Server, named(&"x".repeat(257))),
        (
            PropertyPath::Server,
            OwnedValue::Value(value(json!("raw-external-id"))),
        ),
        (
            PropertyPath::Server,
            OwnedValue::Value(value(json!({"name": "a", "local": true}))),
        ),
        (
            PropertyPath::Server,
            OwnedValue::Value(value(json!({"id": "raw"}))),
        ),
        (
            PropertyPath::Server,
            OwnedValue::Value(value(json!({"local": false}))),
        ),
        (PropertyPath::Registry, OwnedValue::EmptyCollection),
    ] {
        let error = valid(path, value).expect_err("invalid selector must fail closed");
        assert!(matches!(
            error,
            DesiredStateError::InvalidPropertyValue { .. }
        ));
    }
}

#[test]
fn stored_and_remote_selector_values_fail_closed_on_malformed_shapes() {
    let application = address("application.api");
    let instance = instance();

    for (previous, valid) in [
        (json!({"server": {"local": true}}), true),
        (json!({"server": {"name": "edge"}}), true),
        (json!({"registry": null}), true),
        (json!({"registry": {"name": "main"}}), true),
        (json!({"server": null}), false),
        (json!({"registry": {"local": true}}), false),
        (json!({"registry": "raw-id"}), false),
        (json!({"registry": {"name": ""}}), false),
    ] {
        let state = application_state(&application, &instance, previous);
        assert_eq!(StoredState::try_from_state(&state).is_ok(), valid);
    }
    let state = application_state(&application, &instance, json!({"registry": 7}));
    assert!(matches!(
        StoredState::try_from_state(&state),
        Err(StoredStateError::InvalidPropertyValue { .. })
    ));

    for (path, observation, valid) in [
        (
            PropertyPath::Server,
            PropertyObservation::Known(value(json!({"local": true}))),
            true,
        ),
        (
            PropertyPath::Server,
            PropertyObservation::Known(value(json!({"name": "edge"}))),
            true,
        ),
        (
            PropertyPath::Server,
            PropertyObservation::KnownAbsent,
            false,
        ),
        (
            PropertyPath::Registry,
            PropertyObservation::KnownAbsent,
            true,
        ),
        (
            PropertyPath::Registry,
            PropertyObservation::Known(value(json!({"local": true}))),
            false,
        ),
        (
            PropertyPath::Registry,
            PropertyObservation::Known(value(json!("raw-id"))),
            false,
        ),
        (
            PropertyPath::Registry,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
            false,
        ),
        (
            PropertyPath::Registry,
            PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
            true,
        ),
    ] {
        let result = RemoteState::try_new(
            instance.clone(),
            [(
                application.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    remote_id("app-1"),
                    BTreeMap::from([(path, observation)]),
                )),
            )],
        );
        assert_eq!(result.is_ok(), valid);
    }
}

#[test]
fn resolution_attachment_rejects_unobserved_misplaced_or_duplicate_selectors() {
    let application = address("application.api");
    let compose = address("compose.stack");
    let base = || {
        RemoteState::try_new(
            instance(),
            [
                (application.clone(), RemoteObservation::Missing),
                (compose.clone(), RemoteObservation::Missing),
            ],
        )
        .expect("remote state")
    };

    base()
        .with_external_resolutions([
            (
                (application.clone(), PropertyPath::Server),
                ExternalResolution::Local,
            ),
            (
                (application.clone(), PropertyPath::Registry),
                ExternalResolution::Resolved(remote_id(CANARY_ID)),
            ),
        ])
        .expect("valid resolutions");

    for invalid in [
        (
            (application.clone(), PropertyPath::Description),
            ExternalResolution::Unmatched,
        ),
        (
            (compose.clone(), PropertyPath::Registry),
            ExternalResolution::Unmatched,
        ),
        (
            (address("application.other"), PropertyPath::Server),
            ExternalResolution::Unmatched,
        ),
        (
            (application.clone(), PropertyPath::Registry),
            ExternalResolution::Local,
        ),
    ] {
        let error = base()
            .with_external_resolutions([invalid])
            .expect_err("invalid resolution key must fail closed");
        assert!(matches!(
            error,
            RemoteStateError::InvalidExternalResolution { .. }
        ));
    }
    let duplicate = [
        (
            (application.clone(), PropertyPath::Server),
            ExternalResolution::Unmatched,
        ),
        (
            (application.clone(), PropertyPath::Server),
            ExternalResolution::Ambiguous,
        ),
    ];
    assert!(matches!(
        base().with_external_resolutions(duplicate),
        Err(RemoteStateError::InvalidExternalResolution { .. })
    ));
}

#[test]
fn unresolved_selectors_block_the_plan_with_value_free_typed_diagnostics() {
    let application = address("application.api");
    for (resolution, expected) in [
        (
            Some(ExternalResolution::Unmatched),
            ExternalSelectorFailure::Unmatched,
        ),
        (
            Some(ExternalResolution::Ambiguous),
            ExternalSelectorFailure::Ambiguous,
        ),
        (
            Some(ExternalResolution::Unavailable(
                RemoteFailureKind::Unavailable,
            )),
            ExternalSelectorFailure::Unavailable,
        ),
        (None, ExternalSelectorFailure::Unobserved),
    ] {
        let desired = desired_application([(PropertyPath::Registry, named(CANARY_NAME))]);
        let stored = StoredState::absent(instance());
        let mut remote = RemoteState::try_new(
            instance(),
            [(application.clone(), RemoteObservation::Missing)],
        )
        .expect("remote state");
        if let Some(resolution) = resolution {
            remote = remote
                .with_external_resolutions([(
                    (application.clone(), PropertyPath::Registry),
                    resolution,
                )])
                .expect("resolution is valid");
        }

        let plan = plan(&desired, &stored, &remote);

        assert!(!plan.applyable());
        assert!(plan.changes().is_empty());
        assert_eq!(plan.diagnostics().len(), 1);
        let diagnostic = &plan.diagnostics()[0];
        assert_eq!(
            diagnostic.code(),
            PlanDiagnosticCode::UnresolvedExternalSelector
        );
        assert_eq!(diagnostic.code().as_str(), "DOKPLAN019");
        assert_eq!(diagnostic.address(), Some(&application));
        assert_eq!(diagnostic.property(), Some(&PropertyPath::Registry));
        assert_eq!(diagnostic.selector_failure(), Some(expected));
        let json = String::from_utf8(plan.to_json_bytes()).expect("plan JSON is UTF-8");
        assert!(json.contains("\"selectorFailure\""));
        assert!(!json.contains(CANARY_NAME));
        assert!(!json.contains(CANARY_ID));
    }
}

#[test]
fn cleared_local_and_ignored_selectors_need_no_resolution_and_unrelated_work_continues() {
    let application = address("application.api");
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            application.clone(),
            DesiredResource::new(BTreeMap::from([
                (PropertyPath::BuildRegistry, OwnedValue::Null),
                (PropertyPath::Registry, named("ignored")),
                (PropertyPath::Server, local()),
            ]))
            .with_containment(Some(address("environment.production")))
            .with_ignored_changes(vec![PropertyPath::Registry]),
        )]),
    )
    .expect("desired state");
    let remote = RemoteState::try_new(
        instance(),
        [(application.clone(), RemoteObservation::Missing)],
    )
    .expect("remote state")
    .with_external_resolutions([(
        (application.clone(), PropertyPath::Server),
        ExternalResolution::Local,
    )])
    .expect("local resolution");

    let plan = plan(&desired, &StoredState::absent(instance()), &remote);

    assert!(plan.applyable(), "{:?}", plan.diagnostics());
    assert_eq!(plan.changes().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
}

#[test]
fn registry_change_updates_in_place_and_server_change_replaces_delete_before_create() {
    let application = address("application.api");
    let instance = instance();
    let state = application_state(
        &application,
        &instance,
        json!({
            "registry": {"name": "old-registry"},
            "server": {"name": "edge-1"}
        }),
    );
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let contract = application_contract();

    let build = |desired: DesiredResource,
                 observed: BTreeMap<PropertyPath, PropertyObservation>| {
        let desired = DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                application.clone(),
                desired.with_containment(Some(address("environment.production"))),
            )]),
        )
        .expect("desired state");
        let remote = RemoteState::try_new_with_contracts(
            instance.clone(),
            [(
                application.clone(),
                RemoteObservation::Present(RemoteResource::new(remote_id("remote-1"), observed)),
            )],
            [(application.clone(), contract.clone())],
        )
        .expect("remote state")
        .with_external_resolutions([
            (
                (application.clone(), PropertyPath::Registry),
                ExternalResolution::Resolved(remote_id("registry-new")),
            ),
            (
                (application.clone(), PropertyPath::Server),
                ExternalResolution::Resolved(remote_id("server-new")),
            ),
        ])
        .expect("resolutions");
        plan(&desired, &stored, &remote)
    };
    let observed = BTreeMap::from([
        (
            PropertyPath::Registry,
            PropertyObservation::Known(value(json!({"name": "old-registry"}))),
        ),
        (
            PropertyPath::Server,
            PropertyObservation::Known(value(json!({"name": "edge-1"}))),
        ),
    ]);

    let in_place = build(
        DesiredResource::new(BTreeMap::from([
            (PropertyPath::Registry, named("new-registry")),
            (PropertyPath::Server, named("edge-1")),
        ])),
        observed.clone(),
    );
    assert!(in_place.applyable(), "{:?}", in_place.diagnostics());
    assert_eq!(in_place.changes().len(), 1);
    assert_eq!(in_place.changes()[0].kind(), ChangeKind::Update);

    let cleared = build(
        DesiredResource::new(BTreeMap::from([
            (PropertyPath::Registry, OwnedValue::Null),
            (PropertyPath::Server, named("edge-1")),
        ])),
        observed.clone(),
    );
    assert_eq!(cleared.changes()[0].kind(), ChangeKind::Update);

    let replaced = build(
        DesiredResource::new(BTreeMap::from([
            (PropertyPath::Registry, named("old-registry")),
            (PropertyPath::Server, named("edge-2")),
        ])),
        observed.clone(),
    );
    assert!(replaced.applyable(), "{:?}", replaced.diagnostics());
    assert_eq!(replaced.changes()[0].kind(), ChangeKind::Replace);
    assert_eq!(
        replaced.changes()[0].replacement_order(),
        Some(ReplacementOrder::DeleteBeforeCreate)
    );

    let to_local = build(
        DesiredResource::new(BTreeMap::from([
            (PropertyPath::Registry, named("old-registry")),
            (PropertyPath::Server, local()),
        ])),
        observed,
    );
    assert_eq!(to_local.changes()[0].kind(), ChangeKind::Replace);
}

#[test]
fn drifted_association_is_detected_through_the_selector_vocabulary() {
    let application = address("application.api");
    let instance = instance();
    let state = application_state(
        &application,
        &instance,
        json!({"registry": {"name": "main"}}),
    );
    let stored = StoredState::try_from_state(&state).expect("state projects");
    let desired = desired_application([(PropertyPath::Registry, named("main"))]);
    let remote = RemoteState::try_new_with_contracts(
        instance,
        [(
            application.clone(),
            RemoteObservation::Present(RemoteResource::new(
                remote_id("remote-1"),
                BTreeMap::from([(PropertyPath::Registry, PropertyObservation::KnownAbsent)]),
            )),
        )],
        [(application.clone(), application_contract())],
    )
    .expect("remote state")
    .with_external_resolutions([(
        (application.clone(), PropertyPath::Registry),
        ExternalResolution::Resolved(remote_id("registry-main")),
    )])
    .expect("resolution");

    let plan = plan(&desired, &stored, &remote);

    assert_eq!(plan.drift().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
}

#[test]
fn binding_receipt_binds_resolved_external_identities_without_changing_legacy_receipts() {
    let application = address("application.api");
    let key = [7_u8; 32];
    let base = || {
        RemoteState::try_new(
            instance(),
            [(application.clone(), RemoteObservation::Missing)],
        )
        .expect("remote state")
    };
    let with = |resolution: ExternalResolution| {
        base()
            .with_external_resolutions([(
                (application.clone(), PropertyPath::Registry),
                resolution,
            )])
            .expect("resolution")
    };

    let legacy = base().binding_receipt(&key);
    assert_eq!(
        legacy,
        base()
            .with_external_resolutions(Vec::new())
            .expect("empty resolutions")
            .binding_receipt(&key)
    );

    let resolved =
        with(ExternalResolution::Resolved(remote_id("registry-1"))).binding_receipt(&key);
    assert_eq!(
        resolved,
        with(ExternalResolution::Resolved(remote_id("registry-1"))).binding_receipt(&key)
    );
    for different in [
        with(ExternalResolution::Resolved(remote_id("registry-2"))),
        with(ExternalResolution::Unmatched),
        with(ExternalResolution::Ambiguous),
        with(ExternalResolution::Unavailable(
            RemoteFailureKind::Unavailable,
        )),
        with(ExternalResolution::Unavailable(
            RemoteFailureKind::Unauthorized,
        )),
        base(),
    ] {
        assert_ne!(resolved, different.binding_receipt(&key));
    }
    assert_ne!(
        resolved,
        with(ExternalResolution::Resolved(remote_id("registry-1"))).binding_receipt(&[8; 32])
    );

    let other_path = base()
        .with_external_resolutions([(
            (application.clone(), PropertyPath::BuildRegistry),
            ExternalResolution::Resolved(remote_id("registry-1")),
        )])
        .expect("resolution")
        .binding_receipt(&key);
    assert_ne!(resolved, other_path, "the selector property is bound");
}

#[test]
fn external_resolutions_are_redacted_in_debug_output() {
    let application = address("application.api");
    let remote = RemoteState::try_new(
        instance(),
        [(application.clone(), RemoteObservation::Missing)],
    )
    .expect("remote state")
    .with_external_resolutions([(
        (application, PropertyPath::Registry),
        ExternalResolution::Resolved(remote_id(CANARY_ID)),
    )])
    .expect("resolution");

    let debug = format!(
        "{remote:?} {:?}",
        ExternalResolution::Resolved(remote_id(CANARY_ID))
    );

    assert!(!debug.contains(CANARY_ID));
    assert!(debug.contains("external_resolution_count: 1"));
    assert!(ExternalResolution::Local.is_resolved());
    assert!(!ExternalResolution::Ambiguous.is_resolved());
    assert_eq!(
        ExternalResolution::Resolved(remote_id("a")).remote_id(),
        Some(&remote_id("a"))
    );
    assert_eq!(ExternalResolution::Local.remote_id(), None);
}

fn application_contract() -> MutationContract {
    MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
        .with_default_property(PropertyMutation::new(
            MutationMode::InPlace,
            MutationMode::InPlace,
        ))
        .with_property(
            PropertyPath::Server,
            PropertyMutation::new(MutationMode::Replace, MutationMode::Unsupported),
        )
        .with_containment(MutationMode::InPlace)
}

fn desired_application<const N: usize>(
    properties: [(PropertyPath, OwnedValue); N],
) -> DesiredState {
    DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            address("application.api"),
            DesiredResource::new(properties.into_iter().collect())
                .with_containment(Some(address("environment.production"))),
        )]),
    )
    .expect("desired state")
}

fn selector_for(path: &PropertyPath) -> OwnedValue {
    if *path == PropertyPath::Server {
        local()
    } else {
        named("main")
    }
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

fn application_state(
    address: &ResourceAddress,
    instance: &InstanceIdentity,
    previous: serde_json::Value,
) -> StateFile {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    state
        .upsert_resource(
            address.clone(),
            ResourceState::new(
                ResourceKind::Application,
                remote_id("remote-1"),
                false,
                ManagedInputs::try_from_json(previous).expect("managed inputs"),
                Some(self::address("environment.production")),
                Vec::new(),
            ),
        )
        .expect("state insert");
    state
}
