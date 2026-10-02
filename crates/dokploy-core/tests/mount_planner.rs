use std::collections::BTreeMap;

use dokploy_core::{
    ChangeKind, ComparableValue, ConfigDigest, DesiredResource, DesiredState, DesiredStateError,
    MutationContract, MutationMode, OwnedValue, Plan, PlanDiagnosticCode, PropertyMutation,
    PropertyObservation, PropertyPath, PropertyUnknownReason, RemoteObservation, RemoteResource,
    RemoteState, RemoteStateError, RemovalDirective, ReplacementOrder, SensitiveIntent,
    StoredState, StoredStateError, plan,
};
use dokploy_state::{
    FingerprintKeyId, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath, StateFile,
};
use semver::Version;
use serde_json::json;
use uuid::Uuid;

const CONTENT_CANARY: &str = "mount-content-canary-never-leak";

fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
}

fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).expect("digest is valid")
}

fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://dokploy.example.test").expect("instance is valid")
}

fn value(value: serde_json::Value) -> ComparableValue {
    ComparableValue::try_from_json(value).expect("value is comparable")
}

fn text(value_text: &str) -> OwnedValue {
    OwnedValue::Value(value(json!(value_text)))
}

fn intent(mac_byte: u8) -> OwnedValue {
    let key_id = FingerprintKeyId::new(
        Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").expect("UUID parses"),
    )
    .expect("key id is valid");
    OwnedValue::Sensitive(SensitiveIntent::from_fingerprint(
        SensitiveFingerprint::new_v1(key_id, [mac_byte; 32]),
    ))
}

fn mount_contract() -> MutationContract {
    let in_place = PropertyMutation::new(MutationMode::InPlace, MutationMode::InPlace);
    let set_only = PropertyMutation::new(MutationMode::InPlace, MutationMode::Unsupported);
    let replace = PropertyMutation::new(MutationMode::Replace, MutationMode::Unsupported);
    MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
        .requiring(PropertyPath::Target)
        .requiring(PropertyPath::MountType)
        .requiring(PropertyPath::MountPath)
        .allowing_on_create(PropertyPath::HostPath)
        .allowing_on_create(PropertyPath::VolumeName)
        .allowing_on_create(PropertyPath::FilePath)
        .allowing_on_create(PropertyPath::FileContent)
        .with_property(PropertyPath::Target, replace)
        .with_property(PropertyPath::MountType, replace)
        .with_property(PropertyPath::MountPath, in_place)
        .with_property(PropertyPath::HostPath, set_only)
        .with_property(PropertyPath::VolumeName, set_only)
        .with_property(PropertyPath::FilePath, set_only)
        .with_property(PropertyPath::FileContent, set_only)
        .with_containment(MutationMode::StateOnly)
}

fn volume_properties(
    target: &str,
    mount_path: &str,
    volume: &str,
) -> BTreeMap<PropertyPath, OwnedValue> {
    BTreeMap::from([
        (PropertyPath::Target, text(target)),
        (PropertyPath::MountType, text("volume")),
        (PropertyPath::MountPath, text(mount_path)),
        (PropertyPath::VolumeName, text(volume)),
    ])
}

fn volume_observation(
    target: &str,
    mount_path: &str,
    volume: &str,
) -> BTreeMap<PropertyPath, PropertyObservation> {
    BTreeMap::from([
        (
            PropertyPath::Target,
            PropertyObservation::Known(value(json!(target))),
        ),
        (
            PropertyPath::MountType,
            PropertyObservation::Known(value(json!("volume"))),
        ),
        (
            PropertyPath::MountPath,
            PropertyObservation::Known(value(json!(mount_path))),
        ),
        (
            PropertyPath::VolumeName,
            PropertyObservation::Known(value(json!(volume))),
        ),
    ])
}

fn environment() -> ResourceAddress {
    address("environment.production")
}

