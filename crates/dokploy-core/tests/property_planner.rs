use std::collections::BTreeMap;

use dokploy_core::{
    ChangeKind, ChangeOrigin, CheckpointValueRef, ComparableValue, ConfigDigest, DesiredResource,
    DesiredState, DesiredStateError, MetadataChangeKind, MoveDirective, OwnedValue, Plan,
    PlanDiagnosticCode, PropertyObservation, PropertyPath, PropertyUnknownReason, ProtectionIntent,
    RemoteFailureKind, RemoteObservation, RemoteResource, RemoteState, RemoteStateError,
    RemovalDirective, StoredState, StoredStateError, UnsupportedDirectiveKind, ValueState, plan,
};
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateFile,
};
use semver::Version;
use serde_json::json;

#[test]
fn omitted_property_relinquishes_ownership_without_remote_update_or_drift() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "description": "previous", "replicas": 1 }),
        false,
    );
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            address.clone(),
            DesiredResource::new(BTreeMap::from([(
                PropertyPath::Replicas,
                OwnedValue::Value(value(json!(1))),
            )]))
            .with_protection(ProtectionIntent::Unmanaged),
        )]),
    )
    .expect("desired state must be valid");
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let remote = RemoteState::try_new(
        instance,
        [(
            address,
            RemoteObservation::Present(RemoteResource::new(
                remote_id(),
                BTreeMap::from([(
                    PropertyPath::Replicas,
                    PropertyObservation::Known(value(json!(1))),
                )]),
            )),
        )],
    )
    .expect("remote observations must be valid");

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.complete());
    assert!(plan.applyable());
    assert!(plan.drift().is_empty());
    assert_eq!(plan.changes().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::NoOp);
    assert_eq!(plan.changes()[0].origin(), ChangeOrigin::Config);
    assert_eq!(plan.changes()[0].fields().len(), 1);
    assert_eq!(
        plan.changes()[0].fields()[0].key(),
        &PropertyPath::Description
    );
}

#[test]
fn omitting_source_branch_relinquishes_only_that_nested_path() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "source": { "repository": "acme/api", "branch": "main" } }),
        false,
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::SourceRepository,
            OwnedValue::Value(value(json!("acme/api"))),
        )])),
    );
    let remote = remote_state(
        instance,
        &address,
        BTreeMap::from([(
            PropertyPath::SourceRepository,
            PropertyObservation::Known(value(json!("acme/api"))),
        )]),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.complete());
    assert!(plan.drift().is_empty());
    assert_eq!(plan.changes()[0].kind(), ChangeKind::NoOp);
    assert_eq!(plan.changes()[0].fields().len(), 1);
    assert_eq!(
        plan.changes()[0].fields()[0].key(),
        &PropertyPath::SourceBranch
    );
    let checkpoint = plan.changes()[0]
        .checkpoint()
        .present()
        .expect("resource remains managed");
    assert!(
        checkpoint
            .property(&PropertyPath::SourceRepository)
            .is_some()
    );
    assert!(checkpoint.property(&PropertyPath::SourceBranch).is_none());
}

#[test]
fn omitting_one_environment_entry_relinquishes_only_that_entry() {
    let address = address("application.api");
    let kept = PropertyPath::environment_variable("FEATURE_A").expect("path must be valid");
    let relinquished = PropertyPath::environment_variable("FEATURE_B").expect("path must be valid");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "environment": { "FEATURE_A": "old-a", "FEATURE_B": "old-b" } }),
        false,
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(kept.clone(), OwnedValue::Null)])),
    );
    let remote = remote_state(
        instance,
        &address,
        BTreeMap::from([(kept.clone(), PropertyObservation::KnownAbsent)]),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.complete());
    assert_eq!(plan.changes()[0].kind(), ChangeKind::NoOp);
    assert_eq!(
        plan.changes()[0]
            .fields()
            .iter()
            .map(|field| field.key())
            .collect::<Vec<_>>(),
        vec![&kept, &relinquished]
    );
    assert_eq!(plan.drift()[0].properties(), std::slice::from_ref(&kept));
    let checkpoint = plan.changes()[0]
        .checkpoint()
        .present()
        .expect("resource remains managed");
    assert!(checkpoint.property(&kept).is_some());
    assert!(checkpoint.property(&relinquished).is_none());
}

#[test]
fn property_paths_use_config_compatible_stable_strings() {
    let environment =
        PropertyPath::environment_variable("FEATURE_FLAG").expect("path must be valid");
    let cases = [
        (PropertyPath::SourceRepository, "source.repository"),
        (PropertyPath::SourceBranch, "source.branch"),
        (environment, "environment.FEATURE_FLAG"),
        (PropertyPath::DeploymentStatus, "deployment.status"),
    ];

    for (path, expected) in cases {
        assert_eq!(path.to_string(), expected);
        assert_eq!(
            serde_json::to_string(&path).expect("path must serialize"),
            format!("\"{expected}\"")
        );
        assert_eq!(
            expected.parse::<PropertyPath>().expect("path must parse"),
            path
        );
    }
}

#[test]
fn collection_root_intents_are_explicit_and_cannot_conflict_with_children() {
    let address = address("application.api");
    DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            address.clone(),
            DesiredResource::new(BTreeMap::from([
                (PropertyPath::Source, OwnedValue::Null),
                (PropertyPath::Environment, OwnedValue::EmptyCollection),
            ])),
        )]),
    )
    .expect("explicit source clear and owned-empty environment are valid");

    let source_conflict = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            address.clone(),
            DesiredResource::new(BTreeMap::from([
                (PropertyPath::Source, OwnedValue::Null),
                (
                    PropertyPath::SourceBranch,
                    OwnedValue::Value(value(json!("main"))),
                ),
            ])),
        )]),
    )
    .expect_err("source root and child cannot coexist");
    assert!(matches!(
        source_conflict,
        DesiredStateError::ConflictingPropertyPaths { .. }
    ));

    let variable = PropertyPath::environment_variable("FEATURE_FLAG").expect("path must be valid");
    let environment_conflict = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            address,
            DesiredResource::new(BTreeMap::from([
                (PropertyPath::Environment, OwnedValue::EmptyCollection),
                (variable, OwnedValue::Sensitive),
            ])),
        )]),
    )
    .expect_err("environment root and child cannot coexist");
    assert!(matches!(
        environment_conflict,
        DesiredStateError::ConflictingPropertyPaths { .. }
    ));
}

#[test]
fn desired_source_branch_requires_a_non_null_repository() {
    let application = address("application.api");
    for branch in [OwnedValue::Null, OwnedValue::Value(value(json!("main")))] {
        let error = DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                application.clone(),
                DesiredResource::new(BTreeMap::from([(PropertyPath::SourceBranch, branch)])),
            )]),
        )
        .expect_err("a source branch without a repository is impossible");
        assert!(matches!(
            error,
            DesiredStateError::InvalidPropertyValue { .. }
        ));
    }

    for repository in [
        OwnedValue::Null,
        OwnedValue::EmptyCollection,
        OwnedValue::Sensitive,
    ] {
        let error = DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                application.clone(),
                DesiredResource::new(BTreeMap::from([(
                    PropertyPath::SourceRepository,
                    repository,
                )])),
            )]),
        )
        .expect_err("a managed source repository must be a non-null value");
        assert!(matches!(
            error,
            DesiredStateError::InvalidPropertyValue { .. }
        ));
    }

    for branch in [OwnedValue::Null, OwnedValue::Value(value(json!("main")))] {
        DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                application.clone(),
                DesiredResource::new(BTreeMap::from([
                    (
                        PropertyPath::SourceRepository,
                        OwnedValue::Value(value(json!("acme/api"))),
                    ),
                    (PropertyPath::SourceBranch, branch),
                ])),
            )]),
        )
        .expect("a non-null repository may have a null or non-null branch");
    }
}

