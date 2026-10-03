//! What the three snapshots refuse at construction: wrong-kind paths, malformed values,
//! secrets outside their receipts, duplicate physical identities, and bad hierarchy.
//! Also: a planned create materializes exactly the durable resource.

mod support;

use std::collections::BTreeMap;

use dokploy_core::{
    DesiredResource, DesiredState, DesiredStateError, OwnedValue, PropertyObservation,
    RemoteObservation, RemoteResource, RemoteState, RemoteStateError, StoredState,
    StoredStateError, plan,
};
use dokploy_state::RemoteId;
use serde_json::json;
use support::*;

fn desired_of(address: &str, resource: DesiredResource) -> Result<DesiredState, DesiredStateError> {
    DesiredState::try_new(digest(), BTreeMap::from([(addr(address), resource)]))
}

fn project(state_resources: Vec<Have>) -> Result<StoredState, StoredStateError> {
    StoredState::try_from_state(&state(state_resources), specs())
}

#[test]
fn desired_state_rejects_wrong_kind_paths_and_bad_dependencies() {
    let widget_path = BTreeMap::from([(path("widget", "description"), val(json!("x")))]);
    let wrong_kind = desired_of("cache.main", DesiredResource::new(widget_path))
        .expect_err("description is not a cache property");
    assert!(matches!(
        wrong_kind,
        DesiredStateError::InvalidPropertyPath { .. }
    ));

    let this = addr("widget.main");
    let itself = desired_of(
        "widget.main",
        DesiredResource::new(BTreeMap::new()).with_dependencies(vec![this]),
    )
    .expect_err("self dependency");
    assert!(matches!(itself, DesiredStateError::SelfDependency { .. }));

    let missing = desired_of(
        "widget.main",
        DesiredResource::new(BTreeMap::new()).with_dependencies(vec![addr("cache.main")]),
    )
    .expect_err("missing dependency");
    assert!(matches!(
        missing,
        DesiredStateError::MissingDependency { .. }
    ));
}

#[test]
fn desired_values_follow_the_spec_type_and_the_secret_rules() {
    let attempt = |property: &str, value: OwnedValue| {
        try_wanted(vec![want("widget.main").prop(property, value)])
    };
    for (property, value) in [
        ("replicas", val(json!("three"))),
        ("replicas", val(json!(-1))),
        ("flavor", val(json!("huge"))),
        ("name", val(json!(""))),
        // Plain values are never accepted where only a receipt may be owned, and vice versa.
        ("token", val(json!("plaintext"))),
        ("replicas", secret(1)),
        // A non-nullable property cannot be cleared.
        ("token", OwnedValue::Null),
        ("flavor", OwnedValue::Null),
        // A collection root takes null or an empty marker, never a value.
        ("environment", val(json!({"LOG": "1"}))),
    ] {
        let error = attempt(property, value).expect_err(property);
        assert!(
            matches!(error, DesiredStateError::InvalidPropertyValue { .. }),
            "{property}: {error:?}"
        );
    }

    attempt("description", OwnedValue::Null).expect("a nullable property can be cleared");
    attempt("environment", OwnedValue::EmptyCollection)
        .expect("a collection can be declared empty");
}

#[test]
fn a_collection_root_and_one_of_its_entries_cannot_be_owned_together() {
    let error = try_wanted(vec![
        want("widget.main")
            .prop("environment", OwnedValue::EmptyCollection)
            .prop("environment.LOG", secret(1)),
    ])
    .expect_err("conflicting paths");
    assert!(matches!(
        error,
        DesiredStateError::ConflictingPropertyPaths { .. }
    ));
}

#[test]
fn nested_desired_addresses_must_name_their_containment() {
    let nested = || BTreeMap::from([(addr("widget.main"), DesiredResource::new(BTreeMap::new()))]);

    let missing = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            addr("widget.main/gadget.g"),
            DesiredResource::new(BTreeMap::new()),
        )]),
    )
    .expect_err("a nested resource names its parent");
    assert!(matches!(
        missing,
        DesiredStateError::ContainmentAddressMismatch { .. }
    ));

    let unexpected = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            addr("cache.main"),
            DesiredResource::new(BTreeMap::new()).with_containment(Some(addr("widget.main"))),
        )]),
    )
    .expect_err("a top-level resource has no parent");
    assert!(matches!(
        unexpected,
        DesiredStateError::UnexpectedContainment { .. }
    ));

    let wrong_parent = {
        let mut resources = nested();
        resources.insert(
            addr("widget.other/gadget.g"),
            DesiredResource::new(BTreeMap::new()).with_containment(Some(addr("widget.main"))),
        );
        DesiredState::try_new(digest(), resources).expect_err("the parent must be the path's")
    };
    assert!(matches!(
        wrong_parent,
        DesiredStateError::ContainmentAddressMismatch { .. }
    ));

    let valid = wanted(vec![want("widget.main"), want("widget.main/gadget.g")]);
    assert_eq!(
        valid.resources()[&addr("widget.main/gadget.g")].containment(),
        Some(&addr("widget.main"))
    );
}

