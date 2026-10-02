//! The planner driven by property paths from the repository's kind specs (ADR 0004).
//!
//! Nothing here names a kind in the planner: every path, mutation rule, and value
//! check comes from `specs/`. The closed vocabulary is not used.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use dokploy_core::{
    ChangeKind, ComparableValue, ConfigDigest, DesiredResource, DesiredState, DesiredStateError,
    ExternalResolution, MutationContract, OwnedValue, PlanDiagnosticCode, PropertyObservation,
    PropertyPath, RemoteObservation, RemoteResource, RemoteState, SensitiveIntent, StoredState,
    StoredStateError, plan,
};
use dokploy_spec::{SpecRegistry, load_dir};
use dokploy_state::{
    FingerprintKeyId, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath, StateFile,
};
use semver::Version;
use serde_json::json;
use uuid::Uuid;

fn specs() -> &'static SpecRegistry {
    static SPECS: OnceLock<SpecRegistry> = OnceLock::new();
    SPECS.get_or_init(|| {
        load_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs"))
            .expect("repository specs are valid")
    })
}

fn path(kind: &str, property: &str) -> PropertyPath {
    PropertyPath::from_spec(specs().get(kind).expect("kind has a spec"), property)
        .unwrap_or_else(|error| panic!("{kind}.{property}: {error}"))
}

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
}

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://dokploy.example.test").expect("instance is valid")
}

fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).expect("digest is valid")
}

fn value(content: serde_json::Value) -> OwnedValue {
    OwnedValue::Value(ComparableValue::try_from_json(content).expect("value is comparable"))
}

fn fingerprint(byte: u8) -> SensitiveFingerprint {
    SensitiveFingerprint::new_v1(
        FingerprintKeyId::new(
            Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea6").expect("key id is valid"),
        )
        .expect("key id is non-nil"),
        [byte; 32],
    )
}

fn secret(byte: u8) -> OwnedValue {
    OwnedValue::Sensitive(SensitiveIntent::from_fingerprint(fingerprint(byte)))
}

fn known(content: serde_json::Value) -> PropertyObservation {
    PropertyObservation::Known(ComparableValue::try_from_json(content).expect("comparable"))
}

fn desired_one(name: &str, resource: DesiredResource) -> Result<DesiredState, DesiredStateError> {
    DesiredState::try_new(digest(), BTreeMap::from([(address(name), resource)]))
}

fn project(properties: Vec<(&str, OwnedValue)>) -> DesiredResource {
    DesiredResource::new(
        properties
            .into_iter()
            .map(|(property, value)| (path("project", property), value))
            .collect(),
    )
}

fn application(properties: Vec<(&str, OwnedValue)>) -> DesiredResource {
    DesiredResource::new(
        properties
            .into_iter()
            .map(|(property, value)| (path("application", property), value))
            .collect(),
    )
    .with_containment(Some(address("environment.production")))
}

fn present(
    id: &str,
    kind: &str,
    properties: Vec<(&str, PropertyObservation)>,
) -> RemoteObservation {
    RemoteObservation::Present(RemoteResource::new(
        RemoteId::new(id).expect("remote id is valid"),
        properties
            .into_iter()
            .map(|(property, observation)| (path(kind, property), observation))
            .collect(),
    ))
}

fn remote(kind: &str, name: &str, observation: RemoteObservation) -> RemoteState {
    RemoteState::try_new_with_contracts(
        instance(),
        [(address(name), observation)],
        [(
            address(name),
            MutationContract::from_spec(specs().get(kind).expect("kind has a spec")),
        )],
    )
    .expect("remote snapshot is valid")
}

fn stored(kind: ResourceKind, name: &str, id: &str, managed: serde_json::Value) -> StoredState {
    stored_with(kind, name, id, managed, SensitiveInputs::default())
}

fn stored_with(
    kind: ResourceKind,
    name: &str,
    id: &str,
    managed: serde_json::Value,
    sensitive: SensitiveInputs,
) -> StoredState {
    StoredState::try_from_state_with_specs(&state_with(kind, name, id, managed, sensitive), specs())
        .expect("stored state projects")
}