fn desired_resource(
    properties: BTreeMap<PropertyPath, OwnedValue>,
    dependencies: &[&str],
) -> DesiredResource {
    DesiredResource::new(properties)
        .with_containment(Some(environment()))
        .with_dependencies(dependencies.iter().map(|value| address(value)).collect())
}

fn plain(address_text: &str, dependencies: &[&str]) -> (ResourceAddress, DesiredResource) {
    let parent = match address(address_text).kind() {
        ResourceKind::Project => None,
        ResourceKind::Environment => Some(address("project.platform")),
        _ => Some(environment()),
    };
    (
        address(address_text),
        DesiredResource::new(BTreeMap::new())
            .with_containment(parent)
            .with_dependencies(dependencies.iter().map(|value| address(value)).collect()),
    )
}

fn state_resource(
    state: &mut StateFile,
    address_text: &str,
    remote_id: &str,
    managed: serde_json::Value,
    sensitive: &[&str],
    dependencies: &[&str],
    protected: bool,
) {
    let address = address(address_text);
    let key_id = FingerprintKeyId::new(
        Uuid::parse_str("0199a0c8-2351-7c31-8899-2c8f81983ea5").expect("UUID parses"),
    )
    .expect("key id is valid");
    let sensitive = SensitiveInputs::try_from_entries(sensitive.iter().map(|path| {
        (
            SensitivePropertyPath::parse(path).expect("path is valid"),
            SensitiveFingerprint::new_v1(key_id.clone(), [0xa5; 32]),
        )
    }))
    .expect("sensitive inputs are valid");
    let containment = match address.kind() {
        ResourceKind::Project => None,
        ResourceKind::Environment => Some(address_for("project.platform")),
        _ => Some(environment()),
    };
    state
        .upsert_resource(
            address.clone(),
            ResourceState::try_new(
                address.kind(),
                RemoteId::new(remote_id).expect("remote id is valid"),
                protected,
                ManagedInputs::try_from_json(managed).expect("managed inputs are valid"),
                sensitive,
                containment,
                dependencies
                    .iter()
                    .map(|value| address_for(value))
                    .collect(),
            )
            .expect("resource state is valid"),
        )
        .expect("state insert succeeds");
}

fn address_for(value: &str) -> ResourceAddress {
    address(value)
}

fn base_state() -> StateFile {
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    state_resource(
        &mut state,
        "project.platform",
        "project-1",
        json!({}),
        &[],
        &[],
        false,
    );
    state_resource(
        &mut state,
        "environment.production",
        "environment-1",
        json!({}),
        &[],
        &[],
        false,
    );
    state_resource(
        &mut state,
        "application.api",
        "application-1",
        json!({}),
        &[],
        &[],
        false,
    );
    state
}

fn present(
    remote_id: &str,
    properties: BTreeMap<PropertyPath, PropertyObservation>,
) -> RemoteObservation {
    RemoteObservation::Present(RemoteResource::new(
        RemoteId::new(remote_id).expect("remote id is valid"),
        properties,
    ))
}

fn contract_for(kind: ResourceKind) -> MutationContract {
    match kind {
        ResourceKind::Mount => mount_contract(),
        _ => MutationContract::permissive(),
    }
}

fn remote(observations: Vec<(&str, RemoteObservation)>) -> RemoteState {
    let observations = observations
        .into_iter()
        .map(|(address_text, observation)| (address(address_text), observation))
        .collect::<Vec<_>>();
    let contracts = observations
        .iter()
        .map(|(address, _)| (address.clone(), contract_for(address.kind())))
        .collect::<Vec<_>>();
    RemoteState::try_new_with_contracts(instance(), observations, contracts)
        .expect("remote snapshot is valid")
}

fn base_remote(mount: Option<RemoteObservation>) -> Vec<(&'static str, RemoteObservation)> {
    let mut observations = vec![
        ("project.platform", present("project-1", BTreeMap::new())),
        (
            "environment.production",
            present("environment-1", BTreeMap::new()),
        ),
        ("application.api", present("application-1", BTreeMap::new())),
    ];
    if let Some(mount) = mount {
        observations.push(("mount.data", mount));
    }
    observations
}