#[test]
fn stored_source_branch_requires_a_non_null_repository() {
    let application = address("application.api");
    let instance = instance();
    for source in [json!({ "branch": "main" }), json!({ "repository": null })] {
        let state =
            state_with_resource(&application, &instance, json!({ "source": source }), false);
        let error = StoredState::try_from_state(&state)
            .expect_err("stored source shape must be structurally valid");
        assert!(matches!(
            error,
            StoredStateError::InvalidPropertyValue { .. }
        ));
    }
}

#[test]
fn remote_source_rejects_known_branch_with_explicitly_absent_repository() {
    let application = address("application.api");
    let invalid = RemoteState::try_new(
        instance(),
        [(
            application.clone(),
            RemoteObservation::Present(RemoteResource::new(
                remote_id(),
                BTreeMap::from([
                    (
                        PropertyPath::SourceRepository,
                        PropertyObservation::KnownAbsent,
                    ),
                    (
                        PropertyPath::SourceBranch,
                        PropertyObservation::Known(value(json!("main"))),
                    ),
                ]),
            )),
        )],
    )
    .expect_err("known branch cannot coexist with an absent repository");
    assert!(matches!(
        invalid,
        RemoteStateError::InvalidPropertyObservation { .. }
    ));

    RemoteState::try_new(
        instance(),
        [(
            application,
            RemoteObservation::Present(RemoteResource::new(
                remote_id(),
                BTreeMap::from([
                    (
                        PropertyPath::SourceRepository,
                        PropertyObservation::Unknown(PropertyUnknownReason::NotReturned),
                    ),
                    (
                        PropertyPath::SourceBranch,
                        PropertyObservation::Unknown(PropertyUnknownReason::NotReturned),
                    ),
                ]),
            )),
        )],
    )
    .expect("partial unknown source observations remain representable");
}

#[test]
fn root_clear_and_owned_empty_markers_compare_without_collapsing_to_omission() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "source": { "repository": "acme/api" }, "environment": {} }),
        false,
    );
    let stored = StoredState::try_from_state(&state).expect("stored roots must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([
            (PropertyPath::Source, OwnedValue::Null),
            (PropertyPath::Environment, OwnedValue::EmptyCollection),
        ])),
    );
    let remote = remote_state(
        instance,
        &address,
        BTreeMap::from([
            (
                PropertyPath::Source,
                PropertyObservation::Known(value(json!(true))),
            ),
            (
                PropertyPath::Environment,
                PropertyObservation::Known(value(json!({}))),
            ),
        ]),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.complete());
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert!(
        plan.changes()[0]
            .fields()
            .iter()
            .any(|field| field.key() == &PropertyPath::Source)
    );
    let checkpoint = plan.changes()[0]
        .checkpoint()
        .present()
        .expect("resource remains managed");
    assert!(matches!(
        checkpoint.property(&PropertyPath::Source),
        Some(CheckpointValueRef::Null)
    ));
    assert!(matches!(
        checkpoint.property(&PropertyPath::Environment),
        Some(CheckpointValueRef::EmptyCollection)
    ));
}

#[test]
fn sensitive_values_and_remote_observations_are_enforced_at_snapshot_seams() {
    let postgres = address("postgres.main");
    let application = address("application.api");
    let variable = PropertyPath::environment_variable("FEATURE_FLAG").expect("path must be valid");

    for (address, path, owned) in [
        (
            postgres.clone(),
            PropertyPath::Password,
            OwnedValue::Value(value(json!("raw-password-canary"))),
        ),
        (
            postgres.clone(),
            PropertyPath::Database,
            OwnedValue::Sensitive,
        ),
        (
            application.clone(),
            variable.clone(),
            OwnedValue::Value(value(json!("raw-env-canary"))),
        ),
    ] {
        let error = DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                address,
                DesiredResource::new(BTreeMap::from([(path, owned)])),
            )]),
        )
        .expect_err("invalid sensitivity ownership must fail");
        assert!(matches!(
            error,
            DesiredStateError::InvalidPropertyValue { .. }
        ));
        assert!(!format!("{error:?}").contains("canary"));
    }

    let known_sensitive = RemoteState::try_new(
        instance(),
        [(
            postgres.clone(),
            RemoteObservation::Present(RemoteResource::new(
                remote_id(),
                BTreeMap::from([(
                    PropertyPath::Password,
                    PropertyObservation::Known(value(json!("remote-password-canary"))),
                )]),
            )),
        )],
    )
    .expect_err("write-only values cannot enter remote observations");
    assert!(matches!(
        known_sensitive,
        RemoteStateError::InvalidPropertyObservation { .. }
    ));
    assert!(!format!("{known_sensitive:?}").contains("canary"));

    for observation in [
        PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        PropertyObservation::KnownAbsent,
    ] {
        RemoteState::try_new(
            instance(),
            [(
                postgres.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    remote_id(),
                    BTreeMap::from([(PropertyPath::Password, observation)]),
                )),
            )],
        )
        .expect("closed sensitive observations are valid");
    }
}

#[test]
fn desired_state_rejects_wrong_kind_and_invalid_dependencies() {
    let application = address("application.api");
    let postgres = address("postgres.main");
    let wrong_kind = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            application.clone(),
            DesiredResource::new(BTreeMap::from([(
                PropertyPath::Database,
                OwnedValue::Value(value(json!("app"))),
            )])),
        )]),
    )
    .expect_err("database is not an application property");
    assert!(matches!(
        wrong_kind,
        DesiredStateError::InvalidPropertyPath { .. }
    ));

    let self_dependency = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            application.clone(),
            DesiredResource::new(BTreeMap::new()).with_dependencies(vec![application.clone()]),
        )]),
    )
    .expect_err("self dependency must fail");
    assert!(matches!(
        self_dependency,
        DesiredStateError::SelfDependency { .. }
    ));

    let missing_dependency = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            application,
            DesiredResource::new(BTreeMap::new()).with_dependencies(vec![postgres]),
        )]),
    )
    .expect_err("missing dependency must fail");
    assert!(matches!(
        missing_dependency,
        DesiredStateError::MissingDependency { .. }
    ));
}

#[test]
fn stored_and_remote_snapshots_reject_paths_for_the_wrong_resource_kind() {
    let application = address("application.api");
    let instance = instance();
    let state = state_with_resource(
        &application,
        &instance,
        json!({ "database": "wrong-kind-canary" }),
        false,
    );
    let stored_error =
        StoredState::try_from_state(&state).expect_err("stored path must match the resource kind");
    assert!(matches!(
        stored_error,
        StoredStateError::InvalidPropertyPath { .. }
    ));
    assert!(!format!("{stored_error:?}").contains("wrong-kind-canary"));

    let remote_error = RemoteState::try_new(
        instance,
        [(
            application,
            RemoteObservation::Present(RemoteResource::new(
                remote_id(),
                BTreeMap::from([(
                    PropertyPath::Database,
                    PropertyObservation::Known(value(json!("wrong-kind-canary"))),
                )]),
            )),
        )],
    )
    .expect_err("remote path must match the resource kind");
    assert!(matches!(
        remote_error,
        RemoteStateError::InvalidPropertyPath { .. }
    ));
    assert!(!format!("{remote_error:?}").contains("wrong-kind-canary"));
}