fn state_with(
    kind: ResourceKind,
    name: &str,
    id: &str,
    managed: serde_json::Value,
    sensitive: SensitiveInputs,
) -> StateFile {
    let mut state = StateFile::new_in_scope(Version::new(0, 1, 0), instance(), kind.scope());
    let containment = kind
        .containment_parent_kind()
        .map(|_| address("environment.production"));
    state
        .upsert_resource(
            address(name),
            ResourceState::try_new(
                kind,
                RemoteId::new(id).expect("remote id is valid"),
                false,
                ManagedInputs::try_from_json(managed).expect("inputs are valid"),
                sensitive,
                containment,
                Vec::new(),
            )
            .expect("resource state is valid"),
        )
        .expect("state accepts the resource");
    state
}

#[test]
fn a_new_project_is_created_from_spec_paths() {
    let desired = desired_one(
        "project.platform",
        project(vec![("description", value(json!("Managed")))]),
    )
    .expect("desired state is valid");
    let remote = remote("project", "project.platform", RemoteObservation::Missing);

    let plan = plan(&desired, &StoredState::absent(instance()), &remote);

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes().len(), 1);
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
    let paths: Vec<String> = plan.changes()[0]
        .fields()
        .iter()
        .map(|field| field.key().to_string())
        .collect();
    assert_eq!(paths, ["description"]);
}

#[test]
fn a_property_the_spec_requires_blocks_creation() {
    // `replicas` is a non-nullable application field with no default.
    let desired = desired_one(
        "application.api",
        application(vec![("description", value(json!("api")))]),
    )
    .expect("desired state is valid");
    let remote = remote("application", "application.api", RemoteObservation::Missing);

    let plan = plan(&desired, &StoredState::absent(instance()), &remote);

    assert!(!plan.applyable());
    assert!(plan.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == PlanDiagnosticCode::MissingCreateProperty
            && diagnostic.property().map(ToString::to_string).as_deref() == Some("replicas")
    }));
}

#[test]
fn matching_stored_and_remote_state_plans_nothing() {
    let desired = desired_one(
        "project.platform",
        project(vec![("description", value(json!("Managed")))]),
    )
    .unwrap();
    let stored = stored(
        ResourceKind::Project,
        "project.platform",
        "project-1",
        json!({"description": "Managed"}),
    );
    let remote = remote(
        "project",
        "project.platform",
        present(
            "project-1",
            "project",
            vec![("description", known(json!("Managed")))],
        ),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.applyable(), "{plan:?}");
    assert!(plan.changes().is_empty(), "{plan:?}");
}

