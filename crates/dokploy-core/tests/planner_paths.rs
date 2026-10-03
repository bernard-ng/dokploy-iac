//! Property paths: they belong to the kind whose spec made them legal, compare by kind and
//! dotted path, and a spec's own shape decides what may be ignored or replaced.

mod support;

use std::collections::BTreeMap;

use dokploy_core::{DesiredResource, DesiredState, DesiredStateError, OwnedValue, PropertyPath};
use serde_json::json;
use support::*;

#[test]
fn a_path_belongs_to_the_kind_whose_spec_made_it_legal() {
    assert!(PropertyPath::from_spec(specs().get("widget").unwrap(), "missing").is_err());
    assert!(PropertyPath::from_spec(specs().get("cache").unwrap(), "description").is_err());

    let widget_description = BTreeMap::from([(path("widget", "description"), val(json!("x")))]);
    let on_cache = DesiredState::try_new(
        digest(),
        BTreeMap::from([(addr("cache.main"), DesiredResource::new(widget_description))]),
    );
    assert!(matches!(
        on_cache,
        Err(DesiredStateError::InvalidPropertyPath { .. })
    ));
}

#[test]
fn paths_compare_by_kind_and_dotted_path() {
    assert_eq!(path("widget", "description"), path("widget", "description"));
    assert_ne!(path("widget", "description"), path("widget", "replicas"));
    assert_ne!(
        path("gadget", "name"),
        path("widget", "name"),
        "the same path in two kinds is two properties"
    );
    assert_eq!(
        path("widget", "environment.LOG").to_string(),
        "environment.LOG"
    );
    assert_eq!(path("widget", "limits.cpu").as_str(), "limits.cpu");
    assert!(path("widget", "environment").is_collection_root());
    assert!(path("widget", "environment.LOG").is_entry_of(&path("widget", "environment")));
    assert!(!path("widget", "description").is_collection_root());
    assert!(path("widget", "token").is_sensitive());
    assert!(
        path("widget", "environment.LOG").is_sensitive(),
        "env entries are secret by default"
    );
    assert!(!path("widget", "labels.team").is_sensitive());
}

#[test]
fn a_collection_root_or_a_secret_entry_cannot_be_ignored() {
    let ignoring = |ignored: &str| {
        try_wanted(vec![
            want("widget.main")
                .prop("replicas", val(json!(1)))
                .ignoring(&[ignored]),
        ])
    };
    for ignored in [
        "environment",
        "labels",
        "environment.LOG",
        "token",
        "password",
    ] {
        assert!(
            matches!(
                ignoring(ignored),
                Err(DesiredStateError::InvalidIgnoredProperty { .. })
            ),
            "{ignored}"
        );
    }
    ignoring("description").expect("an ordinary property can be ignored");
    ignoring("labels.team").expect("so can an entry of a plain collection");
}

#[test]
fn entries_of_an_owned_plain_collection_cannot_be_ignored_or_replaced() {
    // Owning the root and one of its entries is contradictory.
    let both = try_wanted(vec![
        want("widget.main")
            .prop("labels", OwnedValue::EmptyCollection)
            .prop("labels.team", val(json!("a"))),
    ]);
    assert!(matches!(
        both,
        Err(DesiredStateError::ConflictingPropertyPaths { .. })
    ));

    // Owning one entry is fine, and so is ignoring a sibling.
    try_wanted(vec![
        want("widget.main")
            .prop("labels.team", val(json!("a")))
            .ignoring(&["labels.tier"]),
    ])
    .expect("a sibling entry can be ignored");

    // The root is owned, so an entry cannot also be ignored.
    let conflict = try_wanted(vec![
        want("widget.main")
            .prop("labels", OwnedValue::EmptyCollection)
            .ignoring(&["labels.team"]),
    ]);
    assert!(matches!(
        conflict,
        Err(DesiredStateError::ConflictingLifecyclePaths { .. })
    ));

    // An entry cannot be both ignored and replace-on-change.
    let overlap = try_wanted(vec![
        want("widget.main")
            .ignoring(&["labels.team"])
            .replacing_on(&["labels.team"]),
    ]);
    assert!(matches!(
        overlap,
        Err(DesiredStateError::ConflictingLifecyclePaths { .. })
    ));
}