#[test]
fn explicit_null_is_owned_and_differs_from_an_omitted_property() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "description": "previous" }),
        false,
    );
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Description,
            OwnedValue::Null,
        )])),
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let remote = remote_state(
        instance,
        &address,
        BTreeMap::from([(
            PropertyPath::Description,
            PropertyObservation::Known(value(json!("previous"))),
        )]),
    );

    let plan = plan(&desired, &stored, &remote);

    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    assert_eq!(plan.changes()[0].fields()[0].desired(), ValueState::Null);
    assert_ne!(
        plan.changes()[0].fields()[0].desired(),
        ValueState::Unmanaged
    );
}

#[test]
fn a_partial_remote_property_read_makes_the_plan_incomplete() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "description": "old", "replicas": 1 }),
        false,
    );
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([
            (
                PropertyPath::Description,
                OwnedValue::Value(value(json!("new"))),
            ),
            (PropertyPath::Replicas, OwnedValue::Value(value(json!(2)))),
        ])),
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let remote = remote_state(
        instance,
        &address,
        BTreeMap::from([(
            PropertyPath::Description,
            PropertyObservation::Known(value(json!("old"))),
        )]),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(!plan.complete());
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::MissingPropertyObservation
    );
    assert_eq!(
        plan.diagnostics()[0].property(),
        Some(&PropertyPath::Replicas)
    );
}

#[test]
fn sensitive_unknown_property_blocks_planning_without_leaking_values() {
    let address = address("application.api");
    let secret_path =
        PropertyPath::environment_variable("FEATURE_FLAG").expect("environment path must be valid");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "environment": { "FEATURE_FLAG": "stored-secret-canary" } }),
        false,
    );
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            secret_path.clone(),
            OwnedValue::Sensitive,
        )])),
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let remote = RemoteState::try_new(
        instance,
        [(
            address,
            RemoteObservation::Present(RemoteResource::new(
                remote_id(),
                BTreeMap::from([(
                    secret_path,
                    PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
                )]),
            )),
        )],
    )
    .expect("remote state must be valid");

    let plan = plan(&desired, &stored, &remote);
    let json = String::from_utf8(plan.to_json_bytes()).expect("plan JSON must be UTF-8");
    let debug = format!("{desired:?}{stored:?}{remote:?}{plan:?}");

    assert!(!plan.complete());
    assert!(!plan.applyable());
    assert_eq!(
        plan.diagnostics()[0].property_unknown(),
        Some(PropertyUnknownReason::Sensitive)
    );
    let canary = "stored-secret-canary";
    assert!(!json.contains(canary));
    assert!(!debug.contains(canary));
}

#[test]
fn protection_true_and_false_are_state_only_checkpoints_and_delete_uses_stored_value() {
    let address = address("application.api");
    let instance = instance();
    let inputs = json!({ "replicas": 1 });
    let desired_properties =
        BTreeMap::from([(PropertyPath::Replicas, OwnedValue::Value(value(json!(1))))]);
    let remote_properties = BTreeMap::from([(
        PropertyPath::Replicas,
        PropertyObservation::Known(value(json!(1))),
    )]);

    let unprotected_state = state_with_resource(&address, &instance, inputs.clone(), false);
    let stored = StoredState::try_from_state(&unprotected_state).expect("state must project");
    let protect = desired_state(
        &address,
        DesiredResource::new(desired_properties.clone())
            .with_protection(ProtectionIntent::Set(true)),
    );
    let remote = remote_state(instance.clone(), &address, remote_properties.clone());
    let protect_plan = plan(&protect, &stored, &remote);

    assert_eq!(protect_plan.changes()[0].kind(), ChangeKind::NoOp);
    assert_eq!(protect_plan.changes()[0].origin(), ChangeOrigin::Config);
    assert_eq!(
        protect_plan.changes()[0].metadata(),
        &[MetadataChangeKind::Protection]
    );
    assert!(
        protect_plan.changes()[0]
            .checkpoint()
            .present()
            .expect("resource remains managed")
            .protected()
    );

    let protected_state = state_with_resource(&address, &instance, inputs.clone(), true);
    let stored = StoredState::try_from_state(&protected_state).expect("state must project");
    let unprotect = desired_state(
        &address,
        DesiredResource::new(desired_properties).with_protection(ProtectionIntent::Set(false)),
    );
    let remote = remote_state(instance.clone(), &address, remote_properties.clone());
    let unprotect_plan = plan(&unprotect, &stored, &remote);

    assert_eq!(unprotect_plan.changes()[0].kind(), ChangeKind::NoOp);
    assert_eq!(
        unprotect_plan.changes()[0].metadata(),
        &[MetadataChangeKind::Protection]
    );
    assert!(
        !unprotect_plan.changes()[0]
            .checkpoint()
            .present()
            .expect("resource remains managed")
            .protected()
    );

    let still_protected_removal =
        DesiredState::try_new(digest(), BTreeMap::new()).expect("desired state must be valid");
    let blocked = plan(&still_protected_removal, &stored, &remote);
    assert!(!blocked.applyable());
    assert_eq!(
        blocked.diagnostics()[0].code(),
        PlanDiagnosticCode::ProtectedDelete
    );

    let checkpointed_state = state_with_resource(&address, &instance, inputs, false);
    let checkpointed =
        StoredState::try_from_state(&checkpointed_state).expect("state must project");
    let remote = remote_state(instance, &address, remote_properties);
    let deletion = plan(&still_protected_removal, &checkpointed, &remote);
    assert!(deletion.applyable());
    assert_eq!(deletion.changes()[0].kind(), ChangeKind::Delete);
    assert!(deletion.changes()[0].checkpoint().is_absent());
}

#[test]
fn explicit_false_matching_effective_stored_protection_needs_no_checkpoint() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(&address, &instance, json!({ "replicas": 1 }), false);
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Replicas,
            OwnedValue::Value(value(json!(1))),
        )]))
        .with_protection(ProtectionIntent::Set(false)),
    );
    let remote = remote_state(
        instance,
        &address,
        BTreeMap::from([(
            PropertyPath::Replicas,
            PropertyObservation::Known(value(json!(1))),
        )]),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.changes().is_empty());
}

#[test]
fn dependency_change_is_a_state_only_checkpoint() {
    let resource_address = address("application.api");
    let old_dependency = address("postgres.old");
    let new_dependency = address("postgres.main");
    let instance = instance();
    let state = state_with_resource_details(
        &resource_address,
        &instance,
        ResourceKind::Application,
        "remote-1",
        json!({ "replicas": 1 }),
        false,
        vec![old_dependency],
    );
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from([
            (
                resource_address.clone(),
                DesiredResource::new(BTreeMap::from([(
                    PropertyPath::Replicas,
                    OwnedValue::Value(value(json!(1))),
                )]))
                .with_dependencies(vec![new_dependency.clone()]),
            ),
            (
                new_dependency.clone(),
                DesiredResource::new(BTreeMap::new()),
            ),
        ]),
    )
    .expect("desired state must be valid");
    let remote = RemoteState::try_new(
        instance,
        [
            (
                resource_address.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    remote_id(),
                    BTreeMap::from([(
                        PropertyPath::Replicas,
                        PropertyObservation::Known(value(json!(1))),
                    )]),
                )),
            ),
            (new_dependency.clone(), RemoteObservation::Missing),
        ],
    )
    .expect("remote observations must be valid");

    let plan = plan(&desired, &stored, &remote);
    let checkpoint_change = plan
        .changes()
        .iter()
        .find(|change| change.address() == &resource_address)
        .expect("application has a dependency checkpoint");

    assert_eq!(checkpoint_change.kind(), ChangeKind::NoOp);
    assert_eq!(
        checkpoint_change.metadata(),
        &[MetadataChangeKind::Dependencies]
    );
    assert_eq!(
        checkpoint_change
            .checkpoint()
            .present()
            .expect("resource remains managed")
            .dependencies(),
        &[new_dependency]
    );
}