#[test]
fn a_changed_in_place_property_plans_one_update() {
    let desired = desired_one(
        "project.platform",
        project(vec![("description", value(json!("Renamed")))]),
    )
    .unwrap();
    let stored = stored(
        ResourceKind::Project,
        "project.platform",
        "project-1",
        json!({"description": "Managed"}),
    );
    let remote = remote(
        "project",
        "project.platform",
        present(
            "project-1",
            "project",
            vec![("description", known(json!("Managed")))],
        ),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
}

#[test]
fn only_a_nullable_property_can_be_cleared() {
    let stored = stored(
        ResourceKind::Project,
        "project.platform",
        "project-1",
        json!({"description": "Managed"}),
    );
    let observed = present(
        "project-1",
        "project",
        vec![("description", known(json!("Managed")))],
    );

    // `description` is nullable: clearing is an in-place update.
    let clear = desired_one(
        "project.platform",
        project(vec![("description", OwnedValue::Null)]),
    )
    .unwrap();
    let plan_clear = plan(
        &clear,
        &stored,
        &remote("project", "project.platform", observed),
    );
    assert_eq!(
        plan_clear.changes()[0].kind(),
        ChangeKind::Update,
        "{plan_clear:?}"
    );

    // `replicas` is not: the spec makes the clear unsupported, and `Null` is not even a
    // valid desired value for it.
    let refused = desired_one(
        "application.api",
        application(vec![("replicas", OwnedValue::Null)]),
    );
    assert!(matches!(
        refused,
        Err(DesiredStateError::InvalidPropertyValue { .. })
    ));
}

#[test]
fn changing_a_create_only_property_plans_a_replacement() {
    let desired = desired_one(
        "application.api",
        application(vec![
            ("replicas", value(json!(1))),
            ("app_name", value(json!("api-v2"))),
        ]),
    )
    .unwrap();
    let stored = stored(
        ResourceKind::Application,
        "application.api",
        "application-1",
        json!({"replicas": 1, "app_name": "api"}),
    );
    let remote = remote(
        "application",
        "application.api",
        present(
            "application-1",
            "application",
            vec![
                ("replicas", known(json!(1))),
                ("app_name", known(json!("api"))),
            ],
        ),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Replace, "{plan:?}");
}

#[test]
fn environment_entries_are_planned_per_key_and_never_show_values() {
    let desired = desired_one(
        "application.api",
        application(vec![
            ("replicas", value(json!(1))),
            ("environment.LOG_LEVEL", secret(2)),
            ("environment.REGION", secret(3)),
        ]),
    )
    .unwrap();

    let sensitive = SensitiveInputs::try_from_entries([
        (
            SensitivePropertyPath::parse("environment.LOG_LEVEL").unwrap(),
            fingerprint(2),
        ),
        (
            SensitivePropertyPath::parse("environment.REGION").unwrap(),
            fingerprint(9),
        ),
    ])
    .expect("receipts are unique");
    let stored = stored_with(
        ResourceKind::Application,
        "application.api",
        "application-1",
        json!({"replicas": 1}),
        sensitive,
    );
    let remote = remote(
        "application",
        "application.api",
        present(
            "application-1",
            "application",
            vec![
                ("replicas", known(json!(1))),
                (
                    "environment.LOG_LEVEL",
                    PropertyObservation::Unknown(dokploy_core::PropertyUnknownReason::Sensitive),
                ),
                (
                    "environment.REGION",
                    PropertyObservation::Unknown(dokploy_core::PropertyUnknownReason::Sensitive),
                ),
            ],
        ),
    );

    let plan = plan(&desired, &stored, &remote);

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes().len(), 1, "{plan:?}");
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Update);
    let changed: Vec<String> = plan.changes()[0]
        .fields()
        .iter()
        .map(|field| field.key().to_string())
        .collect();
    assert_eq!(
        changed,
        ["environment.REGION"],
        "only the variable whose receipt changed is named"
    );
    assert!(!format!("{plan:?}").contains("LOG_LEVEL"));
}

#[test]
fn an_environment_root_and_one_of_its_entries_cannot_be_owned_together() {
    let result = desired_one(
        "application.api",
        application(vec![
            ("replicas", value(json!(1))),
            ("environment", OwnedValue::EmptyCollection),
            ("environment.LOG_LEVEL", secret(2)),
        ]),
    );
    assert!(matches!(
        result,
        Err(DesiredStateError::ConflictingPropertyPaths { .. })
    ));

    // The root alone may be cleared or declared empty, and nothing else.
    for owned in [OwnedValue::Null, OwnedValue::EmptyCollection] {
        desired_one(
            "application.api",
            application(vec![("replicas", value(json!(1))), ("environment", owned)]),
        )
        .expect("a bare root is valid");
    }
    assert!(matches!(
        desired_one(
            "application.api",
            application(vec![("environment", value(json!({"A": "b"})))])
        ),
        Err(DesiredStateError::InvalidPropertyValue { .. })
    ));
}

#[test]
fn values_are_checked_against_the_spec_type() {
    let invalid = |properties: Vec<(&str, OwnedValue)>| {
        matches!(
            desired_one("application.api", application(properties)),
            Err(DesiredStateError::InvalidPropertyValue { .. })
        )
    };
    assert!(invalid(vec![("replicas", value(json!(-1)))]), "below min");
    assert!(
        invalid(vec![("replicas", value(json!("one")))]),
        "wrong type"
    );
    assert!(
        invalid(vec![("replicas", value(json!(1.5)))]),
        "not an integer"
    );
    assert!(invalid(vec![("description", value(json!(7)))]), "not text");
    assert!(
        invalid(vec![("args", value(json!(["a", 2])))]),
        "list item type"
    );
    assert!(
        invalid(vec![("source", value(json!({"image": "nginx"})))]),
        "a union value needs its tag"
    );
    assert!(
        invalid(vec![("environment.LOG_LEVEL", value(json!("plain")))]),
        "environment values are owned through receipts only"
    );

    desired_one(
        "application.api",
        application(vec![
            ("replicas", value(json!(0))),
            ("args", value(json!(["--serve", "--quiet"]))),
            ("source", value(json!({"type": "docker", "image": "nginx"}))),
        ]),
    )
    .expect("well-typed values are accepted");

    // `min_len` applies to text.
    assert!(matches!(
        desired_one(
            "project.platform",
            project(vec![("name", value(json!("")))])
        ),
        Err(DesiredStateError::InvalidPropertyValue { .. })
    ));
}

