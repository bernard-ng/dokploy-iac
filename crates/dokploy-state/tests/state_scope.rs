//! Document scopes: independent project and settings state lineages.

use std::{collections::BTreeMap, fs};

use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, StateError, StateFile, StateScope, StateStore, StateStoreError,
};
use semver::Version;
use serde_json::json;
use tempfile::tempdir;

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://deploy.example.com").expect("the instance must be valid")
}

fn project_state() -> StateFile {
    StateFile::new(Version::new(0, 1, 0), instance())
}

fn settings_state() -> StateFile {
    StateFile::new_in_scope(Version::new(0, 1, 0), instance(), StateScope::Settings)
}

fn project_resource(kind: ResourceKind, id: &str) -> ResourceState {
    ResourceState::new(
        kind,
        RemoteId::new(id).expect("remote ID must be valid"),
        false,
        ManagedInputs::try_from_json(json!({})).expect("managed inputs must be valid"),
        None,
        Vec::new(),
    )
}

fn legacy_v3_json() -> serde_json::Value {
    let mut value = serde_json::to_value(project_state()).expect("state must serialize");
    value["formatVersion"] = json!(3);
    value
        .as_object_mut()
        .expect("state is an object")
        .remove("scope");
    value
}

fn decode(value: &serde_json::Value) -> Result<StateFile, dokploy_state::StateDecodeError> {
    StateFile::from_json_slice(&serde_json::to_vec(value).expect("state JSON must serialize"))
}

// ---------------------------------------------------------------------------
// The state file
// ---------------------------------------------------------------------------

#[test]
fn new_states_default_to_project_scope_in_format_four() {
    let state = project_state();

    assert_eq!(state.scope(), StateScope::Project);
    assert_eq!(state.format_version(), 4);
    let encoded = serde_json::to_value(&state).expect("state must serialize");
    assert_eq!(encoded["scope"], "project");
    assert_eq!(encoded["formatVersion"], 4);
}

#[test]
fn settings_scope_round_trips_through_json() {
    let state = settings_state();
    let encoded = serde_json::to_value(&state).expect("state must serialize");

    assert_eq!(encoded["scope"], "settings");
    assert_eq!(decode(&encoded).expect("settings state decodes"), state);
}

#[test]
fn a_format_three_file_decodes_as_a_project_and_is_upgraded_in_memory() {
    let legacy = legacy_v3_json();
    assert_eq!(legacy["formatVersion"], 3);

    let state = decode(&legacy).expect("a legacy project state must still decode");

    assert_eq!(state.scope(), StateScope::Project);
    assert_eq!(state.format_version(), 4);
    let rewritten = serde_json::to_value(&state).expect("state must serialize");
    assert_eq!(rewritten["formatVersion"], 4);
    assert_eq!(rewritten["scope"], "project");
}

#[test]
fn the_scope_field_must_agree_with_the_format_version() {
    let mut legacy_with_scope = legacy_v3_json();
    legacy_with_scope["scope"] = json!("project");
    assert!(decode(&legacy_with_scope).is_err(), "format 3 has no scope");

    let mut missing = serde_json::to_value(project_state()).expect("state must serialize");
    missing.as_object_mut().expect("object").remove("scope");
    assert!(decode(&missing).is_err(), "format 4 requires a scope");

    let mut unknown = serde_json::to_value(project_state()).expect("state must serialize");
    unknown["scope"] = json!("cluster");
    assert!(decode(&unknown).is_err(), "an unknown scope is rejected");
}

#[test]
fn a_settings_lineage_rejects_project_resources() {
    let mut state = settings_state();
    let address: ResourceAddress = "project.main".parse().expect("address parses");

    let error = state
        .upsert_resource(
            address.clone(),
            project_resource(ResourceKind::Project, "project-1"),
        )
        .expect_err("a project kind does not belong in settings state");

    assert!(
        matches!(
            error,
            StateError::ResourceOutOfScope {
                scope: StateScope::Settings,
                ..
            }
        ),
        "{error}"
    );
    assert_eq!(state.serial(), 0, "a rejected upsert advances nothing");

    let error = StateFile::new_with_resources_in_scope(
        Version::new(0, 1, 0),
        instance(),
        StateScope::Settings,
        BTreeMap::from([(
            address,
            project_resource(ResourceKind::Project, "project-1"),
        )]),
    )
    .expect_err("an imported project kind does not belong in settings state");
    assert!(
        matches!(error, StateError::ResourceOutOfScope { .. }),
        "{error}"
    );
}

#[test]
fn a_decoded_settings_file_with_a_project_resource_is_rejected() {
    let mut project = project_state();
    project
        .upsert_resource(
            "project.main".parse().expect("address parses"),
            project_resource(ResourceKind::Project, "project-1"),
        )
        .expect("a project resource belongs in project state");
    let mut encoded = serde_json::to_value(&project).expect("state must serialize");
    encoded["scope"] = json!("settings");

    assert!(decode(&encoded).is_err());
}