#[test]
fn create_changes_are_ordered_dependency_first() {
    let project = address("project.platform");
    let environment = address("environment.production");
    let application = address("application.api");
    let instance = instance();
    let stored =
        StoredState::try_from_state(&StateFile::new(Version::new(0, 1, 0), instance.clone()))
            .expect("empty state must project");
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from([
            (
                application.clone(),
                DesiredResource::new(BTreeMap::new()).with_dependencies(vec![environment.clone()]),
            ),
            (
                environment.clone(),
                DesiredResource::new(BTreeMap::new()).with_dependencies(vec![project.clone()]),
            ),
            (project.clone(), DesiredResource::new(BTreeMap::new())),
        ]),
    )
    .expect("desired dependency graph must be valid");
    let remote = RemoteState::try_new(
        instance,
        [
            (application.clone(), RemoteObservation::Missing),
            (environment.clone(), RemoteObservation::Missing),
            (project.clone(), RemoteObservation::Missing),
        ],
    )
    .expect("remote observations must be valid");

    let plan = plan(&desired, &stored, &remote);

    assert_eq!(
        plan.changes()
            .iter()
            .map(|change| change.address())
            .collect::<Vec<_>>(),
        vec![&project, &environment, &application]
    );
}

#[test]
fn diamond_dependencies_use_lexical_ties_and_stable_json() {
    let root = address("project.platform");
    let postgres = address("postgres.main");
    let redis = address("redis.cache");
    let leaf = address("application.api");
    let instance = instance();
    let stored =
        StoredState::try_from_state(&StateFile::new(Version::new(0, 1, 0), instance.clone()))
            .expect("empty state must project");
    let resources = |leaf_dependencies: Vec<ResourceAddress>, reverse: bool| {
        let mut entries = vec![
            (
                leaf.clone(),
                DesiredResource::new(BTreeMap::new()).with_dependencies(leaf_dependencies),
            ),
            (
                postgres.clone(),
                DesiredResource::new(BTreeMap::new()).with_dependencies(vec![root.clone()]),
            ),
            (
                redis.clone(),
                DesiredResource::new(BTreeMap::new()).with_dependencies(vec![root.clone()]),
            ),
            (root.clone(), DesiredResource::new(BTreeMap::new())),
        ];
        if reverse {
            entries.reverse();
        }
        entries.into_iter().collect()
    };
    let desired_a = DesiredState::try_new(
        digest(),
        resources(vec![redis.clone(), postgres.clone()], false),
    )
    .expect("desired graph must be valid");
    let desired_b = DesiredState::try_new(
        digest(),
        resources(vec![postgres.clone(), redis.clone()], true),
    )
    .expect("desired graph must be valid");
    let observations_a = [
        (redis.clone(), RemoteObservation::Missing),
        (leaf.clone(), RemoteObservation::Missing),
        (root.clone(), RemoteObservation::Missing),
        (postgres.clone(), RemoteObservation::Missing),
    ];
    let observations_b = [
        (postgres.clone(), RemoteObservation::Missing),
        (root.clone(), RemoteObservation::Missing),
        (leaf.clone(), RemoteObservation::Missing),
        (redis.clone(), RemoteObservation::Missing),
    ];
    let remote_a =
        RemoteState::try_new(instance.clone(), observations_a).expect("remote state must be valid");
    let remote_b =
        RemoteState::try_new(instance, observations_b).expect("remote state must be valid");

    let plan_a = plan(&desired_a, &stored, &remote_a);
    let plan_b = plan(&desired_b, &stored, &remote_b);

    assert_eq!(
        plan_a
            .changes()
            .iter()
            .map(|change| change.address())
            .collect::<Vec<_>>(),
        vec![&root, &postgres, &redis, &leaf]
    );
    assert_eq!(plan_a.to_json_bytes(), plan_b.to_json_bytes());
}

#[test]
fn independent_changes_use_lexical_address_order() {
    let application = address("application.api");
    let postgres = address("postgres.main");
    let redis = address("redis.cache");
    let instance = instance();
    let stored =
        StoredState::try_from_state(&StateFile::new(Version::new(0, 1, 0), instance.clone()))
            .expect("empty state must project");
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from([
            (redis.clone(), DesiredResource::new(BTreeMap::new())),
            (application.clone(), DesiredResource::new(BTreeMap::new())),
            (postgres.clone(), DesiredResource::new(BTreeMap::new())),
        ]),
    )
    .expect("desired state must be valid");
    let remote = RemoteState::try_new(
        instance,
        [
            (redis.clone(), RemoteObservation::Missing),
            (postgres.clone(), RemoteObservation::Missing),
            (application.clone(), RemoteObservation::Missing),
        ],
    )
    .expect("remote state must be valid");

    let plan = plan(&desired, &stored, &remote);

    assert_eq!(
        plan.changes()
            .iter()
            .map(|change| change.address())
            .collect::<Vec<_>>(),
        vec![&application, &postgres, &redis]
    );
}

#[test]
fn desired_dependency_cycle_blocks_without_an_execution_order() {
    let application = address("application.api");
    let postgres = address("postgres.main");
    let instance = instance();
    let stored =
        StoredState::try_from_state(&StateFile::new(Version::new(0, 1, 0), instance.clone()))
            .expect("empty state must project");
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from([
            (
                application.clone(),
                DesiredResource::new(BTreeMap::new()).with_dependencies(vec![postgres.clone()]),
            ),
            (
                postgres.clone(),
                DesiredResource::new(BTreeMap::new()).with_dependencies(vec![application.clone()]),
            ),
        ]),
    )
    .expect("cycles are a planner concern");
    let remote = RemoteState::try_new(
        instance,
        [
            (postgres.clone(), RemoteObservation::Missing),
            (application.clone(), RemoteObservation::Missing),
        ],
    )
    .expect("remote state must be valid");

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.complete());
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert_eq!(plan.diagnostics().len(), 2);
    assert!(
        plan.diagnostics()
            .iter()
            .all(|issue| issue.code() == PlanDiagnosticCode::DesiredDependencyCycle)
    );
    assert_eq!(
        plan.diagnostics()
            .iter()
            .map(|issue| issue.address().expect("cycle member has an address"))
            .collect::<Vec<_>>(),
        vec![&application, &postgres]
    );
}