#[test]
fn a_path_belongs_to_the_kind_whose_spec_made_it_legal() {
    // `regex` is a redirect property; it cannot be owned by a project.
    let result = desired_one(
        "project.platform",
        DesiredResource::new(BTreeMap::from([(
            path("redirect", "regex"),
            value(json!("^/old")),
        )])),
    );
    assert!(matches!(
        result,
        Err(DesiredStateError::InvalidPropertyPath { .. })
    ));

    assert!(PropertyPath::from_spec(specs().get("redirect").unwrap(), "missing").is_err());
}

#[test]
fn a_collection_root_or_a_secret_entry_cannot_be_ignored() {
    let ignoring = |ignored: &str| {
        desired_one(
            "application.api",
            application(vec![("replicas", value(json!(1)))])
                .with_ignored_changes(vec![path("application", ignored)]),
        )
    };
    assert!(matches!(
        ignoring("environment"),
        Err(DesiredStateError::InvalidIgnoredProperty { .. })
    ));
    // An environment entry is secret, and a secret cannot be ignored.
    assert!(matches!(
        ignoring("environment.LOG_LEVEL"),
        Err(DesiredStateError::InvalidIgnoredProperty { .. })
    ));
    ignoring("description").expect("an ordinary property can be ignored");
}

/// An `application` spec with a map of plain labels, so a non-secret collection can be tested.
fn labelled_application() -> dokploy_spec::KindSpec {
    dokploy_spec::parse_spec(
        r#"
kind: application
scope: project
parent: environment
section: applications
title: Application
identity: { key: name, collision: [name], address: "application.{key}" }
api:
  id: applicationId
  create: { op: application.create }
fields:
  name: { type: text, default: key }
  labels: { type: "map<text, text>", granularity: key }
"#,
    )
    .expect("the test spec parses")
}

#[test]
fn entries_of_an_owned_plain_collection_cannot_be_ignored_or_replaced() {
    let spec = labelled_application();
    let labels = PropertyPath::from_spec(&spec, "labels").unwrap();
    let entry = PropertyPath::from_spec(&spec, "labels.team").unwrap();
    let other = PropertyPath::from_spec(&spec, "labels.tier").unwrap();

    // Owning the root and one of its entries is contradictory.
    let both = desired_one(
        "application.api",
        DesiredResource::new(BTreeMap::from([
            (labels.clone(), OwnedValue::EmptyCollection),
            (entry.clone(), value(json!("a"))),
        ]))
        .with_containment(Some(address("environment.production"))),
    );
    assert!(matches!(
        both,
        Err(DesiredStateError::ConflictingPropertyPaths { .. })
    ));

    // Entries are plain here, so owning one is fine, and so is ignoring a sibling.
    desired_one(
        "application.api",
        DesiredResource::new(BTreeMap::from([(entry.clone(), value(json!("a")))]))
            .with_containment(Some(address("environment.production")))
            .with_ignored_changes(vec![other.clone()]),
    )
    .expect("a sibling entry can be ignored");

    // The root is owned, so an entry cannot also be ignored.
    let conflict = desired_one(
        "application.api",
        DesiredResource::new(BTreeMap::from([(
            labels.clone(),
            OwnedValue::EmptyCollection,
        )]))
        .with_containment(Some(address("environment.production")))
        .with_ignored_changes(vec![entry.clone()]),
    );
    assert!(matches!(
        conflict,
        Err(DesiredStateError::ConflictingLifecyclePaths { .. })
    ));

    // An entry cannot be both ignored and replace-on-change.
    let overlap = desired_one(
        "application.api",
        DesiredResource::new(BTreeMap::new())
            .with_containment(Some(address("environment.production")))
            .with_ignored_changes(vec![entry.clone()])
            .with_replacement_changes(vec![entry]),
    );
    assert!(matches!(
        overlap,
        Err(DesiredStateError::ConflictingLifecyclePaths { .. })
    ));
}