#[test]
fn stored_and_remote_snapshots_reject_paths_for_the_wrong_kind() {
    let canary = "wrong-kind-canary";
    let stored_error = project(vec![have(
        "cache.main",
        "c-1",
        json!({"description": canary}),
    )])
    .expect_err("a stored path must match the kind");
    assert!(
        matches!(
            stored_error,
            StoredStateError::InvalidPropertyPath { .. }
                | StoredStateError::UnsupportedProperty { .. }
        ),
        "{stored_error:?}"
    );
    assert!(!format!("{stored_error:?}").contains(canary));

    let widget_description = path("widget", "description");
    let remote_error = RemoteState::try_new(
        instance(),
        [(
            addr("cache.main"),
            RemoteObservation::Present(RemoteResource::new(
                RemoteId::new("c-1").unwrap(),
                BTreeMap::from([(widget_description, known(json!(canary)))]),
            )),
        )],
    )
    .expect_err("a remote path must match the kind");
    assert!(matches!(
        remote_error,
        RemoteStateError::InvalidPropertyPath { .. }
    ));
    assert!(!format!("{remote_error:?}").contains(canary));
}

#[test]
fn stored_state_that_the_specs_cannot_place_is_refused_without_echoing_it() {
    let unknown = project(vec![have(
        "widget.main",
        "w-1",
        json!({"future_secret_canary": "value-canary"}),
    )])
    .expect_err("unknown stored properties fail closed");
    assert!(matches!(
        unknown,
        StoredStateError::UnsupportedProperty { .. }
    ));
    let shown = format!("{unknown} {unknown:?}");
    assert!(!shown.contains("future_secret_canary") && !shown.contains("value-canary"));

    for managed in [json!({"replicas": "three"}), json!({"flavor": "huge"})] {
        let error =
            project(vec![have("widget.main", "w-1", managed.clone())]).expect_err("{managed}");
        assert!(
            matches!(error, StoredStateError::InvalidPropertyValue { .. }),
            "{managed}: {error:?}"
        );
    }

    // The state crate refuses a secret-named key in plain managed inputs before we see it.
    for plaintext in [
        json!({"token": "plaintext"}),
        json!({"environment": {"LOG": "1"}}),
    ] {
        assert!(dokploy_state::ManagedInputs::try_from_json(plaintext).is_err());
    }

    let receipt_on_plain = project(vec![
        have("widget.main", "w-1", json!({})).secret("replicas", 1),
    ])
    .expect_err("a receipt for a non-secret property");
    assert!(matches!(
        receipt_on_plain,
        StoredStateError::InvalidPropertyPath { .. }
    ));
    let wrong_kind_receipt = project(vec![
        have("cache.main", "c-1", json!({})).secret("token", 1),
    ])
    .expect_err("a receipt for another kind's property");
    assert!(matches!(
        wrong_kind_receipt,
        StoredStateError::InvalidPropertyPath { .. } | StoredStateError::UnsupportedProperty { .. }
    ));
}

#[test]
fn durable_receipts_project_as_opaque_owned_properties_and_null_clears_stay_explicit() {
    let projected = project(vec![
        have("widget.main", "w-1", json!({"password": null}))
            .secret("token", 3)
            .secret("environment.LOG", 4),
    ])
    .expect("receipts project");
    let _ = projected;

    let plan = plan(
        &wanted(vec![
            want("widget.main")
                .prop("token", secret(3))
                .prop("environment.LOG", secret(4))
                .prop("password", OwnedValue::Null),
        ]),
        &stored(vec![
            have("widget.main", "w-1", json!({"password": null}))
                .secret("token", 3)
                .secret("environment.LOG", 4),
        ]),
        &remote(vec![there(
            "widget.main",
            "w-1",
            vec![
                ("token", unreadable()),
                ("environment.LOG", unreadable()),
                ("password", unreadable()),
            ],
        )]),
    );
    assert!(
        plan.changes().is_empty() && plan.drift().is_empty(),
        "{plan:?}"
    );
}

