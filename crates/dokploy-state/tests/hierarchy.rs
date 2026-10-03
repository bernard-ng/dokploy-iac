//! Hierarchical addresses, registered kinds, and per-document stores (ADR 0006, 0009).

mod support;

use support::kinds;

use std::fs;

use dokploy_state::{
    AddressSuffixError, DocumentId, ExpectedState, InstanceIdentity, KindRegistrationError,
    ManagedInputs, RemoteId, ResourceAddress, ResourceAddressParseError, ResourceKind,
    ResourceName, ResourceState, ResourceStateError, SensitiveInputs, StateError, StateFile,
    StateScope, StateStore, StateStoreError,
};
use semver::Version;
use serde_json::json;
use tempfile::tempdir;

const PROJECT: &str = "project.shop";
const ENVIRONMENT: &str = "project.shop/environment.staging";
const APPLICATION: &str = "project.shop/environment.staging/application.api";
const REDIRECT: &str = "project.shop/environment.staging/application.api/redirect.www";

fn address(value: &str) -> ResourceAddress {
    kinds();
    value
        .parse()
        .unwrap_or_else(|error| panic!("{value}: {error}"))
}

fn instance() -> InstanceIdentity {
    kinds();
    InstanceIdentity::parse("https://deploy.example.com").expect("instance is valid")
}

fn resource(kind: ResourceKind, id: &str, containment: Option<&str>) -> ResourceState {
    ResourceState::new(
        kind,
        RemoteId::new(id).expect("remote id is valid"),
        false,
        ManagedInputs::try_from_json(json!({})).expect("inputs are valid"),
        containment.map(address),
        Vec::new(),
    )
}