fn desired_with(
    mount: Option<BTreeMap<PropertyPath, OwnedValue>>,
    extra: Vec<(ResourceAddress, DesiredResource)>,
) -> DesiredState {
    let mut resources = BTreeMap::from_iter([
        plain("project.platform", &[]),
        plain("environment.production", &[]),
        plain("application.api", &[]),
    ]);
    resources.extend(extra);
    if let Some(properties) = mount {
        let target = if properties.get(&PropertyPath::Target) == Some(&text("application.worker")) {
            "application.worker"
        } else {
            "application.api"
        };
        resources.insert(
            address("mount.data"),
            desired_resource(properties, &[target]),
        );
    }
    DesiredState::try_new(digest(), resources).expect("desired state is valid")
}

fn stored_with_mount(protected: bool) -> StateFile {
    let mut state = base_state();
    state_resource(
        &mut state,
        "mount.data",
        "mount-1",
        json!({
            "target": "application.api",
            "mount_type": "volume",
            "mount_path": "/data",
            "volume_name": "api-data"
        }),
        &[],
        &["application.api"],
        protected,
    );
    state
}

fn plan_for(desired: &DesiredState, state: &StateFile, remote: &RemoteState) -> Plan {
    plan(
        desired,
        &StoredState::try_from_state(state).expect("state projects"),
        remote,
    )
}

fn mount_change(plan: &Plan) -> &dokploy_core::PlannedChange {
    plan.changes()
        .iter()
        .find(|change| change.address().kind() == ResourceKind::Mount)
        .unwrap_or_else(|| panic!("plan contains a Mount change: {:?}", plan.diagnostics()))
}

#[test]
fn mount_property_paths_are_stable_scoped_and_content_is_sensitive() {
    for (path, text) in [
        (PropertyPath::Target, "target"),
        (PropertyPath::MountType, "mount_type"),
        (PropertyPath::MountPath, "mount_path"),
        (PropertyPath::HostPath, "host_path"),
        (PropertyPath::VolumeName, "volume_name"),
        (PropertyPath::FilePath, "file_path"),
        (PropertyPath::FileContent, "content"),
    ] {
        assert_eq!(path.to_string(), text);
        assert_eq!(text.parse::<PropertyPath>().unwrap(), path);
        assert_eq!(path.is_sensitive(), text == "content");
    }

    let wrong_kind = DesiredState::try_new(
        digest(),
        BTreeMap::from([(
            address("application.api"),
            DesiredResource::new(BTreeMap::from([(PropertyPath::MountPath, text("/data"))]))
                .with_containment(Some(environment())),
        )]),
    )
    .expect_err("Mount paths are not Application properties");
    assert!(matches!(
        wrong_kind,
        DesiredStateError::InvalidPropertyPath { .. }
    ));
}

#[test]
fn desired_seam_validates_every_mount_value_and_content_shape() {
    let valid = || {
        let mut properties = volume_properties("application.api", "/data", "api-data");
        properties.insert(PropertyPath::FileContent, intent(1));
        properties
    };
    desired_with(Some(valid()), vec![]);

    for (path, invalid) in [
        (PropertyPath::Target, OwnedValue::Null),
        (PropertyPath::Target, text("project.platform")),
        (PropertyPath::Target, text("port.http")),
        (PropertyPath::Target, text("mount.other")),
        (PropertyPath::Target, text("not-an-address")),
        (PropertyPath::MountType, text("tmpfs")),
        (PropertyPath::MountType, OwnedValue::Null),
        (PropertyPath::MountPath, text("")),
        (PropertyPath::MountPath, text("/line\nbreak")),
        (PropertyPath::MountPath, OwnedValue::Null),
        (PropertyPath::VolumeName, OwnedValue::Null),
        (PropertyPath::VolumeName, OwnedValue::EmptyCollection),
        (PropertyPath::HostPath, OwnedValue::Value(value(json!(7)))),
        (PropertyPath::FileContent, OwnedValue::Null),
        (PropertyPath::FileContent, text(CONTENT_CANARY)),
        (PropertyPath::FileContent, OwnedValue::EmptyCollection),
    ] {
        let mut properties = valid();
        properties.insert(path, invalid);
        let error = DesiredState::try_new(
            digest(),
            BTreeMap::from([
                plain("project.platform", &[]),
                plain("environment.production", &[]),
                plain("application.api", &[]),
                (
                    address("mount.data"),
                    desired_resource(properties, &["application.api"]),
                ),
            ]),
        )
        .expect_err("invalid Mount values fail at the desired-state seam");
        assert!(matches!(
            error,
            DesiredStateError::InvalidPropertyValue { .. }
        ));
        assert!(!format!("{error:?}{error}").contains(CONTENT_CANARY));
    }
}