#[test]
fn a_selector_property_needs_a_fresh_unique_resolution() {
    let desired = desired_one(
        "application.api",
        application(vec![
            ("replicas", value(json!(1))),
            ("server", value(json!({"name": "edge-1"}))),
        ]),
    )
    .unwrap();
    let stored = stored(
        ResourceKind::Application,
        "application.api",
        "application-1",
        json!({"replicas": 1, "server": {"name": "edge-1"}}),
    );
    let observation = || {
        present(
            "application-1",
            "application",
            vec![
                ("replicas", known(json!(1))),
                ("server", known(json!({"name": "edge-1"}))),
            ],
        )
    };

    let unresolved = remote("application", "application.api", observation());
    let blocked = plan(&desired, &stored, &unresolved);
    assert!(!blocked.applyable());
    assert!(
        blocked.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == PlanDiagnosticCode::UnresolvedExternalSelector
        })
    );

    let resolved = remote("application", "application.api", observation())
        .with_external_resolutions([(
            (address("application.api"), path("application", "server")),
            ExternalResolution::Resolved(RemoteId::new("server-1").unwrap()),
        )])
        .expect("the selector resolution is valid");
    let allowed = plan(&desired, &stored, &resolved);
    assert!(allowed.applyable(), "{allowed:?}");

    // A server selector may name the local server; a non-server selector may not.
    desired_one(
        "application.api",
        application(vec![
            ("replicas", value(json!(1))),
            ("server", value(json!({"local": true}))),
        ]),
    )
    .expect("local is valid for a server selector");
    assert!(matches!(
        desired_one(
            "application.api",
            application(vec![
                ("replicas", value(json!(1))),
                ("server", value(json!({"local": false}))),
            ])
        ),
        Err(DesiredStateError::InvalidPropertyValue { .. })
    ));
}

#[test]
fn stored_state_that_the_spec_cannot_place_is_refused() {
    let project_state = |managed| {
        StoredState::try_from_state_with_specs(
            &state_with(
                ResourceKind::Project,
                "project.platform",
                "project-1",
                managed,
                SensitiveInputs::default(),
            ),
            specs(),
        )
    };
    assert!(matches!(
        project_state(json!({"unknown_field": "x"})),
        Err(StoredStateError::UnsupportedProperty { .. })
    ));
    assert!(matches!(
        project_state(json!({"description": 7})),
        Err(StoredStateError::InvalidPropertyValue { .. })
    ));
    project_state(json!({"description": null})).expect("an owned clear projects");

    // A kind without a spec cannot be projected through the registry.
    let tag = state_with(
        ResourceKind::Tag,
        "tag.prod",
        "tag-1",
        json!({"name": "x"}),
        SensitiveInputs::default(),
    );
    assert!(matches!(
        StoredState::try_from_state_with_specs(&tag, specs()),
        Err(StoredStateError::UnsupportedProperty { .. })
    ));
}

#[test]
fn a_planned_create_materializes_nested_durable_inputs() {
    let address_ = address("application.api");
    let desired = desired_one(
        "application.api",
        application(vec![
            ("replicas", value(json!(2))),
            ("source", value(json!({"type": "docker", "image": "nginx"}))),
            ("environment.LOG_LEVEL", secret(4)),
            ("description", OwnedValue::Null),
        ]),
    )
    .unwrap();
    let remote = remote("application", "application.api", RemoteObservation::Missing);

    let plan = plan(&desired, &StoredState::absent(instance()), &remote);
    assert!(plan.applyable(), "{plan:?}");
    let checkpoint = plan.changes()[0]
        .checkpoint()
        .present()
        .expect("create has a present checkpoint");
    let state = checkpoint
        .materialize(&address_, RemoteId::new("application-1").unwrap())
        .expect("checkpoint materializes");

    assert_eq!(
        state.last_applied().as_json(),
        &json!({
            "replicas": 2,
            "source": {"type": "docker", "image": "nginx"},
            "description": null
        })
    );
    let receipts: Vec<String> = state
        .sensitive_inputs()
        .paths()
        .map(ToString::to_string)
        .collect();
    assert_eq!(receipts, ["environment.LOG_LEVEL"]);
}