#[test]
fn remote_observations_obey_sensitivity_value_and_root_rules() {
    let attempt = |property: &str, observation: PropertyObservation| {
        try_remote(vec![there(
            "widget.main",
            "w-1",
            vec![(property, observation)],
        )])
    };

    assert!(matches!(
        attempt("token", known(json!("plaintext"))),
        Err(RemoteStateError::InvalidPropertyObservation { .. })
    ));
    assert!(matches!(
        attempt("replicas", unreadable()),
        Err(RemoteStateError::InvalidPropertyObservation { .. })
    ));
    assert!(matches!(
        attempt("replicas", known(json!("three"))),
        Err(RemoteStateError::InvalidPropertyObservation { .. })
    ));
    assert!(matches!(
        attempt("flavor", PropertyObservation::KnownAbsent),
        Err(RemoteStateError::InvalidPropertyObservation { .. })
    ));
    assert!(matches!(
        attempt("environment", known(json!("not-an-object"))),
        Err(RemoteStateError::InvalidPropertyObservation { .. })
    ));
    // A computed property is readable but never part of an observation set the planner compares.
    assert!(matches!(
        attempt("status", known(json!("running"))),
        Err(RemoteStateError::InvalidPropertyPath { .. })
    ));
    assert!(matches!(
        try_remote(vec![there(
            "widget.main",
            "w-1",
            vec![
                ("environment", known(json!({}))),
                ("environment.LOG", unreadable())
            ],
        )]),
        Err(RemoteStateError::ConflictingPropertyPaths { .. })
    ));
    attempt("token", unreadable()).expect("a secret is observed only as unreadable or absent");
    attempt("description", PropertyObservation::KnownAbsent)
        .expect("a nullable property can be absent");
}

fn state_with_ids(entries: &[(&str, &str)]) -> dokploy_state::StateFile {
    // `upsert_resource` does not police physical identity; the stored projection does.
    let mut file = state(vec![]);
    for (address, id) in entries {
        let have = have(address, id, json!({}));
        let (address, resource) = have.into_state_entry();
        file.upsert_resource(address, resource)
            .expect("the entry is valid");
    }
    file
}

#[test]
fn physical_identity_is_scoped_by_kind() {
    let duplicate = StoredState::try_from_state(
        &state_with_ids(&[("widget.a", "shared-id"), ("widget.b", "shared-id")]),
        specs(),
    )
    .expect_err("one physical widget under two addresses");
    assert!(matches!(
        duplicate,
        StoredStateError::DuplicateRemoteIdentity { .. }
    ));
    assert!(!duplicate.to_string().contains("shared-id"));

    StoredState::try_from_state(
        &state_with_ids(&[("widget.a", "shared-id"), ("cache.a", "shared-id")]),
        specs(),
    )
    .expect("the identity namespace includes the kind");

    let observation = || {
        RemoteObservation::Present(RemoteResource::new(
            RemoteId::new("shared-id").unwrap(),
            BTreeMap::new(),
        ))
    };
    RemoteState::try_new(
        instance(),
        [
            (addr("widget.a"), observation()),
            (addr("cache.a"), observation()),
        ],
    )
    .expect("the same raw id across kinds is valid");
    let error = RemoteState::try_new(
        instance(),
        [
            (addr("widget.a"), observation()),
            (addr("widget.b"), observation()),
        ],
    )
    .expect_err("the same id under two widgets");
    assert!(matches!(
        error,
        RemoteStateError::DuplicateRemoteIdentity { .. }
    ));
    assert!(!error.to_string().contains("shared-id"));
}

#[test]
fn a_planned_create_materializes_the_exact_durable_resource() {
    let address = addr("widget.main");
    let plan = plan(
        &wanted(vec![
            creatable("widget.main")
                .prop("description", val(json!("Managed")))
                .prop("limits.cpu", val(json!("1")))
                .prop("environment.LOG", secret(4))
                .prop("password", OwnedValue::Null),
        ]),
        &nothing_stored(),
        &remote(vec![missing("widget.main")]),
    );
    let checkpoint = plan.changes()[0]
        .checkpoint()
        .present()
        .expect("a create has a checkpoint");

    let resource = checkpoint
        .materialize(&address, RemoteId::new("widget-1").unwrap())
        .expect("the checkpoint materializes");

    assert_eq!(resource.remote_id().as_str(), "widget-1");
    assert_eq!(resource.kind(), address.kind());
    assert_eq!(
        resource.last_applied().as_json(),
        &json!({
            "description": "Managed",
            "flavor": "small",
            "limits": {"cpu": "1"},
            "password": null,
        })
    );
    let sensitive: Vec<_> = resource
        .sensitive_inputs()
        .paths()
        .map(|p| p.as_str().to_owned())
        .collect();
    assert_eq!(sensitive, ["environment.LOG", "token"]);
    assert_eq!(
        resource
            .sensitive_inputs()
            .fingerprint(&dokploy_state::SensitivePropertyPath::parse("token").unwrap()),
        Some(&fingerprint(7))
    );
}

#[test]
fn a_planned_create_without_secrets_materializes_no_receipts() {
    let plan = plan(
        &wanted(vec![want("cache.main").prop("password", OwnedValue::Null)]),
        &nothing_stored(),
        &remote(vec![missing("cache.main")]),
    );
    let resource = plan.changes()[0]
        .checkpoint()
        .present()
        .unwrap()
        .materialize(&addr("cache.main"), RemoteId::new("c-1").unwrap())
        .expect("materializes");
    assert!(resource.sensitive_inputs().is_empty());
    assert_eq!(
        resource.last_applied().as_json(),
        &json!({"password": null})
    );
}