#[test]
fn stored_seam_rejects_malformed_mount_state_and_projects_content_as_a_receipt() {
    let mut state = base_state();
    state_resource(
        &mut state,
        "mount.data",
        "mount-1",
        json!({
            "target": "application.api",
            "mount_type": "file",
            "mount_path": "/etc/app.conf",
            "file_path": "app.conf"
        }),
        &["content"],
        &["application.api"],
        false,
    );
    let stored = StoredState::try_from_state(&state).expect("valid Mount state projects");
    assert!(matches!(
        stored.property(&address("mount.data"), &PropertyPath::FileContent),
        Some(OwnedValue::Sensitive(_))
    ));

    for managed in [
        json!({"target": null}),
        json!({"target": "project.platform"}),
        json!({"target": "application.api", "mount_type": "overlay"}),
        json!({"mount_path": ""}),
        json!({"volume_name": null}),
        json!({"mount_path": 5}),
        // The state layer stores a clear, since a clear holds no bytes. A Mount's content
        // is receipt-only, so the planner's stored seam refuses a managed `content`.
        json!({"content": null}),
    ] {
        let mut state = base_state();
        state_resource(
            &mut state,
            "mount.data",
            "mount-1",
            managed,
            &[],
            &["application.api"],
            false,
        );
        let error = StoredState::try_from_state(&state)
            .expect_err("malformed stored Mount values fail closed");
        assert!(matches!(
            error,
            StoredStateError::InvalidPropertyValue { .. }
        ));
    }

    let error = ManagedInputs::try_from_json(json!({"content": CONTENT_CANARY}))
        .expect_err("file content can never enter managed state");
    assert!(!error.to_string().contains(CONTENT_CANARY));
}

