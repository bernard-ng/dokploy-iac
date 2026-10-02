//! Planning for settings-scope tags and a project's tag association.

use std::collections::BTreeMap;

use dokploy_core::{
    ChangeKind, ComparableValue, ConfigDigest, DesiredResource, DesiredState, MutationContract,
    MutationMode, OwnedValue, PropertyMutation, PropertyObservation, PropertyPath,
    RemoteObservation, RemoteResource, RemoteState, ReplacementOrder, StoredState, plan,
};
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    StateFile, StateScope,
};
use semver::Version;
use serde_json::json;

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
}

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://dokploy.example.test").expect("instance is valid")
}

fn value(value: serde_json::Value) -> ComparableValue {
    ComparableValue::try_from_json(value).expect("value is comparable")
}

fn text(content: &str) -> OwnedValue {
    OwnedValue::Value(value(json!(content)))
}

/// A tag: name is required and in place; a color can be set but never cleared.
fn tag_contract() -> MutationContract {
    let in_place = PropertyMutation::new(MutationMode::InPlace, MutationMode::InPlace);
    let set_only = PropertyMutation::new(MutationMode::InPlace, MutationMode::Unsupported);
    MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
        .requiring(PropertyPath::Name)
        .allowing_on_create(PropertyPath::Color)
        .with_property(PropertyPath::Name, in_place)
        .with_property(PropertyPath::Color, set_only)
}

fn desired(resources: Vec<(&str, DesiredResource)>) -> DesiredState {
    DesiredState::try_new(
        ConfigDigest::parse("a".repeat(64)).expect("digest is valid"),
        resources
            .into_iter()
            .map(|(name, resource)| (address(name), resource))
            .collect(),
    )
    .expect("desired state is valid")
}

fn tag(properties: BTreeMap<PropertyPath, OwnedValue>) -> DesiredResource {
    DesiredResource::new(properties)
}

fn settings_state(tags: &[(&str, &str, serde_json::Value)]) -> StateFile {
    let mut state =
        StateFile::new_in_scope(Version::new(0, 1, 0), instance(), StateScope::Settings);
    for (name, id, managed) in tags {
        state
            .upsert_resource(
                address(name),
                ResourceState::new(
                    ResourceKind::Tag,
                    RemoteId::new(*id).expect("remote id is valid"),
                    false,
                    ManagedInputs::try_from_json(managed.clone()).expect("inputs are valid"),
                    None,
                    Vec::new(),
                ),
            )
            .expect("tag state is valid");
    }
    state
}

fn present(id: &str, properties: Vec<(PropertyPath, PropertyObservation)>) -> RemoteObservation {
    RemoteObservation::Present(RemoteResource::new(
        RemoteId::new(id).expect("remote id is valid"),
        properties.into_iter().collect(),
    ))
}

fn known(content: serde_json::Value) -> PropertyObservation {
    PropertyObservation::Known(value(content))
}

fn remote(observations: Vec<(&str, RemoteObservation)>) -> RemoteState {
    let observations = observations
        .into_iter()
        .map(|(name, observation)| (address(name), observation))
        .collect::<Vec<_>>();
    let contracts = observations
        .iter()
        .map(|(address, _)| (address.clone(), tag_contract()))
        .collect::<Vec<_>>();
    RemoteState::try_new_with_contracts(instance(), observations, contracts)
        .expect("remote snapshot is valid")
}

#[test]
fn a_missing_tag_is_created_with_its_name_and_color() {
    let desired = desired(vec![(
        "tag.prod",
        tag(BTreeMap::from([
            (PropertyPath::Name, text("Production")),
            (PropertyPath::Color, text("#e11d48")),
        ])),
    )]);
    let stored = StoredState::absent(instance());
    let remote = remote(vec![("tag.prod", RemoteObservation::Missing)]);

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
    assert_eq!(plan.changes()[0].address(), &address("tag.prod"));
}

#[test]
fn a_tag_without_a_name_cannot_be_created() {
    let desired = desired(vec![(
        "tag.prod",
        tag(BTreeMap::from([(PropertyPath::Color, text("#e11d48"))])),
    )]);
    let remote = remote(vec![("tag.prod", RemoteObservation::Missing)]);

    let plan = plan(&desired, &StoredState::absent(instance()), &remote);

    assert!(!plan.applyable(), "{plan:?}");
}