#[test]
fn paths_compare_by_kind_and_dotted_path() {
    let regex = path("redirect", "regex");
    assert_eq!(regex, path("redirect", "regex"));
    assert_ne!(regex, path("redirect", "replacement"));
    assert_ne!(
        path("project", "description"),
        path("environment", "description"),
        "the same path in two kinds is two properties"
    );
    assert_eq!(
        path("application", "environment.LOG_LEVEL").to_string(),
        "environment.LOG_LEVEL"
    );
    assert!(path("application", "environment").is_collection_root());
    assert!(
        path("application", "environment.LOG_LEVEL")
            .is_entry_of(&path("application", "environment"))
    );
    assert!(!path("application", "description").is_collection_root());
}

/// A spec-declared secret in a kind the first engine never knew plans and checkpoints: the
/// durable receipt grammar is syntactic, and the spec decides which paths are secret.
#[test]
fn a_secret_outside_the_first_engines_vocabulary_plans_and_checkpoints() {
    let spec = dokploy_spec::parse_spec(
        r#"
kind: application
scope: project
parent: environment
section: applications
title: Application
identity: { key: name, collision: [name], address: "application.{key}" }
api:
  id: applicationId
  create: { op: application.create }
fields:
  token: { type: text, class: secret, mutability: write_only }
"#,
    )
    .expect("the test spec parses");
    let token = PropertyPath::from_spec(&spec, "token").unwrap();
    assert!(token.is_sensitive());

    let desired = desired_one(
        "application.api",
        DesiredResource::new(BTreeMap::from([(token, secret(5))]))
            .with_containment(Some(address("environment.production"))),
    )
    .expect("a receipt is a valid owned value for a secret");
    let remote = RemoteState::try_new_with_contracts(
        instance(),
        [(address("application.api"), RemoteObservation::Missing)],
        [(
            address("application.api"),
            MutationContract::from_spec(&spec),
        )],
    )
    .unwrap();

    let plan = plan(&desired, &StoredState::absent(instance()), &remote);
    assert!(plan.applyable(), "{plan:?}");
    let checkpoint = plan.changes()[0].checkpoint().present().expect("present");
    let state = checkpoint
        .materialize(
            &address("application.api"),
            RemoteId::new("application-1").unwrap(),
        )
        .expect("the receipt is stored without a value");
    let receipts: Vec<String> = state
        .sensitive_inputs()
        .paths()
        .map(ToString::to_string)
        .collect();
    assert_eq!(receipts, ["token"]);
    assert_eq!(state.last_applied().as_json(), &json!({}));
}

/// Registers the repository's kinds with the state layer once per test binary.
fn registered() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        dokploy_core::register_spec_kinds(specs()).expect("repository kinds register");
    });
}

#[test]
fn registering_spec_kinds_makes_them_addressable_and_keeps_the_first_engines_facts() {
    registered();
    let registry: ResourceKind = "registry".parse().expect("registry is registered");
    assert_eq!(registry.scope(), dokploy_state::StateScope::Settings);
    assert_eq!(registry.containment_parent_kind(), None);
    assert_eq!(address("registry.main").kind(), registry);

    // Kinds the first engine already has keep their constants and facts.
    assert_eq!(
        "redirect".parse::<ResourceKind>().unwrap(),
        ResourceKind::Redirect
    );
    assert_eq!(
        ResourceKind::Redirect.containment_parent_kind(),
        Some(ResourceKind::Application)
    );
    // Registering twice is harmless.
    dokploy_core::register_spec_kinds(specs()).expect("idempotent");
}