#[test]
fn stored_removal_cycle_blocks_without_an_execution_order() {
    let application = address("application.api");
    let postgres = address("postgres.main");
    let instance = instance();
    let mut state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    insert_state_resource(
        &mut state,
        application.clone(),
        "application-1",
        vec![postgres.clone()],
    );
    insert_state_resource(
        &mut state,
        postgres.clone(),
        "postgres-1",
        vec![application.clone()],
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let desired =
        DesiredState::try_new(digest(), BTreeMap::new()).expect("desired state must be valid");
    let remote = RemoteState::try_new(
        instance,
        [
            (
                postgres.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("postgres-1").expect("remote id must be valid"),
                    BTreeMap::new(),
                )),
            ),
            (
                application.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("application-1").expect("remote id must be valid"),
                    BTreeMap::new(),
                )),
            ),
        ],
    )
    .expect("remote state must be valid");

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.complete());
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert_eq!(plan.diagnostics().len(), 2);
    assert!(
        plan.diagnostics()
            .iter()
            .all(|issue| issue.code() == PlanDiagnosticCode::StoredDependencyCycle)
    );
    assert_eq!(
        plan.diagnostics()
            .iter()
            .map(|issue| issue.address().expect("cycle member has an address"))
            .collect::<Vec<_>>(),
        vec![&application, &postgres]
    );
}

#[test]
fn delete_and_forget_changes_are_ordered_dependent_first() {
    let project = address("project.platform");
    let environment = address("environment.production");
    let application = address("application.api");
    let instance = instance();
    let mut state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    insert_state_resource(&mut state, project.clone(), "project-1", Vec::new());
    insert_state_resource(
        &mut state,
        environment.clone(),
        "environment-1",
        vec![project.clone()],
    );
    insert_state_resource(
        &mut state,
        application.clone(),
        "application-1",
        vec![environment.clone()],
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let desired =
        DesiredState::try_new(digest(), BTreeMap::new()).expect("desired state must be valid");
    let remote = RemoteState::try_new(
        instance,
        [
            (
                project.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("project-1").expect("remote id must be valid"),
                    BTreeMap::new(),
                )),
            ),
            (environment.clone(), RemoteObservation::Missing),
            (
                application.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("application-1").expect("remote id must be valid"),
                    BTreeMap::new(),
                )),
            ),
        ],
    )
    .expect("remote state must be valid");

    let plan = plan(&desired, &stored, &remote);

    assert_eq!(
        plan.changes()
            .iter()
            .map(|change| (change.address(), change.kind()))
            .collect::<Vec<_>>(),
        vec![
            (&application, ChangeKind::Delete),
            (&environment, ChangeKind::Forget),
            (&project, ChangeKind::Delete),
        ]
    );
}

#[test]
fn independent_removals_keep_lexical_ties_and_stable_json() {
    let application = address("application.api");
    let postgres = address("postgres.main");
    let redis = address("redis.cache");
    let instance = instance();
    let base = StateFile::new(Version::new(0, 1, 0), instance.clone());
    let mut state_a = base.clone();
    insert_state_resource(&mut state_a, redis.clone(), "redis-1", Vec::new());
    insert_state_resource(
        &mut state_a,
        application.clone(),
        "application-1",
        Vec::new(),
    );
    insert_state_resource(&mut state_a, postgres.clone(), "postgres-1", Vec::new());
    let mut state_b = base;
    insert_state_resource(&mut state_b, postgres.clone(), "postgres-1", Vec::new());
    insert_state_resource(
        &mut state_b,
        application.clone(),
        "application-1",
        Vec::new(),
    );
    insert_state_resource(&mut state_b, redis.clone(), "redis-1", Vec::new());
    let stored_a = StoredState::try_from_state(&state_a).expect("stored state must project");
    let stored_b = StoredState::try_from_state(&state_b).expect("stored state must project");
    let desired =
        DesiredState::try_new(digest(), BTreeMap::new()).expect("desired state must be valid");
    let present = |remote_id: &str| {
        RemoteObservation::Present(RemoteResource::new(
            RemoteId::new(remote_id).expect("remote id must be valid"),
            BTreeMap::new(),
        ))
    };
    let remote_a = RemoteState::try_new(
        instance.clone(),
        [
            (redis.clone(), RemoteObservation::Missing),
            (application.clone(), present("application-1")),
            (postgres.clone(), present("postgres-1")),
        ],
    )
    .expect("remote state must be valid");
    let remote_b = RemoteState::try_new(
        instance,
        [
            (postgres.clone(), present("postgres-1")),
            (application.clone(), present("application-1")),
            (redis.clone(), RemoteObservation::Missing),
        ],
    )
    .expect("remote state must be valid");

    let plan_a = plan(&desired, &stored_a, &remote_a);
    let plan_b = plan(&desired, &stored_b, &remote_b);

    assert_eq!(
        plan_a
            .changes()
            .iter()
            .map(|change| (change.address(), change.kind()))
            .collect::<Vec<_>>(),
        vec![
            (&application, ChangeKind::Delete),
            (&postgres, ChangeKind::Delete),
            (&redis, ChangeKind::Forget),
        ]
    );
    assert_eq!(plan_a.to_json_bytes(), plan_b.to_json_bytes());
}

#[test]
fn mixed_plans_finish_dependency_first_changes_before_removals() {
    let project = address("project.shared");
    let environment = address("environment.new");
    let updated = address("application.updated");
    let removed = address("application.old");
    let instance = instance();
    let mut state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    insert_state_resource(&mut state, project.clone(), "project-1", Vec::new());
    state
        .upsert_resource(
            updated.clone(),
            ResourceState::new(
                ResourceKind::Application,
                RemoteId::new("updated-1").expect("remote id must be valid"),
                false,
                ManagedInputs::try_from_json(json!({ "description": "old" }))
                    .expect("managed inputs must be valid"),
                Vec::new(),
            ),
        )
        .expect("state insert must succeed");
    insert_state_resource(
        &mut state,
        removed.clone(),
        "removed-1",
        vec![project.clone()],
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from([
            (
                project.clone(),
                DesiredResource::new(BTreeMap::new()).with_protection(ProtectionIntent::Set(true)),
            ),
            (
                environment.clone(),
                DesiredResource::new(BTreeMap::new()).with_dependencies(vec![project.clone()]),
            ),
            (
                updated.clone(),
                DesiredResource::new(BTreeMap::from([(
                    PropertyPath::Description,
                    OwnedValue::Value(value(json!("new"))),
                )]))
                .with_dependencies(vec![environment.clone()]),
            ),
        ]),
    )
    .expect("desired graph must be valid");
    let remote = RemoteState::try_new(
        instance,
        [
            (
                removed.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("removed-1").expect("remote id must be valid"),
                    BTreeMap::new(),
                )),
            ),
            (
                updated.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("updated-1").expect("remote id must be valid"),
                    BTreeMap::from([(
                        PropertyPath::Description,
                        PropertyObservation::Known(value(json!("old"))),
                    )]),
                )),
            ),
            (environment.clone(), RemoteObservation::Missing),
            (
                project.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("project-1").expect("remote id must be valid"),
                    BTreeMap::new(),
                )),
            ),
        ],
    )
    .expect("remote state must be valid");

    let plan = plan(&desired, &stored, &remote);

    assert_eq!(
        plan.changes()
            .iter()
            .map(|change| (change.address(), change.kind()))
            .collect::<Vec<_>>(),
        vec![
            (&project, ChangeKind::NoOp),
            (&environment, ChangeKind::Create),
            (&updated, ChangeKind::Update),
            (&removed, ChangeKind::Delete),
        ]
    );
}