/// A project document holding one environment, one application, and one redirect.
fn tree() -> StateFile {
    let mut state = StateFile::new(
        Version::new(0, 1, 0),
        instance(),
        DocumentId::Project("shop".parse().unwrap()),
    );
    for (path, kind, id, parent) in [
        (PROJECT, kinds().project, "project-1", None),
        (
            ENVIRONMENT,
            kinds().environment,
            "environment-1",
            Some(PROJECT),
        ),
        (
            APPLICATION,
            kinds().application,
            "application-1",
            Some(ENVIRONMENT),
        ),
        (REDIRECT, kinds().redirect, "redirect-1", Some(APPLICATION)),
    ] {
        state
            .upsert_resource(address(path), resource(kind, id, parent))
            .unwrap_or_else(|error| panic!("{path}: {error}"));
    }
    state
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

#[test]
fn an_address_is_a_path_of_kind_key_segments() {
    let redirect = address(REDIRECT);
    assert_eq!(redirect.to_string(), REDIRECT);
    assert_eq!(redirect.depth(), 4);
    assert_eq!(redirect.kind(), kinds().redirect);
    assert_eq!(redirect.name().as_str(), "www");
    assert_eq!(redirect.parent(), Some(address(APPLICATION)));
    assert_eq!(address(PROJECT).parent(), None);

    let built = address(PROJECT)
        .child(kinds().environment, ResourceName::new("staging").unwrap())
        .unwrap();
    assert_eq!(built, address(ENVIRONMENT));

    // A first-engine address is a path of one segment, with the same text as before.
    let flat = address("application.api");
    assert_eq!((flat.depth(), flat.parent()), (1, None));
    assert_eq!(flat.to_string(), "application.api");
}

#[test]
fn malformed_addresses_are_rejected() {
    for bad in [
        "",
        "/",
        "project.shop/",
        "/project.shop",
        "project.shop//environment.staging",
        "project.shop/environment",
        "unknownkind.x",
        "project.UPPER",
    ] {
        assert!(bad.parse::<ResourceAddress>().is_err(), "`{bad}`");
    }
    let deep = vec!["environment.e"; 17].join("/");
    assert_eq!(
        deep.parse::<ResourceAddress>(),
        Err(ResourceAddressParseError::TooDeep)
    );
    let long = format!("application.{}", "a".repeat(9000));
    assert_eq!(
        long.parse::<ResourceAddress>(),
        Err(ResourceAddressParseError::TooLong)
    );
}

#[test]
fn ancestry_and_rebasing_follow_the_path() {
    let environment = address(ENVIRONMENT);
    let redirect = address(REDIRECT);
    assert!(environment.is_ancestor_of(&redirect));
    assert!(!redirect.is_ancestor_of(&environment));
    assert!(!environment.is_ancestor_of(&environment), "strictly below");
    assert!(environment.starts_with(&environment) && redirect.starts_with(&environment));

    let renamed = address("project.shop/environment.production");
    assert_eq!(
        redirect.rebased(&environment, &renamed),
        Some(address(
            "project.shop/environment.production/application.api/redirect.www"
        ))
    );
    assert_eq!(address(PROJECT).rebased(&environment, &renamed), None);
}

#[test]
fn addresses_order_a_parent_before_its_descendants() {
    let mut all = [
        address(REDIRECT),
        address(PROJECT),
        address(APPLICATION),
        address(ENVIRONMENT),
    ];
    all.sort();
    assert_eq!(
        all.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [PROJECT, ENVIRONMENT, APPLICATION, REDIRECT]
    );
}

#[test]
fn a_typed_suffix_resolves_when_it_names_exactly_one_address() {
    let known = [
        address("project.shop/environment.staging/application.api/domain.main"),
        address("project.shop/environment.staging/application.web/domain.main"),
        address("project.shop/environment.staging/application.web/domain.admin"),
        address(APPLICATION),
    ];
    let resolve = |text: &str| ResourceAddress::resolve_suffix(known.iter(), text);

    assert_eq!(resolve("domain.admin"), Ok(&known[2]));
    assert_eq!(resolve("application.web/domain.main"), Ok(&known[1]));
    assert_eq!(resolve(&known[0].to_string()), Ok(&known[0]), "exact");
    assert_eq!(resolve("application.api"), Ok(&known[3]));

    let Err(AddressSuffixError::Ambiguous { candidates, .. }) = resolve("domain.main") else {
        panic!("domain.main is ambiguous");
    };
    assert_eq!(candidates, [known[0].clone(), known[1].clone()]);
    assert!(
        resolve("domain.main")
            .unwrap_err()
            .to_string()
            .contains("application.web")
    );

    assert!(matches!(
        resolve("domain.nope"),
        Err(AddressSuffixError::NotFound { .. })
    ));
    assert!(matches!(
        resolve("not an address"),
        Err(AddressSuffixError::Malformed(_))
    ));
}

// ---------------------------------------------------------------------------
// Registered kinds
// ---------------------------------------------------------------------------

#[test]
fn a_spec_kind_must_be_registered_before_it_can_be_named() {
    assert!("widget_alpha.main".parse::<ResourceAddress>().is_err());

    let kind = ResourceKind::register("widget_alpha", StateScope::Settings, &[]).unwrap();
    assert_eq!(kind.as_str(), "widget_alpha");
    assert_eq!(kind.scope(), StateScope::Settings);
    assert!(kind.containment_parent_kinds().is_empty());
    assert_eq!("widget_alpha".parse::<ResourceKind>().unwrap(), kind);
    assert_eq!(address("widget_alpha.main").kind(), kind);

    // Registering again with the same facts is idempotent; different facts are refused.
    assert_eq!(
        ResourceKind::register("widget_alpha", StateScope::Settings, &[]).unwrap(),
        kind
    );
    assert!(matches!(
        ResourceKind::register("widget_alpha", StateScope::Project, &[]),
        Err(KindRegistrationError::Conflict { .. })
    ));
}

#[test]
fn a_registered_kind_carries_its_containment() {
    let parent = ResourceKind::register("widget_parent", StateScope::Project, &[]).unwrap();
    let child = ResourceKind::register("widget_child", StateScope::Project, &[parent]).unwrap();
    assert_eq!(child.containment_parent_kinds(), &[parent]);
    assert_eq!(child.scope(), StateScope::Project);

    let serialized = serde_json::to_string(&child).unwrap();
    assert_eq!(serialized, "\"widget_child\"");
    assert_eq!(
        serde_json::from_str::<ResourceKind>(&serialized).unwrap(),
        child
    );
}

#[test]
fn the_first_engine_kinds_keep_their_built_in_facts() {
    assert_eq!(
        ResourceKind::register("redirect", StateScope::Project, &[kinds().application]).unwrap(),
        kinds().redirect
    );
    assert!(matches!(
        ResourceKind::register("redirect", StateScope::Settings, &[]),
        Err(KindRegistrationError::Conflict { .. })
    ));
    assert_eq!(kinds().redirect.scope(), StateScope::Project);
}

#[test]
fn kind_names_are_validated() {
    for bad in [
        "",
        "Widget",
        "1widget",
        "wid-get",
        "wid get",
        &"a".repeat(65),
    ] {
        assert!(
            matches!(
                ResourceKind::register(bad, StateScope::Project, &[]),
                Err(KindRegistrationError::InvalidName { .. })
            ),
            "`{bad}`"
        );
    }
}

#[test]
fn state_with_an_unknown_kind_does_not_decode() {
    let mut value = serde_json::to_value(tree()).unwrap();
    let resources = value["resources"].as_object_mut().unwrap();
    let project = resources[PROJECT].clone();
    resources.insert("gadget_unregistered.x".to_owned(), project);
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(StateFile::from_json_slice(&bytes).is_err());
}

// ---------------------------------------------------------------------------
// The state file
// ---------------------------------------------------------------------------

#[test]
fn a_hierarchical_state_round_trips_with_full_addresses_as_keys() {
    let state = tree();
    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(value["document"], "project.shop");
    let keys: Vec<&String> = value["resources"].as_object().unwrap().keys().collect();
    assert!(keys.contains(&&REDIRECT.to_owned()));

    let decoded = StateFile::from_json_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(decoded, state);
}

#[test]
fn a_nested_address_must_be_contained_by_its_path_parent() {
    let mut state = tree();

    // The redirect must be contained by the application its address is nested under;
    // another application of the right kind is not enough.
    let mismatch = state.upsert_resource(
        address("project.shop/environment.staging/application.api/redirect.other"),
        resource(
            kinds().redirect,
            "redirect-2",
            Some("project.shop/environment.staging/application.web"),
        ),
    );
    assert!(matches!(
        mismatch,
        Err(StateError::AddressContainmentMismatch { .. })
    ));

    // The named parent must exist.
    let orphan = state.upsert_resource(
        address("project.shop/environment.staging/application.web/redirect.www"),
        resource(
            kinds().redirect,
            "redirect-3",
            Some("project.shop/environment.staging/application.web"),
        ),
    );
    assert!(matches!(
        orphan,
        Err(StateError::MissingResourceReference { .. })
    ));

    // The same rule applies when decoding.
    let mut value = serde_json::to_value(&state).unwrap();
    value["resources"]
        .as_object_mut()
        .unwrap()
        .remove(APPLICATION);
    assert!(StateFile::from_json_slice(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn moving_an_address_moves_everything_below_it() {
    let mut state = tree();
    let serial = state.serial();

    state
        .move_resource(
            &address(ENVIRONMENT),
            address("project.shop/environment.production"),
        )
        .expect("a prefix move succeeds");

    assert_eq!(
        state.serial(),
        serial + 1,
        "one move advances the serial once"
    );
    assert!(state.resource(&address(ENVIRONMENT)).is_none());
    let moved_application = address("project.shop/environment.production/application.api");
    let moved_redirect =
        address("project.shop/environment.production/application.api/redirect.www");
    assert_eq!(
        state
            .resource(&moved_application)
            .unwrap()
            .remote_id()
            .as_str(),
        "application-1",
        "remote identity is unchanged"
    );
    assert_eq!(
        state.resource(&moved_redirect).unwrap().containment(),
        Some(&moved_application),
        "containment follows the rename"
    );
    assert_eq!(
        state.resource(&moved_application).unwrap().containment(),
        Some(&address("project.shop/environment.production"))
    );
    // The result still satisfies every hierarchy rule when it is written and read back.
    let decoded =
        StateFile::from_json_slice(&serde_json::to_vec(&state).unwrap()).expect("round trips");
    assert_eq!(decoded, state);
}

#[test]
fn a_service_can_be_moved_to_another_environment() {
    let mut state = tree();
    state
        .upsert_resource(
            address("project.shop/environment.production"),
            resource(kinds().environment, "environment-2", Some(PROJECT)),
        )
        .unwrap();

    state
        .move_resource(
            &address(APPLICATION),
            address("project.shop/environment.production/application.api"),
        )
        .expect("a reparenting move succeeds");

    let moved = state
        .resource(&address(
            "project.shop/environment.production/application.api",
        ))
        .unwrap();
    assert_eq!(
        moved.containment(),
        Some(&address("project.shop/environment.production"))
    );
    assert!(
        state
            .resource(&address(
                "project.shop/environment.production/application.api/redirect.www"
            ))
            .is_some()
    );
}

#[test]
fn impossible_moves_are_refused_without_changing_state() {
    let mut state = tree();
    let before = state.clone();

    assert!(matches!(
        state.move_resource(
            &address(ENVIRONMENT),
            address("project.shop/environment.staging/environment.inner")
        ),
        Err(StateError::MoveIntoItself { .. })
    ));
    assert!(matches!(
        state.move_resource(
            &address(APPLICATION),
            address("project.shop/environment.staging/compose.api")
        ),
        Err(StateError::MoveKindMismatch { .. })
    ));
    assert!(matches!(
        state.move_resource(
            &address(APPLICATION),
            address("project.shop/environment.missing/application.api")
        ),
        Err(StateError::MissingResourceReference { .. })
    ));
    assert_eq!(state, before);
}

#[test]
fn forgetting_a_resource_with_descendants_is_refused() {
    let mut state = tree();
    assert!(matches!(
        state.forget_resource(&address(APPLICATION)),
        Err(StateError::ResourceHasDependents { .. })
    ));
    assert!(state.forget_resource(&address(REDIRECT)).unwrap().is_some());
    assert!(
        state
            .forget_resource(&address(APPLICATION))
            .unwrap()
            .is_some()
    );
}

// ---------------------------------------------------------------------------
// Documents and stores
// ---------------------------------------------------------------------------

fn project(slug: &str) -> DocumentId {
    DocumentId::Project(slug.parse().unwrap())
}

fn store(workspace: &std::path::Path, document: DocumentId) -> StateStore {
    StateStore::for_document(workspace, instance(), document).expect("store binds")
}

#[test]
fn every_document_has_its_own_directory() {
    let workspace = tempdir().unwrap();
    let cases = [
        (DocumentId::Settings, ".dokploy/settings"),
        (project("shop"), ".dokploy/projects/shop"),
    ];
    for (document, directory) in cases {
        let store = store(workspace.path(), document.clone());
        let state = StateFile::new(Version::new(0, 1, 0), instance(), document);
        store
            .begin_write()
            .unwrap()
            .checkpoint(ExpectedState::absent(), &state)
            .unwrap();
        assert!(
            workspace
                .path()
                .join(directory)
                .join("state.json")
                .is_file(),
            "{directory}"
        );
        assert_eq!(store.inspect().unwrap().unwrap(), state);
    }
}

#[test]
fn two_project_documents_never_block_each_other() {
    let workspace = tempdir().unwrap();
    let shop = store(workspace.path(), project("shop"));
    let blog = store(workspace.path(), project("blog"));

    let _held = shop.begin_write().expect("the shop lock is taken");
    let second = shop.begin_write();
    assert!(matches!(second, Err(StateStoreError::LockContended)));
    blog.begin_write()
        .expect("another project's lock is independent");
    store(workspace.path(), DocumentId::Settings)
        .begin_write()
        .expect("so is the settings lock");
}

#[test]
fn a_store_refuses_state_recorded_for_another_document() {
    let workspace = tempdir().unwrap();
    let shop = store(workspace.path(), project("shop"));
    let state = StateFile::new(Version::new(0, 1, 0), instance(), project("shop"));
    shop.begin_write()
        .unwrap()
        .checkpoint(ExpectedState::absent(), &state)
        .unwrap();

    // The shop file is copied into the blog directory by hand.
    let blog_directory = workspace.path().join(".dokploy/projects/blog");
    fs::create_dir_all(&blog_directory).unwrap();
    fs::copy(
        workspace.path().join(".dokploy/projects/shop/state.json"),
        blog_directory.join("state.json"),
    )
    .unwrap();
    let blog = store(workspace.path(), project("blog"));
    assert!(matches!(
        blog.inspect(),
        Err(StateStoreError::StateDocumentMismatch { .. })
    ));

    // A write of the wrong document is refused before anything is written.
    let other = StateFile::new(Version::new(0, 1, 0), instance(), project("shop"));
    let fresh = store(workspace.path(), project("news"));
    assert!(matches!(
        fresh
            .begin_write()
            .unwrap()
            .checkpoint(ExpectedState::absent(), &other),
        Err(StateStoreError::ProposedDocumentMismatch { .. })
    ));
}

#[test]
fn a_document_decides_the_scope_of_its_state() {
    assert_eq!(DocumentId::Settings.scope(), StateScope::Settings);
    assert_eq!(project("shop").scope(), StateScope::Project);
}

#[test]
fn a_kind_can_live_under_several_parents() {
    let first = ResourceKind::register("widget_home", StateScope::Project, &[]).unwrap();
    let second = ResourceKind::register("widget_garage", StateScope::Project, &[]).unwrap();
    let shared =
        ResourceKind::register("widget_shared", StateScope::Project, &[first, second]).unwrap();

    assert_eq!(shared.containment_parent_kinds(), &[first, second]);
    assert_eq!(
        ResourceKind::register("widget_shared", StateScope::Project, &[first, second]).unwrap(),
        shared,
        "the same facts register again"
    );
    assert!(matches!(
        ResourceKind::register("widget_shared", StateScope::Project, &[first]),
        Err(KindRegistrationError::Conflict { .. })
    ));

    let state_under = |parent: &str| {
        ResourceState::try_new(
            shared,
            RemoteId::new("remote-1").unwrap(),
            false,
            ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
            SensitiveInputs::default(),
            Some(parent.parse::<ResourceAddress>().unwrap()),
            Vec::new(),
        )
    };
    assert!(state_under("widget_home.a").is_ok());
    assert!(state_under("widget_garage.b").is_ok());
    ResourceKind::register("widget_stranger", StateScope::Project, &[]).unwrap();
    assert!(matches!(
        state_under("widget_stranger.c"),
        Err(ResourceStateError::InvalidContainmentKind { .. })
    ));
}
