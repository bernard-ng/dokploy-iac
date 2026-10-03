//! Properties that name another resource by selector: how they are validated, resolved, and
//! planned. `server` accepts `{local: true}` or `{name}`; `mirror` accepts only `{name}`.

mod support;

use dokploy_core::{
    ChangeKind, DesiredStateError, ExternalResolution, ExternalSelectorFailure, MutationContract,
    MutationMode, OwnedValue, PlanDiagnosticCode, PropertyMutation, RemoteFailureKind,
    RemoteObservation, RemoteState, RemoteStateError, ReplacementOrder, StoredStateError, plan,
};
use dokploy_state::RemoteId;
use serde_json::json;
use support::*;

const CANARY_ID: &str = "external-id-canary-8f21";
const CANARY_NAME: &str = "edge-canary-name";

fn named(name: &str) -> OwnedValue {
    val(json!({"name": name}))
}

fn local() -> OwnedValue {
    val(json!({"local": true}))
}

fn remote_id(text: &str) -> RemoteId {
    RemoteId::new(text).expect("remote id is valid")
}

fn key(property: &str) -> (dokploy_state::ResourceAddress, dokploy_core::PropertyPath) {
    (addr("widget.main"), path("widget", property))
}

fn missing_widget() -> RemoteState {
    remote(vec![missing("widget.main")])
}

#[test]
fn selector_paths_are_marked_and_are_neither_secret_nor_lifecycle_only() {
    for property in ["server", "mirror"] {
        let path = path("widget", property);
        assert_eq!(path.as_str(), property);
        assert!(path.is_external_selector());
        assert!(!path.is_sensitive() && !path.is_lifecycle_only());
    }
    assert!(!path("widget", "description").is_external_selector());
}

#[test]
fn desired_selector_values_are_closed_stable_selectors() {
    let attempt = |property: &str, value: OwnedValue| {
        try_wanted(vec![want("widget.main").prop(property, value)])
    };

    attempt("server", named("edge-1")).expect("named server");
    attempt("server", local()).expect("local server");
    attempt("server", OwnedValue::Null).expect("cleared server");
    attempt("mirror", named("main")).expect("named mirror");
    attempt("mirror", OwnedValue::Null).expect("cleared mirror");

    for (property, value) in [
        ("mirror", local()),
        ("server", named("")),
        ("server", named(" padded")),
        ("server", named("line\nbreak")),
        ("server", named(&"x".repeat(257))),
        ("server", val(json!("raw-external-id"))),
        ("server", val(json!({"name": "a", "local": true}))),
        ("server", val(json!({"id": "raw"}))),
        ("server", val(json!({"local": false}))),
        ("mirror", OwnedValue::EmptyCollection),
    ] {
        let error = attempt(property, value).expect_err("an invalid selector fails closed");
        assert!(
            matches!(error, DesiredStateError::InvalidPropertyValue { .. }),
            "{error:?}"
        );
    }
}

#[test]
fn stored_and_remote_selector_values_fail_closed_on_malformed_shapes() {
    for (managed, valid) in [
        (json!({"server": {"local": true}}), true),
        (json!({"server": {"name": "edge"}}), true),
        (json!({"mirror": null}), true),
        (json!({"mirror": {"name": "main"}}), true),
        (json!({"mirror": {"local": true}}), false),
        (json!({"mirror": "raw-id"}), false),
        (json!({"mirror": {"name": ""}}), false),
        (json!({"server": {"id": "raw"}}), false),
    ] {
        let result = dokploy_core::StoredState::try_from_state(
            &state(vec![have("widget.main", "w-1", managed.clone())]),
            specs(),
        );
        match (valid, result) {
            (true, Ok(_)) => {}
            (false, Err(StoredStateError::InvalidPropertyValue { .. })) => {}
            (expected, other) => panic!("{managed}: expected valid={expected}, got {other:?}"),
        }
    }

    for (observation, valid) in [
        (known(json!({"name": "edge"})), true),
        (known(json!({"local": true})), true),
        (known(json!({"local": false})), false),
        (known(json!("raw-id")), false),
    ] {
        let result = try_remote(vec![there(
            "widget.main",
            "w-1",
            vec![("server", observation)],
        )]);
        assert_eq!(result.is_ok(), valid);
    }
}