#[test]
fn unchanged_dependencies_do_not_create_state_only_changes() {
    let project = address("project.platform");
    let application = address("application.api");
    let instance = instance();
    let mut state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    insert_state_resource(&mut state, project.clone(), "project-1", Vec::new());
    insert_state_resource(
        &mut state,
        application.clone(),
        "application-1",
        vec![project.clone()],
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from([
            (project.clone(), DesiredResource::new(BTreeMap::new())),
            (
                application.clone(),
                DesiredResource::new(BTreeMap::new()).with_dependencies(vec![project.clone()]),
            ),
        ]),
    )
    .expect("desired graph must be valid");
    let remote = RemoteState::try_new(
        instance,
        [
            (
                application,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("application-1").expect("remote id must be valid"),
                    BTreeMap::new(),
                )),
            ),
            (
                project,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("project-1").expect("remote id must be valid"),
                    BTreeMap::new(),
                )),
            ),
        ],
    )
    .expect("remote state must be valid");

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.complete());
    assert!(plan.applyable());
    assert!(plan.changes().is_empty());
}

#[test]
fn move_and_removal_directives_block_instead_of_degrading_into_create_or_delete() {
    let old = address("application.backend");
    let new = address("application.api");
    let instance = instance();
    let state = state_with_resource(&old, &instance, json!({ "replicas": 1 }), false);
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired = desired_state(
        &new,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Replicas,
            OwnedValue::Value(value(json!(1))),
        )])),
    )
    .with_moves(vec![MoveDirective::new(old.clone(), new)]);
    let remote = RemoteState::try_new(instance.clone(), [])
        .expect("empty remote state is structurally valid");

    let move_plan = plan(&desired, &stored, &remote);

    assert!(move_plan.complete());
    assert!(!move_plan.applyable());
    assert!(move_plan.changes().is_empty());
    assert_eq!(
        move_plan.diagnostics()[0].unsupported(),
        Some(UnsupportedDirectiveKind::Move)
    );

    let desired = DesiredState::try_new(digest(), BTreeMap::new())
        .expect("desired state must be valid")
        .with_removals(vec![RemovalDirective::new(old, true)]);
    let removal_plan = plan(&desired, &stored, &remote);

    assert!(!removal_plan.applyable());
    assert!(removal_plan.changes().is_empty());
    assert_eq!(
        removal_plan.diagnostics()[0].unsupported(),
        Some(UnsupportedDirectiveKind::Removal)
    );
}

#[test]
fn ignore_and_replacement_metadata_block_until_supported() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(&address, &instance, json!({ "replicas": 1 }), false);
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Replicas,
            OwnedValue::Value(value(json!(2))),
        )]))
        .with_ignored_changes(vec![PropertyPath::SourceBranch])
        .with_replacement_changes(vec![PropertyPath::DeploymentStatus]),
    );
    let remote = remote_state(
        instance,
        &address,
        BTreeMap::from([(
            PropertyPath::Replicas,
            PropertyObservation::Known(value(json!(1))),
        )]),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.complete());
    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert_eq!(plan.diagnostics().len(), 2);
    assert_eq!(
        plan.diagnostics()[0].unsupported(),
        Some(UnsupportedDirectiveKind::IgnoreChanges)
    );
    assert_eq!(
        plan.diagnostics()[0].property(),
        Some(&PropertyPath::SourceBranch)
    );
    assert_eq!(
        plan.diagnostics()[1].unsupported(),
        Some(UnsupportedDirectiveKind::Replacement)
    );
    assert_eq!(
        plan.diagnostics()[1].property(),
        Some(&PropertyPath::DeploymentStatus)
    );
}

#[test]
fn stored_state_rejects_duplicate_same_kind_identity_but_allows_same_raw_id_across_kinds() {
    let instance = instance();
    let first = address("application.api");
    let second = address("application.worker");
    let mut duplicate = StateFile::new(Version::new(0, 1, 0), instance.clone());
    for address in [&first, &second] {
        duplicate
            .upsert_resource(
                address.clone(),
                ResourceState::new(
                    ResourceKind::Application,
                    RemoteId::new("shared-id").expect("remote id must be valid"),
                    false,
                    ManagedInputs::try_from_json(json!({ "replicas": 1 }))
                        .expect("inputs must be valid"),
                    Vec::new(),
                ),
            )
            .expect("state insert must succeed");
    }

    let error = StoredState::try_from_state(&duplicate)
        .expect_err("same-kind duplicate physical identity must fail");
    assert!(matches!(
        error,
        StoredStateError::DuplicateRemoteIdentity { .. }
    ));
    assert!(!error.to_string().contains("shared-id"));

    let postgres = address("postgres.main");
    let mut cross_kind = StateFile::new(Version::new(0, 1, 0), instance);
    cross_kind
        .upsert_resource(
            first,
            ResourceState::new(
                ResourceKind::Application,
                RemoteId::new("shared-id").expect("remote id must be valid"),
                false,
                ManagedInputs::try_from_json(json!({ "replicas": 1 }))
                    .expect("inputs must be valid"),
                Vec::new(),
            ),
        )
        .expect("state insert must succeed");
    cross_kind
        .upsert_resource(
            postgres,
            ResourceState::new(
                ResourceKind::Postgres,
                RemoteId::new("shared-id").expect("remote id must be valid"),
                false,
                ManagedInputs::try_from_json(json!({ "database": "app" }))
                    .expect("inputs must be valid"),
                Vec::new(),
            ),
        )
        .expect("state insert must succeed");

    StoredState::try_from_state(&cross_kind)
        .expect("the identity namespace includes resource kind");
}

#[test]
fn remote_state_uses_resource_kind_as_part_of_physical_identity() {
    let application = address("application.api");
    let worker = address("application.worker");
    let postgres = address("postgres.main");
    let observation = || {
        RemoteObservation::Present(RemoteResource::new(
            RemoteId::new("shared-id").expect("remote id must be valid"),
            BTreeMap::new(),
        ))
    };

    RemoteState::try_new(
        instance(),
        [
            (application.clone(), observation()),
            (postgres, observation()),
        ],
    )
    .expect("same raw identity across resource kinds is valid");

    let error = RemoteState::try_new(
        instance(),
        [(application, observation()), (worker, observation())],
    )
    .expect_err("same-kind duplicate physical identity must fail");
    assert!(!error.to_string().contains("shared-id"));
}

#[test]
fn three_way_property_matrix_preserves_config_drift_and_combined_origins() {
    let cases = [
        (2, 1, ChangeKind::Update, ChangeOrigin::Config, false),
        (1, 2, ChangeKind::Update, ChangeOrigin::Drift, true),
        (2, 3, ChangeKind::Update, ChangeOrigin::ConfigAndDrift, true),
        (2, 2, ChangeKind::NoOp, ChangeOrigin::ConfigAndDrift, true),
    ];

    for (desired_replicas, remote_replicas, kind, origin, has_drift) in cases {
        let plan = replicas_plan(1, desired_replicas, remote_replicas);
        assert_eq!(plan.changes()[0].kind(), kind);
        assert_eq!(plan.changes()[0].origin(), origin);
        assert_eq!(!plan.drift().is_empty(), has_drift);
    }
}