#[test]
fn every_current_kind_belongs_to_the_project_scope() {
    for kind in [
        ResourceKind::Project,
        ResourceKind::Environment,
        ResourceKind::Application,
        ResourceKind::Backup,
    ] {
        assert_eq!(kind.scope(), StateScope::Project, "{kind}");
    }
    assert_eq!(StateScope::default(), StateScope::Project);
    assert_eq!(StateScope::Settings.to_string(), "settings");
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

#[test]
fn settings_state_lives_beside_project_state_and_never_blocks_it() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let project = StateStore::new(workspace.path(), instance()).expect("project store binds");
    let settings = StateStore::with_scope(workspace.path(), instance(), StateScope::Settings)
        .expect("settings store binds");
    assert_eq!(project.scope(), StateScope::Project);
    assert_eq!(settings.scope(), StateScope::Settings);

    // The project writer lock is held while the settings scope writes.
    let mut project_session = project.begin_write().expect("project lock is acquired");
    let mut settings_session = settings
        .begin_write()
        .expect("a held project lock must not block the settings scope");
    project_session
        .checkpoint(ExpectedState::absent(), &project_state())
        .expect("project state initializes");
    settings_session
        .checkpoint(ExpectedState::absent(), &settings_state())
        .expect("settings state initializes");
    drop((project_session, settings_session));

    assert!(workspace.path().join(".dokploy/state.json").is_file());
    assert!(
        workspace
            .path()
            .join(".dokploy/settings/state.json")
            .is_file()
    );
    assert!(
        workspace
            .path()
            .join(".dokploy/settings/state.lock")
            .is_file()
    );
    assert_eq!(
        project
            .inspect()
            .expect("project readable")
            .map(|s| s.scope()),
        Some(StateScope::Project)
    );
    assert_eq!(
        settings
            .inspect()
            .expect("settings readable")
            .map(|s| s.scope()),
        Some(StateScope::Settings)
    );
}

#[test]
fn a_settings_store_alone_leaves_project_state_absent() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let settings = StateStore::with_scope(workspace.path(), instance(), StateScope::Settings)
        .expect("settings store binds");
    settings
        .begin_write()
        .expect("settings lock is acquired")
        .checkpoint(ExpectedState::absent(), &settings_state())
        .expect("settings state initializes");

    let project = StateStore::new(workspace.path(), instance()).expect("project store binds");

    assert_eq!(project.inspect().expect("project is readable"), None);
    assert!(!workspace.path().join(".dokploy/state.json").exists());
}

#[test]
fn a_store_refuses_to_write_or_read_another_scope() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let settings = StateStore::with_scope(workspace.path(), instance(), StateScope::Settings)
        .expect("settings store binds");

    let error = settings
        .begin_write()
        .expect("lock is acquired")
        .checkpoint(ExpectedState::absent(), &project_state())
        .expect_err("project state must not enter the settings directory");
    assert!(
        matches!(
            error,
            StateStoreError::ProposedScopeMismatch {
                found: StateScope::Project,
                expected: StateScope::Settings
            }
        ),
        "{error}"
    );

    // A settings file copied into the project directory is never read as project state.
    settings
        .begin_write()
        .expect("lock is acquired")
        .checkpoint(ExpectedState::absent(), &settings_state())
        .expect("settings state initializes");
    fs::copy(
        workspace.path().join(".dokploy/settings/state.json"),
        workspace.path().join(".dokploy/state.json"),
    )
    .expect("state file is copied");
    let project = StateStore::new(workspace.path(), instance()).expect("project store binds");

    let error = project
        .inspect()
        .expect_err("a foreign scope must be refused");
    assert!(
        matches!(
            error,
            StateStoreError::StateScopeMismatch {
                found: StateScope::Settings,
                expected: StateScope::Project
            }
        ),
        "{error}"
    );
}

#[test]
fn a_legacy_project_file_is_readable_and_rewritten_as_format_four() {
    let workspace = tempdir().expect("temporary workspace must be created");
    let directory = workspace.path().join(".dokploy");
    fs::create_dir(&directory).expect("state directory is created");
    let legacy = legacy_v3_json();
    fs::write(
        directory.join("state.json"),
        serde_json::to_vec(&legacy).expect("legacy JSON serializes"),
    )
    .expect("legacy state is written");
    let store = StateStore::new(workspace.path(), instance()).expect("project store binds");

    let loaded = store
        .inspect()
        .expect("legacy state is readable")
        .expect("legacy state exists");
    assert_eq!(loaded.scope(), StateScope::Project);

    let mut next = loaded.clone();
    next.upsert_resource(
        "project.main".parse().expect("address parses"),
        project_resource(ResourceKind::Project, "project-1"),
    )
    .expect("a resource is added");
    store
        .begin_write()
        .expect("lock is acquired")
        .checkpoint(ExpectedState::from_state(&loaded), &next)
        .expect("the next checkpoint succeeds");

    let rewritten: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("state.json")).expect("state is read"))
            .expect("state is JSON");
    assert_eq!(rewritten["formatVersion"], 4);
    assert_eq!(rewritten["scope"], "project");
}

#[cfg(unix)]
#[test]
fn the_settings_directory_is_hardened_and_never_follows_a_symlinked_parent() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let workspace = tempdir().expect("temporary workspace must be created");
    let settings = StateStore::with_scope(workspace.path(), instance(), StateScope::Settings)
        .expect("settings store binds");
    settings
        .begin_write()
        .expect("lock is acquired")
        .checkpoint(ExpectedState::absent(), &settings_state())
        .expect("settings state initializes");
    for directory in [".dokploy", ".dokploy/settings"] {
        let mode = fs::metadata(workspace.path().join(directory))
            .expect("directory exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{directory} must be private");
    }

    // A symlinked `.dokploy` would redirect settings state outside the workspace.
    let hostile = tempdir().expect("temporary workspace must be created");
    let target = tempdir().expect("redirect target must be created");
    symlink(target.path(), hostile.path().join(".dokploy")).expect("symlink is created");
    let store = StateStore::with_scope(hostile.path(), instance(), StateScope::Settings)
        .expect("binding does not touch the disk");

    let error = store
        .begin_write()
        .err()
        .expect("a symlinked state parent must be refused");

    assert!(
        matches!(error, StateStoreError::UnsafeStateDirectory),
        "{error}"
    );
    assert!(
        fs::read_dir(target.path())
            .expect("target is readable")
            .next()
            .is_none(),
        "nothing may be written through the symlink"
    );
}