#[test]
fn a_tag_that_matches_its_remote_record_plans_nothing() {
    let properties = BTreeMap::from([
        (PropertyPath::Name, text("Production")),
        (PropertyPath::Color, text("#e11d48")),
    ]);
    let desired = desired(vec![("tag.prod", tag(properties))]);
    let state = settings_state(&[(
        "tag.prod",
        "tag-1",
        json!({"name": "Production", "color": "#e11d48"}),
    )]);
    let remote = remote(vec![(
        "tag.prod",
        present(
            "tag-1",
            vec![
                (PropertyPath::Name, known(json!("Production"))),
                (PropertyPath::Color, known(json!("#e11d48"))),
            ],
        ),
    )]);

    let plan = plan(
        &desired,
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(plan.applyable() && plan.complete(), "{plan:?}");
    assert!(plan.changes().is_empty(), "{plan:?}");
}

#[test]
fn a_changed_color_or_name_updates_in_place() {
    let desired = desired(vec![(
        "tag.prod",
        tag(BTreeMap::from([
            (PropertyPath::Name, text("Production")),
            (PropertyPath::Color, text("#000000")),
        ])),
    )]);
    let state = settings_state(&[(
        "tag.prod",
        "tag-1",
        json!({"name": "Production", "color": "#e11d48"}),
    )]);
    let remote = remote(vec![(
        "tag.prod",
        present(
            "tag-1",
            vec![
                (PropertyPath::Name, known(json!("Production"))),
                (PropertyPath::Color, known(json!("#e11d48"))),
            ],
        ),
    )]);

    let plan = plan(
        &desired,
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
}

#[test]
fn a_color_cannot_be_cleared() {
    // No explicit-null update is proven, so a cleared color is not even a valid
    // desired value; configuration rejects `color: null` for the same reason.
    let error = DesiredState::try_new(
        ConfigDigest::parse("a".repeat(64)).expect("digest is valid"),
        BTreeMap::from([(
            address("tag.prod"),
            tag(BTreeMap::from([
                (PropertyPath::Name, text("Production")),
                (PropertyPath::Color, OwnedValue::Null),
            ])),
        )]),
    )
    .expect_err("a null color is invalid");

    assert!(matches!(
        error,
        dokploy_core::DesiredStateError::InvalidPropertyValue { .. }
    ));
}

#[test]
fn a_tag_removed_from_the_document_is_deleted() {
    let desired = desired(Vec::new());
    let state = settings_state(&[("tag.gone", "tag-9", json!({"name": "Gone"}))]);
    let remote = remote(vec![(
        "tag.gone",
        present("tag-9", vec![(PropertyPath::Name, known(json!("Gone")))]),
    )]);

    let plan = plan(
        &desired,
        &StoredState::try_from_state(&state).unwrap(),
        &remote,
    );

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Delete);
}

#[test]
fn tag_paths_are_only_valid_for_their_own_kinds() {
    let project_state = {
        let mut state = StateFile::new(Version::new(0, 1, 0), instance());
        state
            .upsert_resource(
                address("project.platform"),
                ResourceState::new(
                    ResourceKind::Project,
                    RemoteId::new("project-1").unwrap(),
                    false,
                    // `color` belongs to tags, never to a project.
                    ManagedInputs::try_from_json(json!({"color": "#fff"})).unwrap(),
                    None,
                    Vec::new(),
                ),
            )
            .unwrap();
        state
    };

    assert!(StoredState::try_from_state(&project_state).is_err());

    let tag_with_tags = settings_state(&[("tag.prod", "tag-1", json!({"tags": ["a"]}))]);
    assert!(StoredState::try_from_state(&tag_with_tags).is_err());
}

#[test]
fn a_project_tag_list_must_be_canonical() {
    let project = |tags: serde_json::Value| {
        let mut state = StateFile::new(Version::new(0, 1, 0), instance());
        state
            .upsert_resource(
                address("project.platform"),
                ResourceState::new(
                    ResourceKind::Project,
                    RemoteId::new("project-1").unwrap(),
                    false,
                    ManagedInputs::try_from_json(json!({ "tags": tags })).unwrap(),
                    None,
                    Vec::new(),
                ),
            )
            .unwrap();
        StoredState::try_from_state(&state)
    };

    assert!(
        project(json!([])).is_ok(),
        "an empty list is the explicit no-tags intent"
    );
    assert!(project(json!(["a", "b"])).is_ok());
    assert!(project(json!(["b", "a"])).is_err(), "names are sorted");
    assert!(project(json!(["a", "a"])).is_err(), "names are unique");
    assert!(project(json!([1])).is_err(), "names are strings");
    assert!(project(json!([""])).is_err(), "names are non-empty");
}