#[test]
fn no_op_change_carries_an_exact_redacted_checkpoint_target() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "description": "stored-checkpoint-canary" }),
        false,
    );
    let stored = StoredState::try_from_state(&state).expect("stored state must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Description,
            OwnedValue::Value(value(json!("desired-checkpoint-canary"))),
        )]))
        .with_protection(ProtectionIntent::Set(true)),
    );
    let remote = remote_state(
        instance,
        &address,
        BTreeMap::from([(
            PropertyPath::Description,
            PropertyObservation::Known(value(json!("desired-checkpoint-canary"))),
        )]),
    );

    let plan = plan(&desired, &stored, &remote);
    let change = &plan.changes()[0];
    let checkpoint = change
        .checkpoint()
        .present()
        .expect("no-op keeps the resource managed");

    assert_eq!(change.kind(), ChangeKind::NoOp);
    assert!(checkpoint.protected());
    assert!(checkpoint.dependencies().is_empty());
    match checkpoint
        .property(&PropertyPath::Description)
        .expect("description remains owned")
    {
        CheckpointValueRef::NonSensitive(value) => {
            assert_eq!(value, &json!("desired-checkpoint-canary"));
        }
        other => panic!("unexpected checkpoint value: {other:?}"),
    }
    let json = String::from_utf8(plan.to_json_bytes()).expect("plan JSON must be UTF-8");
    let debug = format!("{plan:?}");
    for canary in ["stored-checkpoint-canary", "desired-checkpoint-canary"] {
        assert!(!json.contains(canary));
        assert!(!debug.contains(canary));
    }
}

#[test]
fn creates_only_after_explicit_remote_absence() {
    let address = address("application.api");
    let instance = instance();
    let state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Replicas,
            OwnedValue::Value(value(json!(1))),
        )])),
    );
    let unobserved = RemoteState::try_new(instance.clone(), [])
        .expect("empty remote state is structurally valid");
    let incomplete = plan(&desired, &stored, &unobserved);
    assert!(!incomplete.complete());
    assert!(incomplete.changes().is_empty());

    let missing = RemoteState::try_new(instance, [(address, RemoteObservation::Missing)])
        .expect("remote state must be valid");
    let create = plan(&desired, &stored, &missing);
    assert!(create.complete());
    assert_eq!(create.changes()[0].kind(), ChangeKind::Create);
    assert_eq!(create.changes()[0].origin(), ChangeOrigin::Config);
    assert!(create.changes()[0].checkpoint().present().is_some());
}

#[test]
fn managed_remote_deletion_recreates_or_forgets_according_to_desired_presence() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(&address, &instance, json!({ "replicas": 1 }), false);
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let remote = RemoteState::try_new(instance, [(address.clone(), RemoteObservation::Missing)])
        .expect("remote state must be valid");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Replicas,
            OwnedValue::Value(value(json!(1))),
        )])),
    );

    let recreate = plan(&desired, &stored, &remote);
    assert_eq!(recreate.changes()[0].kind(), ChangeKind::Create);
    assert_eq!(recreate.changes()[0].origin(), ChangeOrigin::Drift);
    assert_eq!(recreate.drift()[0].kind(), dokploy_core::DriftKind::Deleted);

    let removed =
        DesiredState::try_new(digest(), BTreeMap::new()).expect("desired state must be valid");
    let forget = plan(&removed, &stored, &remote);
    assert_eq!(forget.changes()[0].kind(), ChangeKind::Forget);
    assert_eq!(forget.changes()[0].origin(), ChangeOrigin::ConfigAndDrift);
    assert!(forget.changes()[0].checkpoint().is_absent());
}

#[test]
fn resource_level_identity_and_availability_fail_closed() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(&address, &instance, json!({ "replicas": 1 }), false);
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Replicas,
            OwnedValue::Value(value(json!(1))),
        )])),
    );

    let unavailable = RemoteState::try_new(
        instance.clone(),
        [(
            address.clone(),
            RemoteObservation::Unavailable(RemoteFailureKind::Unavailable),
        )],
    )
    .expect("remote state must be valid");
    let unavailable_plan = plan(&desired, &stored, &unavailable);
    assert!(!unavailable_plan.complete());
    assert_eq!(
        unavailable_plan.diagnostics()[0].remote_failure(),
        Some(RemoteFailureKind::Unavailable)
    );

    let mismatch = RemoteState::try_new(
        instance.clone(),
        [(
            address.clone(),
            RemoteObservation::Present(RemoteResource::new(
                RemoteId::new("different-id").expect("remote id must be valid"),
                BTreeMap::new(),
            )),
        )],
    )
    .expect("remote state must be valid");
    let mismatch_plan = plan(&desired, &stored, &mismatch);
    assert_eq!(
        mismatch_plan.diagnostics()[0].code(),
        PlanDiagnosticCode::RemoteIdentityMismatch
    );

    let other_instance =
        InstanceIdentity::parse("https://other.example.test").expect("instance must be valid");
    let wrong_instance =
        RemoteState::try_new(other_instance, []).expect("remote state must be valid");
    let instance_plan = plan(&desired, &stored, &wrong_instance);
    assert_eq!(
        instance_plan.diagnostics()[0].code(),
        PlanDiagnosticCode::InstanceMismatch
    );
}

#[test]
fn unmanaged_collision_blocks_but_unrelated_remote_only_resource_is_untouched() {
    let desired_address = address("application.api");
    let manual_address = address("application.manual");
    let instance = instance();
    let state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired = desired_state(
        &desired_address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Replicas,
            OwnedValue::Value(value(json!(1))),
        )])),
    );
    let remote = RemoteState::try_new(
        instance,
        [
            (
                desired_address,
                RemoteObservation::Present(RemoteResource::new(remote_id(), BTreeMap::new())),
            ),
            (
                manual_address,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("manual-id").expect("remote id must be valid"),
                    BTreeMap::new(),
                )),
            ),
        ],
    )
    .expect("remote state must be valid");

    let plan = plan(&desired, &stored, &remote);

    assert!(!plan.applyable());
    assert!(plan.changes().is_empty());
    assert!(plan.drift().is_empty());
    assert_eq!(
        plan.diagnostics()[0].code(),
        PlanDiagnosticCode::UnmanagedAddressCollision
    );
}