#[test]
fn resolutions_attach_only_to_an_observed_selector_of_the_right_shape_once() {
    let base = || remote(vec![missing("widget.main"), missing("widget.other")]);

    base()
        .with_external_resolutions([
            (key("server"), ExternalResolution::Local),
            (
                key("mirror"),
                ExternalResolution::Resolved(remote_id(CANARY_ID)),
            ),
        ])
        .expect("valid resolutions");

    for invalid in [
        (key("description"), ExternalResolution::Unmatched),
        (key("mirror"), ExternalResolution::Local),
        (
            (addr("widget.absent"), path("widget", "server")),
            ExternalResolution::Unmatched,
        ),
    ] {
        let error = base()
            .with_external_resolutions([invalid])
            .expect_err("invalid key");
        assert!(matches!(
            error,
            RemoteStateError::InvalidExternalResolution { .. }
        ));
    }

    let duplicate = [
        (key("server"), ExternalResolution::Unmatched),
        (key("server"), ExternalResolution::Ambiguous),
    ];
    assert!(matches!(
        base().with_external_resolutions(duplicate),
        Err(RemoteStateError::InvalidExternalResolution { .. })
    ));
}

#[test]
fn an_unresolved_selector_blocks_the_plan_with_a_value_free_typed_diagnostic() {
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
        let mut observed = missing_widget();
        if let Some(resolution) = resolution {
            observed = observed
                .with_external_resolutions([(key("mirror"), resolution)])
                .expect("resolution is valid");
        }

        let plan = plan(
            &wanted(vec![
                creatable("widget.main").prop("mirror", named(CANARY_NAME)),
            ]),
            &nothing_stored(),
            &observed,
        );

        assert!(!plan.applyable());
        assert!(plan.changes().is_empty());
        assert_eq!(plan.diagnostics().len(), 1);
        let diagnostic = &plan.diagnostics()[0];
        assert_eq!(
            diagnostic.code(),
            PlanDiagnosticCode::UnresolvedExternalSelector
        );
        assert_eq!(diagnostic.address(), Some(&addr("widget.main")));
        assert_eq!(diagnostic.property(), Some(&path("widget", "mirror")));
        assert_eq!(diagnostic.selector_failure(), Some(expected));
        let shown = json_of(&plan);
        assert!(shown.contains("\"selectorFailure\""));
        assert!(!shown.contains(CANARY_NAME) && !shown.contains(CANARY_ID));
    }
}