/// The kernel half of "plan a settings document containing a registry": a kind that has
/// no first-engine code at all, planned and checkpointed from its spec alone, secret
/// included, with nothing but the spec to say what its paths mean.
#[test]
fn a_registry_is_planned_and_checkpointed_from_its_spec_alone() {
    registered();
    let registry_spec = specs().get("registry").expect("registry has a spec");
    let paths = |name: &str| PropertyPath::from_spec(registry_spec, name).unwrap();
    let desired_properties = || {
        BTreeMap::from([
            (paths("type"), value(json!("cloud"))),
            (paths("url"), value(json!("ghcr.io"))),
            (paths("username"), value(json!("ci"))),
            (paths("password"), secret(7)),
            (paths("image_prefix"), OwnedValue::Null),
        ])
    };
    let desired = desired_one("registry.main", DesiredResource::new(desired_properties()))
        .expect("desired state is valid");
    let contract = || MutationContract::from_spec(registry_spec);
    let missing = RemoteState::try_new_with_contracts(
        instance(),
        [(address("registry.main"), RemoteObservation::Missing)],
        [(address("registry.main"), contract())],
    )
    .unwrap();

    let created = plan(&desired, &StoredState::absent(instance()), &missing);
    assert!(created.applyable(), "{created:?}");
    assert_eq!(created.changes()[0].kind(), ChangeKind::Create);
    assert!(
        !format!("{created:?}").contains("ghcr.io"),
        "a plan never shows values"
    );

    // Apply: the checkpoint becomes durable state with the secret as a receipt only.
    let checkpoint = created.changes()[0]
        .checkpoint()
        .present()
        .expect("present");
    let resource = checkpoint
        .materialize(
            &address("registry.main"),
            RemoteId::new("registry-1").unwrap(),
        )
        .expect("the registry checkpoint materializes");
    let mut file = StateFile::new_for_document(
        Version::new(0, 1, 0),
        instance(),
        dokploy_state::DocumentId::Settings,
    );
    file.upsert_resource(address("registry.main"), resource)
        .unwrap();
    let text = serde_json::to_string(&file).unwrap();
    assert!(text.contains("\"password\"") && !text.contains("ghcr.io-secret"));

    // The next plan, against what the checkpoint recorded, converges.
    let stored = StoredState::try_from_state_with_specs(&file, specs()).expect("projects");
    let observed = present(
        "registry-1",
        "registry",
        vec![
            ("type", known(json!("cloud"))),
            ("url", known(json!("ghcr.io"))),
            ("username", known(json!("ci"))),
            (
                "password",
                PropertyObservation::Unknown(dokploy_core::PropertyUnknownReason::Sensitive),
            ),
            ("image_prefix", PropertyObservation::KnownAbsent),
        ],
    );
    let remote = RemoteState::try_new_with_contracts(
        instance(),
        [(address("registry.main"), observed)],
        [(address("registry.main"), contract())],
    )
    .unwrap();
    let converged = plan(&desired, &stored, &remote);
    assert!(converged.applyable(), "{converged:?}");
    assert!(converged.changes().is_empty(), "{converged:?}");

    // Rotating the password plans one in-place update, naming only the path.
    let mut rotated = desired_properties();
    rotated.insert(paths("password"), secret(8));
    let rotation = desired_one("registry.main", DesiredResource::new(rotated)).unwrap();
    let updated = plan(&rotation, &stored, &remote);
    assert_eq!(
        updated.changes()[0].kind(),
        ChangeKind::Update,
        "{updated:?}"
    );
    let changed: Vec<String> = updated.changes()[0]
        .fields()
        .iter()
        .map(|field| field.key().to_string())
        .collect();
    assert_eq!(changed, ["password"]);
}

#[test]
fn a_nested_desired_address_must_name_its_containment() {
    let nested = address("project.shop/environment.staging/application.api");
    let wrong = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            nested.clone(),
            application(vec![("replicas", value(json!(1)))]),
        )]),
    );
    assert!(matches!(
        wrong,
        Err(DesiredStateError::ContainmentAddressMismatch { .. })
    ));

    let right = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            nested,
            DesiredResource::new(BTreeMap::from([(
                path("application", "replicas"),
                value(json!(1)),
            )]))
            .with_containment(Some(address("project.shop/environment.staging"))),
        )]),
    );
    right.expect("containment equal to the address parent is accepted");
}