#[test]
fn canonical_json_is_deterministic_and_never_contains_property_values_or_remote_ids() {
    let address = address("application.api");
    let secret_path =
        PropertyPath::environment_variable("FEATURE_FLAG").expect("environment path must be valid");
    let instance = instance();
    let state = state_with_resource_details(
        &address,
        &instance,
        ResourceKind::Application,
        "remote-id-canary",
        json!({
            "description": "stored-value-canary",
            "environment": { "FEATURE_FLAG": "stored-secret-canary" }
        }),
        false,
        Vec::new(),
    );
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired_properties_a = [
        (
            PropertyPath::Description,
            OwnedValue::Value(value(json!("desired-value-canary"))),
        ),
        (secret_path.clone(), OwnedValue::Sensitive),
    ]
    .into_iter()
    .collect();
    let desired_properties_b = [
        (secret_path.clone(), OwnedValue::Sensitive),
        (
            PropertyPath::Description,
            OwnedValue::Value(value(json!("desired-value-canary"))),
        ),
    ]
    .into_iter()
    .collect();
    let remote_properties_a = [
        (
            PropertyPath::Description,
            PropertyObservation::Known(value(json!("stored-value-canary"))),
        ),
        (
            secret_path.clone(),
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
    ]
    .into_iter()
    .collect();
    let remote_properties_b = [
        (
            secret_path,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
        (
            PropertyPath::Description,
            PropertyObservation::Known(value(json!("stored-value-canary"))),
        ),
    ]
    .into_iter()
    .collect();
    let desired_a = desired_state(&address, DesiredResource::new(desired_properties_a));
    let desired_b = desired_state(&address, DesiredResource::new(desired_properties_b));
    let remote_a = RemoteState::try_new(
        instance.clone(),
        [(
            address.clone(),
            RemoteObservation::Present(RemoteResource::new(
                RemoteId::new("remote-id-canary").expect("remote id must be valid"),
                remote_properties_a,
            )),
        )],
    )
    .expect("remote state must be valid");
    let remote_b = RemoteState::try_new(
        instance,
        [(
            address,
            RemoteObservation::Present(RemoteResource::new(
                RemoteId::new("remote-id-canary").expect("remote id must be valid"),
                remote_properties_b,
            )),
        )],
    )
    .expect("remote state must be valid");

    let plan_a = plan(&desired_a, &stored, &remote_a);
    let plan_b = plan(&desired_b, &stored, &remote_b);
    let json_a = plan_a.to_json_bytes();
    let json_b = plan_b.to_json_bytes();
    let rendered = String::from_utf8(json_a.clone()).expect("plan JSON must be UTF-8");
    let debug = format!("{desired_a:?}{stored:?}{remote_a:?}{plan_a:?}");

    assert_eq!(json_a, json_b);
    for canary in [
        "stored-value-canary",
        "stored-secret-canary",
        "desired-value-canary",
        "desired-secret-canary",
        "remote-id-canary",
    ] {
        assert!(!rendered.contains(canary));
        assert!(!debug.contains(canary));
    }
}

#[test]
fn unknown_stored_property_is_rejected_with_redaction_safe_projection_error() {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "future_secret_canary": "value-canary" }),
        false,
    );

    let error = StoredState::try_from_state(&state)
        .expect_err("unknown stored properties must fail closed");

    assert!(matches!(
        error,
        StoredStateError::UnsupportedProperty { .. }
    ));
    assert!(!error.to_string().contains("future_secret_canary"));
    assert!(!format!("{error:?}").contains("future_secret_canary"));
    assert!(!format!("{error:?}").contains("value-canary"));
}

#[test]
fn diagnostic_codes_and_plan_metadata_are_stable() {
    assert_eq!(PlanDiagnosticCode::InstanceMismatch.as_str(), "DOKPLAN001");
    assert_eq!(
        PlanDiagnosticCode::MissingObservation.as_str(),
        "DOKPLAN002"
    );
    assert_eq!(PlanDiagnosticCode::RemoteUnavailable.as_str(), "DOKPLAN003");
    assert_eq!(
        PlanDiagnosticCode::RemoteIdentityMismatch.as_str(),
        "DOKPLAN004"
    );
    assert_eq!(
        PlanDiagnosticCode::UnmanagedAddressCollision.as_str(),
        "DOKPLAN005"
    );
    assert_eq!(PlanDiagnosticCode::ProtectedDelete.as_str(), "DOKPLAN006");
    assert_eq!(
        PlanDiagnosticCode::UnsupportedDirective.as_str(),
        "DOKPLAN007"
    );
    assert_eq!(
        PlanDiagnosticCode::MissingPropertyObservation.as_str(),
        "DOKPLAN008"
    );
    assert_eq!(
        PlanDiagnosticCode::UnknownPropertyObservation.as_str(),
        "DOKPLAN009"
    );
    assert_eq!(
        PlanDiagnosticCode::DesiredDependencyCycle.as_str(),
        "DOKPLAN010"
    );
    assert_eq!(
        PlanDiagnosticCode::StoredDependencyCycle.as_str(),
        "DOKPLAN011"
    );

    let address = address("application.api");
    let instance = instance();
    let state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Replicas,
            OwnedValue::Value(value(json!(1))),
        )])),
    );
    let remote = RemoteState::try_new(instance, [(address, RemoteObservation::Missing)])
        .expect("remote state must be valid");
    let plan = plan(&desired, &stored, &remote);

    assert_eq!(plan.format_version(), 1);
    assert_eq!(plan.lineage(), stored.lineage());
    assert_eq!(plan.state_serial(), stored.serial());
    assert_eq!(plan.config_digest(), desired.digest());
}

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address must be valid")
}

fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).expect("digest must be valid")
}

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://dokploy.example.test").expect("instance must be valid")
}

fn remote_id() -> RemoteId {
    RemoteId::new("remote-1").expect("remote id must be valid")
}

fn value(value: serde_json::Value) -> ComparableValue {
    ComparableValue::try_from_json(value).expect("comparable value must be valid")
}

fn desired_state(address: &ResourceAddress, resource: DesiredResource) -> DesiredState {
    DesiredState::try_new(digest(), BTreeMap::from([(address.clone(), resource)]))
        .expect("desired state must be valid")
}

fn remote_state(
    instance: InstanceIdentity,
    address: &ResourceAddress,
    properties: BTreeMap<PropertyPath, PropertyObservation>,
) -> RemoteState {
    RemoteState::try_new(
        instance,
        [(
            address.clone(),
            RemoteObservation::Present(RemoteResource::new(remote_id(), properties)),
        )],
    )
    .expect("remote observations must be valid")
}

fn replicas_plan(stored_replicas: u64, desired_replicas: u64, remote_replicas: u64) -> Plan {
    let address = address("application.api");
    let instance = instance();
    let state = state_with_resource(
        &address,
        &instance,
        json!({ "replicas": stored_replicas }),
        false,
    );
    let stored = StoredState::try_from_state(&state).expect("state must project");
    let desired = desired_state(
        &address,
        DesiredResource::new(BTreeMap::from([(
            PropertyPath::Replicas,
            OwnedValue::Value(value(json!(desired_replicas))),
        )])),
    );
    let remote = remote_state(
        instance,
        &address,
        BTreeMap::from([(
            PropertyPath::Replicas,
            PropertyObservation::Known(value(json!(remote_replicas))),
        )]),
    );
    plan(&desired, &stored, &remote)
}

fn state_with_resource(
    address: &ResourceAddress,
    instance: &InstanceIdentity,
    previous: serde_json::Value,
    protected: bool,
) -> StateFile {
    state_with_resource_details(
        address,
        instance,
        ResourceKind::Application,
        "remote-1",
        previous,
        protected,
        Vec::new(),
    )
}

fn state_with_resource_details(
    address: &ResourceAddress,
    instance: &InstanceIdentity,
    kind: ResourceKind,
    remote_id: &str,
    previous: serde_json::Value,
    protected: bool,
    dependencies: Vec<ResourceAddress>,
) -> StateFile {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance.clone());
    state
        .upsert_resource(
            address.clone(),
            ResourceState::new(
                kind,
                RemoteId::new(remote_id).expect("remote id must be valid"),
                protected,
                ManagedInputs::try_from_json(previous).expect("managed inputs must be valid"),
                dependencies,
            ),
        )
        .expect("state insert must succeed");
    state
}

fn insert_state_resource(
    state: &mut StateFile,
    address: ResourceAddress,
    remote_id: &str,
    dependencies: Vec<ResourceAddress>,
) {
    state
        .upsert_resource(
            address.clone(),
            ResourceState::new(
                address.kind(),
                RemoteId::new(remote_id).expect("remote id must be valid"),
                false,
                ManagedInputs::try_from_json(json!({})).expect("managed inputs must be valid"),
                dependencies,
            ),
        )
        .expect("state insert must succeed");
}