#[test]
fn remote_seam_rejects_invalid_mount_observations() {
    let observe = |path: PropertyPath, observation: PropertyObservation| {
        RemoteState::try_new(
            instance(),
            [(
                address("mount.data"),
                present("mount-1", BTreeMap::from([(path, observation)])),
            )],
        )
    };
    for (path, observation) in [
        (PropertyPath::Target, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Target,
            PropertyObservation::Known(value(json!("project.platform"))),
        ),
        (
            PropertyPath::MountType,
            PropertyObservation::Known(value(json!("tmpfs"))),
        ),
        (PropertyPath::MountPath, PropertyObservation::KnownAbsent),
        (
            PropertyPath::MountPath,
            PropertyObservation::Known(value(json!(""))),
        ),
        (
            PropertyPath::HostPath,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
        (
            PropertyPath::VolumeName,
            PropertyObservation::Known(value(json!(3))),
        ),
    ] {
        assert!(matches!(
            observe(path, observation),
            Err(RemoteStateError::InvalidPropertyObservation { .. })
        ));
    }

    for (path, observation) in [
        (PropertyPath::HostPath, PropertyObservation::KnownAbsent),
        (
            PropertyPath::FilePath,
            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned),
        ),
        (
            PropertyPath::FileContent,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
    ] {
        observe(path, observation).expect("source fields and write-only content are observable");
    }
}

#[test]
fn matching_mount_is_a_no_op_and_in_place_path_change_is_an_update() {
    let state = stored_with_mount(false);
    let observed = volume_observation("application.api", "/data", "api-data");

    let unchanged = plan_for(
        &desired_with(
            Some(volume_properties("application.api", "/data", "api-data")),
            vec![],
        ),
        &state,
        &remote(base_remote(Some(present("mount-1", observed.clone())))),
    );
    assert!(unchanged.changes().is_empty() && unchanged.applyable());

    let updated = plan_for(
        &desired_with(
            Some(volume_properties("application.api", "/updated", "api-data")),
            vec![],
        ),
        &state,
        &remote(base_remote(Some(present("mount-1", observed)))),
    );
    assert!(updated.applyable());
    assert_eq!(mount_change(&updated).kind(), ChangeKind::Update);
    assert_eq!(mount_change(&updated).replacement_order(), None);
}

#[test]
fn target_or_type_change_is_delete_before_create_replacement_unless_protected() {
    let state = stored_with_mount(false);
    let observed = volume_observation("application.api", "/data", "api-data");
    let worker = plain("application.worker", &[]);

    let mut state_with_worker = state.clone();
    state_resource(
        &mut state_with_worker,
        "application.worker",
        "application-2",
        json!({}),
        &[],
        &[],
        false,
    );
    let mut observations = base_remote(Some(present("mount-1", observed.clone())));
    observations.push((
        "application.worker",
        present("application-2", BTreeMap::new()),
    ));
    let retarget = plan_for(
        &desired_with(
            Some(volume_properties("application.worker", "/data", "api-data")),
            vec![worker],
        ),
        &state_with_worker,
        &remote(observations),
    );
    // The desired Mount dependency must follow its new target.
    assert_eq!(mount_change(&retarget).kind(), ChangeKind::Replace);
    assert_eq!(
        mount_change(&retarget).replacement_order(),
        Some(ReplacementOrder::DeleteBeforeCreate)
    );

    let mut bind = volume_properties("application.api", "/data", "api-data");
    bind.remove(&PropertyPath::VolumeName);
    bind.insert(PropertyPath::MountType, text("bind"));
    bind.insert(PropertyPath::HostPath, text("/srv/data"));
    let mut bind_observed = observed.clone();
    bind_observed.insert(PropertyPath::HostPath, PropertyObservation::KnownAbsent);
    let type_change = plan_for(
        &desired_with(Some(bind.clone()), vec![]),
        &state,
        &remote(base_remote(Some(present("mount-1", bind_observed.clone())))),
    );
    assert_eq!(mount_change(&type_change).kind(), ChangeKind::Replace);

    let protected = plan_for(
        &desired_with(Some(bind), vec![]),
        &stored_with_mount(true),
        &remote(base_remote(Some(present("mount-1", bind_observed)))),
    );
    assert!(!protected.applyable());
    assert!(
        protected
            .diagnostics()
            .iter()
            .any(|issue| issue.code() == PlanDiagnosticCode::ProtectedDelete)
    );
}

#[test]
fn creation_requires_target_type_and_path_and_is_ordered_after_its_target() {
    let empty_state = StateFile::new(Version::new(0, 1, 0), instance());
    let desired = desired_with(
        Some(volume_properties("application.api", "/data", "api-data")),
        vec![],
    );
    let plan_value = plan_for(
        &desired,
        &empty_state,
        &remote(vec![
            ("project.platform", RemoteObservation::Missing),
            ("environment.production", RemoteObservation::Missing),
            ("application.api", RemoteObservation::Missing),
            ("mount.data", RemoteObservation::Missing),
        ]),
    );
    let order = plan_value
        .changes()
        .iter()
        .map(|change| change.address().to_string())
        .collect::<Vec<_>>();
    assert!(
        order.iter().position(|item| item == "application.api")
            < order.iter().position(|item| item == "mount.data"),
        "{order:?}"
    );
    assert_eq!(mount_change(&plan_value).kind(), ChangeKind::Create);

    let mut incomplete = volume_properties("application.api", "/data", "api-data");
    incomplete.remove(&PropertyPath::MountType);
    let blocked = plan_for(
        &desired_with(Some(incomplete), vec![]),
        &base_state(),
        &remote(base_remote(Some(RemoteObservation::Missing))),
    );
    assert!(blocked.changes().is_empty());
    assert_eq!(
        blocked.diagnostics()[0].code(),
        PlanDiagnosticCode::MissingCreateProperty
    );
}

#[test]
fn mount_deletion_is_ordered_before_target_deletion_through_stored_dependencies() {
    let state = stored_with_mount(false);
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from_iter([
            plain("project.platform", &[]),
            plain("environment.production", &[]),
        ]),
    )
    .expect("desired state is valid")
    .with_removals(Vec::<RemovalDirective>::new());
    let observed = volume_observation("application.api", "/data", "api-data");

    let removal = plan_for(
        &desired,
        &state,
        &remote(base_remote(Some(present("mount-1", observed)))),
    );

    let order = removal
        .changes()
        .iter()
        .map(|change| (change.address().to_string(), change.kind()))
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            ("mount.data".to_owned(), ChangeKind::Delete),
            ("application.api".to_owned(), ChangeKind::Delete)
        ]
    );
}