#[test]
fn cleared_local_and_ignored_selectors_need_no_resolution() {
    let observed = missing_widget()
        .with_external_resolutions([(key("server"), ExternalResolution::Local)])
        .expect("local resolution");

    let plan = plan(
        &wanted(vec![
            creatable("widget.main")
                .prop("server", local())
                .prop("mirror", named("ignored"))
                .ignoring(&["mirror"]),
        ]),
        &nothing_stored(),
        &observed,
    );

    assert!(plan.applyable(), "{:?}", plan.diagnostics());
    assert_eq!(plan.changes().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
}

fn selectors_contract() -> MutationContract {
    MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
        .with_default_property(PropertyMutation::new(
            MutationMode::InPlace,
            MutationMode::InPlace,
        ))
        .with_property(
            path("widget", "server"),
            PropertyMutation::new(MutationMode::Replace, MutationMode::Unsupported),
        )
}

#[test]
fn changing_a_mirror_updates_in_place_and_changing_the_server_replaces() {
    let existing = stored(vec![have(
        "widget.main",
        "w-1",
        json!({"mirror": {"name": "old-mirror"}, "server": {"name": "edge-1"}}),
    )]);
    let build = |mirror: OwnedValue, server: OwnedValue| {
        let observed = remote_with_contract(
            vec![there(
                "widget.main",
                "w-1",
                vec![
                    ("mirror", known(json!({"name": "old-mirror"}))),
                    ("server", known(json!({"name": "edge-1"}))),
                ],
            )],
            selectors_contract(),
        )
        .with_external_resolutions([
            (
                key("mirror"),
                ExternalResolution::Resolved(remote_id("mirror-new")),
            ),
            (
                key("server"),
                ExternalResolution::Resolved(remote_id("server-new")),
            ),
        ])
        .expect("resolutions");
        plan(
            &wanted(vec![
                want("widget.main")
                    .prop("mirror", mirror)
                    .prop("server", server),
            ]),
            &existing,
            &observed,
        )
    };

    let in_place = build(named("new-mirror"), named("edge-1"));
    assert!(in_place.applyable(), "{:?}", in_place.diagnostics());
    assert_eq!(in_place.changes()[0].kind(), ChangeKind::Update);

    assert_eq!(
        build(OwnedValue::Null, named("edge-1")).changes()[0].kind(),
        ChangeKind::Update
    );

    let replaced = build(named("old-mirror"), named("edge-2"));
    assert!(replaced.applyable(), "{:?}", replaced.diagnostics());
    assert_eq!(replaced.changes()[0].kind(), ChangeKind::Replace);
    assert_eq!(
        replaced.changes()[0].replacement_order(),
        Some(ReplacementOrder::DeleteBeforeCreate)
    );

    assert_eq!(
        build(named("old-mirror"), local()).changes()[0].kind(),
        ChangeKind::Replace
    );
}

#[test]
fn a_dropped_association_is_drift_found_through_the_selector() {
    let observed = remote_with_contract(
        vec![there(
            "widget.main",
            "w-1",
            vec![("mirror", dokploy_core::PropertyObservation::KnownAbsent)],
        )],
        selectors_contract(),
    )
    .with_external_resolutions([(
        key("mirror"),
        ExternalResolution::Resolved(remote_id("mirror-main")),
    )])
    .expect("resolution");

    let plan = plan(
        &wanted(vec![want("widget.main").prop("mirror", named("main"))]),
        &stored(vec![have(
            "widget.main",
            "w-1",
            json!({"mirror": {"name": "main"}}),
        )]),
        &observed,
    );

    assert_eq!(plan.drift().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
}

#[test]
fn the_binding_receipt_binds_resolved_external_identities() {
    let secret_key = [7_u8; 32];
    let with = |resolution: ExternalResolution| {
        missing_widget()
            .with_external_resolutions([(key("mirror"), resolution)])
            .expect("resolution")
    };

    let unresolved = missing_widget().binding_receipt(&secret_key);
    assert_eq!(
        unresolved,
        missing_widget()
            .with_external_resolutions(Vec::new())
            .expect("empty resolutions")
            .binding_receipt(&secret_key)
    );

    let resolved =
        with(ExternalResolution::Resolved(remote_id("mirror-1"))).binding_receipt(&secret_key);
    assert_eq!(
        resolved,
        with(ExternalResolution::Resolved(remote_id("mirror-1"))).binding_receipt(&secret_key)
    );
    for different in [
        with(ExternalResolution::Resolved(remote_id("mirror-2"))),
        with(ExternalResolution::Unmatched),
        with(ExternalResolution::Ambiguous),
        with(ExternalResolution::Unavailable(
            RemoteFailureKind::Unavailable,
        )),
        with(ExternalResolution::Unavailable(
            RemoteFailureKind::Unauthorized,
        )),
        missing_widget(),
    ] {
        assert_ne!(resolved, different.binding_receipt(&secret_key));
    }
    assert_ne!(
        resolved,
        with(ExternalResolution::Resolved(remote_id("mirror-1"))).binding_receipt(&[8; 32])
    );

    let other_property = missing_widget()
        .with_external_resolutions([(
            key("server"),
            ExternalResolution::Resolved(remote_id("mirror-1")),
        )])
        .expect("resolution")
        .binding_receipt(&secret_key);
    assert_ne!(resolved, other_property, "the selector property is bound");
}

#[test]
fn the_binding_receipt_binds_the_key_identity_and_values_of_observations() {
    let snapshot = |id: &str, description: &str| {
        remote(vec![there(
            "widget.main",
            id,
            vec![("description", known(json!(description)))],
        )])
    };
    let secret_key = [7_u8; 32];
    let receipt = snapshot("w-1", "stable").binding_receipt(&secret_key);

    assert_eq!(
        receipt,
        snapshot("w-1", "stable").binding_receipt(&secret_key)
    );
    assert_ne!(receipt, snapshot("w-1", "stable").binding_receipt(&[8; 32]));
    assert_ne!(
        receipt,
        snapshot("w-2", "stable").binding_receipt(&secret_key)
    );
    assert_ne!(
        receipt,
        snapshot("w-1", "changed").binding_receipt(&secret_key)
    );
}

#[test]
fn resolutions_are_redacted_in_debug_output() {
    let observed = missing_widget()
        .with_external_resolutions([(
            key("mirror"),
            ExternalResolution::Resolved(remote_id(CANARY_ID)),
        )])
        .expect("resolution");

    let debug = format!(
        "{observed:?} {:?}",
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

#[allow(dead_code)]
fn unused(_: RemoteObservation) {}