#[test]
fn content_rotation_is_a_write_only_update_and_receipts_stay_opaque() {
    let mut state = base_state();
    state_resource(
        &mut state,
        "mount.data",
        "mount-1",
        json!({
            "target": "application.api",
            "mount_type": "file",
            "mount_path": "/etc/app.conf",
            "file_path": "app.conf"
        }),
        &["content"],
        &["application.api"],
        false,
    );
    let file_properties = |mac| {
        BTreeMap::from([
            (PropertyPath::Target, text("application.api")),
            (PropertyPath::MountType, text("file")),
            (PropertyPath::MountPath, text("/etc/app.conf")),
            (PropertyPath::FilePath, text("app.conf")),
            (PropertyPath::FileContent, intent(mac)),
        ])
    };
    let observed = BTreeMap::from([
        (
            PropertyPath::Target,
            PropertyObservation::Known(value(json!("application.api"))),
        ),
        (
            PropertyPath::MountType,
            PropertyObservation::Known(value(json!("file"))),
        ),
        (
            PropertyPath::MountPath,
            PropertyObservation::Known(value(json!("/etc/app.conf"))),
        ),
        (
            PropertyPath::FilePath,
            PropertyObservation::Known(value(json!("app.conf"))),
        ),
        (
            PropertyPath::FileContent,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
    ]);

    let stable = plan_for(
        &desired_with(Some(file_properties(0xa5)), vec![]),
        &state,
        &remote(base_remote(Some(present("mount-1", observed.clone())))),
    );
    assert!(stable.changes().is_empty() && stable.applyable());

    let rotated = plan_for(
        &desired_with(Some(file_properties(0xb6)), vec![]),
        &state,
        &remote(base_remote(Some(present("mount-1", observed)))),
    );
    assert_eq!(mount_change(&rotated).kind(), ChangeKind::Update);
    let json = String::from_utf8(rotated.to_json_bytes()).unwrap();
    assert!(!json.contains(CONTENT_CANARY));

    let checkpoint = mount_change(&rotated)
        .checkpoint()
        .present()
        .expect("update has a checkpoint");
    let materialized = checkpoint
        .materialize(&address("mount.data"), RemoteId::new("mount-1").unwrap())
        .expect("checkpoint materializes");
    assert!(
        !materialized
            .last_applied()
            .as_json()
            .as_object()
            .unwrap()
            .contains_key("content")
    );
    assert_eq!(materialized.sensitive_inputs().paths().count(), 1);
    assert_eq!(
        materialized.dependencies(),
        &[address("application.api")],
        "the target remains an inferred dependency"
    );
    assert_eq!(materialized.containment(), Some(&environment()));
}
