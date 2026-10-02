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

const COMMAND_CANARY: &str = "schedule-command-canary-never-leak";
const SCRIPT_CANARY: &str = "schedule-script-canary-never-leak";

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

fn boolean(flag: bool) -> OwnedValue {
    OwnedValue::Value(value(json!(flag)))
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

fn schedule_contract() -> MutationContract {
    let in_place = PropertyMutation::new(MutationMode::InPlace, MutationMode::InPlace);
    let set_only = PropertyMutation::new(MutationMode::InPlace, MutationMode::Unsupported);
    let replace = PropertyMutation::new(MutationMode::Replace, MutationMode::Unsupported);
    MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
        .requiring(PropertyPath::Target)
        .requiring(PropertyPath::Name)
        .requiring(PropertyPath::CronExpression)
        .requiring(PropertyPath::ShellType)
        .requiring(PropertyPath::Enabled)
        .requiring(PropertyPath::Command)
        .allowing_on_create(PropertyPath::ServiceName)
        .allowing_on_create(PropertyPath::Description)
        .allowing_on_create(PropertyPath::Timezone)
        .allowing_on_create(PropertyPath::Script)
        .with_property(PropertyPath::Target, replace)
        .with_property(PropertyPath::ServiceName, replace)
        .with_property(PropertyPath::Name, in_place)
        .with_property(PropertyPath::CronExpression, in_place)
        .with_property(PropertyPath::ShellType, in_place)
        .with_property(PropertyPath::Enabled, in_place)
        .with_property(PropertyPath::Description, set_only)
        .with_property(PropertyPath::Timezone, set_only)
        .with_property(PropertyPath::Command, set_only)
        .with_property(PropertyPath::Script, set_only)
        .with_containment(MutationMode::StateOnly)
}

fn schedule_properties(target: &str, cron: &str) -> BTreeMap<PropertyPath, OwnedValue> {
    BTreeMap::from([
        (PropertyPath::Target, text(target)),
        (PropertyPath::Name, text("nightly")),
        (PropertyPath::CronExpression, text(cron)),
        (PropertyPath::ShellType, text("bash")),
        (PropertyPath::Enabled, boolean(false)),
        (PropertyPath::Command, intent(0xa5)),
    ])
}

fn schedule_observation(target: &str, cron: &str) -> BTreeMap<PropertyPath, PropertyObservation> {
    BTreeMap::from([
        (
            PropertyPath::Target,
            PropertyObservation::Known(value(json!(target))),
        ),
        (
            PropertyPath::Name,
            PropertyObservation::Known(value(json!("nightly"))),
        ),
        (
            PropertyPath::CronExpression,
            PropertyObservation::Known(value(json!(cron))),
        ),
        (
            PropertyPath::ShellType,
            PropertyObservation::Known(value(json!("bash"))),
        ),
        (
            PropertyPath::Enabled,
            PropertyObservation::Known(value(json!(false))),
        ),
        (
            PropertyPath::Command,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
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
    for (name, id, managed) in [
        ("project.platform", "project-1", json!({})),
        ("environment.production", "environment-1", json!({})),
        ("application.api", "application-1", json!({})),
    ] {
        state_resource(&mut state, name, id, managed, &[], &[], false);
    }
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
        ResourceKind::Schedule => schedule_contract(),
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

fn base_remote(schedule: Option<RemoteObservation>) -> Vec<(&'static str, RemoteObservation)> {
    let mut observations = vec![
        ("project.platform", present("project-1", BTreeMap::new())),
        (
            "environment.production",
            present("environment-1", BTreeMap::new()),
        ),
        ("application.api", present("application-1", BTreeMap::new())),
    ];
    if let Some(schedule) = schedule {
        observations.push(("schedule.nightly", schedule));
    }
    observations
}

fn desired_with(
    schedule: Option<BTreeMap<PropertyPath, OwnedValue>>,
    extra: Vec<(ResourceAddress, DesiredResource)>,
) -> DesiredState {
    let mut resources = BTreeMap::from_iter([
        plain("project.platform", &[]),
        plain("environment.production", &[]),
        plain("application.api", &[]),
    ]);
    resources.extend(extra);
    if let Some(properties) = schedule {
        let target = if properties.get(&PropertyPath::Target) == Some(&text("application.worker")) {
            "application.worker"
        } else {
            "application.api"
        };
        resources.insert(
            address("schedule.nightly"),
            desired_resource(properties, &[target]),
        );
    }
    DesiredState::try_new(digest(), resources).expect("desired state is valid")
}

fn stored_with_schedule(protected: bool) -> StateFile {
    let mut state = base_state();
    state_resource(
        &mut state,
        "schedule.nightly",
        "schedule-1",
        json!({
            "target": "application.api",
            "name": "nightly",
            "cron_expression": "0 3 * * *",
            "shell_type": "bash",
            "enabled": false
        }),
        &["command"],
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

fn schedule_change(plan: &Plan) -> &dokploy_core::PlannedChange {
    plan.changes()
        .iter()
        .find(|change| change.address().kind() == ResourceKind::Schedule)
        .unwrap_or_else(|| panic!("plan contains a Schedule change: {:?}", plan.diagnostics()))
}

#[test]
fn schedule_property_paths_are_stable_scoped_and_executables_are_sensitive() {
    for (path, text) in [
        (PropertyPath::ServiceName, "service_name"),
        (PropertyPath::Name, "name"),
        (PropertyPath::CronExpression, "cron_expression"),
        (PropertyPath::ShellType, "shell_type"),
        (PropertyPath::Enabled, "enabled"),
        (PropertyPath::Timezone, "timezone"),
        (PropertyPath::Command, "command"),
        (PropertyPath::Script, "script"),
    ] {
        assert_eq!(path.to_string(), text);
        assert_eq!(text.parse::<PropertyPath>().unwrap(), path);
        assert_eq!(path.is_sensitive(), matches!(text, "command" | "script"));
    }

    for property in [PropertyPath::CronExpression, PropertyPath::Command] {
        let wrong_kind = DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                address("application.api"),
                DesiredResource::new(BTreeMap::from([(property, text("x"))]))
                    .with_containment(Some(environment())),
            )]),
        )
        .expect_err("Schedule paths are not Application properties");
        assert!(matches!(
            wrong_kind,
            DesiredStateError::InvalidPropertyPath { .. }
        ));
    }
}

#[test]
fn sensitive_paths_cannot_be_ignored() {
    for path in [PropertyPath::Command, PropertyPath::Script] {
        let error = DesiredState::try_new(
            digest(),
            BTreeMap::from([
                plain("project.platform", &[]),
                plain("environment.production", &[]),
                plain("application.api", &[]),
                (
                    address("schedule.nightly"),
                    desired_resource(
                        schedule_properties("application.api", "0 3 * * *"),
                        &["application.api"],
                    )
                    .with_ignored_changes(vec![path]),
                ),
            ]),
        )
        .expect_err("sensitive paths are never ignorable");
        assert!(matches!(
            error,
            DesiredStateError::InvalidIgnoredProperty { .. }
        ));
    }
}

#[test]
fn desired_seam_validates_every_schedule_value_and_executable_shape() {
    let valid = || {
        let mut properties = schedule_properties("application.api", "0 3 * * *");
        properties.insert(PropertyPath::Script, intent(2));
        properties.insert(PropertyPath::Description, text("nightly cleanup"));
        properties.insert(PropertyPath::Timezone, text("UTC"));
        properties
    };
    desired_with(Some(valid()), vec![]);

    let mut compose = valid();
    compose.insert(PropertyPath::Target, text("compose.stack"));
    compose.insert(PropertyPath::ServiceName, text("worker"));
    DesiredState::try_new(
        digest(),
        BTreeMap::from([
            plain("project.platform", &[]),
            plain("environment.production", &[]),
            plain("compose.stack", &[]),
            (
                address("schedule.nightly"),
                desired_resource(compose, &["compose.stack"]),
            ),
        ]),
    )
    .expect("a Compose target is in the closed union");

    for (path, invalid) in [
        (PropertyPath::Target, OwnedValue::Null),
        (PropertyPath::Target, text("postgres.main")),
        (PropertyPath::Target, text("project.platform")),
        (PropertyPath::Target, text("schedule.other")),
        (PropertyPath::Target, text("not-an-address")),
        (PropertyPath::Name, text("")),
        (PropertyPath::Name, text(" padded")),
        (PropertyPath::Name, OwnedValue::Null),
        (PropertyPath::CronExpression, text("")),
        (PropertyPath::CronExpression, text("0 3 *\n* *")),
        (PropertyPath::CronExpression, OwnedValue::Null),
        (PropertyPath::ShellType, text("zsh")),
        (PropertyPath::ShellType, OwnedValue::Null),
        (PropertyPath::Enabled, text("false")),
        (PropertyPath::Enabled, OwnedValue::Null),
        (PropertyPath::ServiceName, text("")),
        (PropertyPath::ServiceName, OwnedValue::Null),
        (PropertyPath::Description, OwnedValue::Null),
        (PropertyPath::Description, text("")),
        (PropertyPath::Timezone, OwnedValue::Null),
        (PropertyPath::Timezone, OwnedValue::Value(value(json!(7)))),
        (PropertyPath::Command, OwnedValue::Null),
        (PropertyPath::Command, text(COMMAND_CANARY)),
        (PropertyPath::Command, OwnedValue::EmptyCollection),
        (PropertyPath::Script, OwnedValue::Null),
        (PropertyPath::Script, text(SCRIPT_CANARY)),
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
                    address("schedule.nightly"),
                    desired_resource(properties, &["application.api"]),
                ),
            ]),
        )
        .expect_err("invalid Schedule values fail at the desired-state seam");
        assert!(matches!(
            error,
            DesiredStateError::InvalidPropertyValue { .. }
        ));
        let rendered = format!("{error:?}{error}");
        assert!(!rendered.contains(COMMAND_CANARY));
        assert!(!rendered.contains(SCRIPT_CANARY));
    }
}

#[test]
fn stored_seam_rejects_malformed_schedule_state_and_projects_executables_as_receipts() {
    let mut state = base_state();
    state_resource(
        &mut state,
        "schedule.nightly",
        "schedule-1",
        json!({
            "target": "application.api",
            "name": "nightly",
            "cron_expression": "0 3 * * *",
            "shell_type": "sh",
            "enabled": true
        }),
        &["command", "script"],
        &["application.api"],
        false,
    );
    let stored = StoredState::try_from_state(&state).expect("valid Schedule state projects");
    for path in [PropertyPath::Command, PropertyPath::Script] {
        assert!(matches!(
            stored.property(&address("schedule.nightly"), &path),
            Some(OwnedValue::Sensitive(_))
        ));
    }

    for managed in [
        json!({"target": null}),
        json!({"target": "postgres.main"}),
        json!({"target": "application.api", "shell_type": "zsh"}),
        json!({"name": ""}),
        json!({"name": " padded"}),
        json!({"cron_expression": ""}),
        json!({"enabled": "true"}),
        json!({"enabled": null}),
        json!({"service_name": ""}),
        json!({"description": null}),
        json!({"timezone": 5}),
    ] {
        let mut state = base_state();
        state_resource(
            &mut state,
            "schedule.nightly",
            "schedule-1",
            managed,
            &[],
            &["application.api"],
            false,
        );
        let error = StoredState::try_from_state(&state)
            .expect_err("malformed stored Schedule values fail closed");
        assert!(matches!(
            error,
            StoredStateError::InvalidPropertyValue { .. }
        ));
    }

    for managed in [
        json!({"command": COMMAND_CANARY}),
        json!({"script": SCRIPT_CANARY}),
        json!({"command": null}),
    ] {
        let error = ManagedInputs::try_from_json(managed)
            .expect_err("executable text can never enter managed state");
        let rendered = format!("{error:?}{error}");
        assert!(!rendered.contains(COMMAND_CANARY));
        assert!(!rendered.contains(SCRIPT_CANARY));
    }
}

#[test]
fn remote_seam_rejects_invalid_schedule_observations() {
    let observe = |path: PropertyPath, observation: PropertyObservation| {
        RemoteState::try_new(
            instance(),
            [(
                address("schedule.nightly"),
                present("schedule-1", BTreeMap::from([(path, observation)])),
            )],
        )
    };
    for (path, observation) in [
        (PropertyPath::Target, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Target,
            PropertyObservation::Known(value(json!("postgres.main"))),
        ),
        (PropertyPath::Name, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Name,
            PropertyObservation::Known(value(json!(""))),
        ),
        (
            PropertyPath::CronExpression,
            PropertyObservation::Known(value(json!(4))),
        ),
        (
            PropertyPath::ShellType,
            PropertyObservation::Known(value(json!("zsh"))),
        ),
        (PropertyPath::Enabled, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Enabled,
            PropertyObservation::Known(value(json!("false"))),
        ),
        (
            PropertyPath::Timezone,
            PropertyObservation::Known(value(json!(""))),
        ),
        (
            PropertyPath::ServiceName,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
        (
            PropertyPath::Description,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
        (
            PropertyPath::Command,
            PropertyObservation::Known(value(json!(COMMAND_CANARY))),
        ),
        (
            PropertyPath::Script,
            PropertyObservation::Known(value(json!(SCRIPT_CANARY))),
        ),
    ] {
        assert!(matches!(
            observe(path, observation),
            Err(RemoteStateError::InvalidPropertyObservation { .. })
        ));
    }

    for (path, observation) in [
        (PropertyPath::ServiceName, PropertyObservation::KnownAbsent),
        (PropertyPath::Timezone, PropertyObservation::KnownAbsent),
        (PropertyPath::Description, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Command,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
        (PropertyPath::Script, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Script,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
    ] {
        observe(path, observation).expect("optional fields and write-only text are observable");
    }
}

#[test]
fn matching_schedule_is_a_no_op_and_in_place_cron_or_enabled_change_is_an_update() {
    let state = stored_with_schedule(false);
    let observed = schedule_observation("application.api", "0 3 * * *");

    let unchanged = plan_for(
        &desired_with(
            Some(schedule_properties("application.api", "0 3 * * *")),
            vec![],
        ),
        &state,
        &remote(base_remote(Some(present("schedule-1", observed.clone())))),
    );
    assert!(unchanged.changes().is_empty() && unchanged.applyable());

    let updated = plan_for(
        &desired_with(
            Some(schedule_properties("application.api", "5 4 * * 1")),
            vec![],
        ),
        &state,
        &remote(base_remote(Some(present("schedule-1", observed.clone())))),
    );
    assert!(updated.applyable());
    assert_eq!(schedule_change(&updated).kind(), ChangeKind::Update);
    assert_eq!(schedule_change(&updated).replacement_order(), None);

    let mut enabled = schedule_properties("application.api", "0 3 * * *");
    enabled.insert(PropertyPath::Enabled, boolean(true));
    let toggled = plan_for(
        &desired_with(Some(enabled), vec![]),
        &state,
        &remote(base_remote(Some(present("schedule-1", observed)))),
    );
    assert_eq!(schedule_change(&toggled).kind(), ChangeKind::Update);
}

#[test]
fn target_or_service_change_is_delete_before_create_replacement_unless_protected() {
    let state = stored_with_schedule(false);
    let observed = schedule_observation("application.api", "0 3 * * *");
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
    let mut observations = base_remote(Some(present("schedule-1", observed.clone())));
    observations.push((
        "application.worker",
        present("application-2", BTreeMap::new()),
    ));
    let retarget = plan_for(
        &desired_with(
            Some(schedule_properties("application.worker", "0 3 * * *")),
            vec![worker],
        ),
        &state_with_worker,
        &remote(observations),
    );
    // The desired Schedule dependency must follow its new target.
    assert_eq!(schedule_change(&retarget).kind(), ChangeKind::Replace);
    assert_eq!(
        schedule_change(&retarget).replacement_order(),
        Some(ReplacementOrder::DeleteBeforeCreate)
    );

    let mut with_service = schedule_properties("application.api", "0 3 * * *");
    with_service.insert(PropertyPath::ServiceName, text("worker"));
    let mut service_observed = observed.clone();
    service_observed.insert(PropertyPath::ServiceName, PropertyObservation::KnownAbsent);
    let service_change = plan_for(
        &desired_with(Some(with_service.clone()), vec![]),
        &state,
        &remote(base_remote(Some(present(
            "schedule-1",
            service_observed.clone(),
        )))),
    );
    assert_eq!(schedule_change(&service_change).kind(), ChangeKind::Replace);

    let protected = plan_for(
        &desired_with(Some(with_service), vec![]),
        &stored_with_schedule(true),
        &remote(base_remote(Some(present("schedule-1", service_observed)))),
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
fn creation_requires_target_schedule_fields_and_command_and_is_ordered_after_its_target() {
    let empty_state = StateFile::new(Version::new(0, 1, 0), instance());
    let desired = desired_with(
        Some(schedule_properties("application.api", "0 3 * * *")),
        vec![],
    );
    let plan_value = plan_for(
        &desired,
        &empty_state,
        &remote(vec![
            ("project.platform", RemoteObservation::Missing),
            ("environment.production", RemoteObservation::Missing),
            ("application.api", RemoteObservation::Missing),
            ("schedule.nightly", RemoteObservation::Missing),
        ]),
    );
    let order = plan_value
        .changes()
        .iter()
        .map(|change| change.address().to_string())
        .collect::<Vec<_>>();
    assert!(
        order.iter().position(|item| item == "application.api")
            < order.iter().position(|item| item == "schedule.nightly"),
        "{order:?}"
    );
    assert_eq!(schedule_change(&plan_value).kind(), ChangeKind::Create);

    for required in [
        PropertyPath::Target,
        PropertyPath::Name,
        PropertyPath::CronExpression,
        PropertyPath::ShellType,
        PropertyPath::Enabled,
        PropertyPath::Command,
    ] {
        let mut incomplete = schedule_properties("application.api", "0 3 * * *");
        incomplete.remove(&required);
        let blocked = plan_for(
            &desired_with(Some(incomplete), vec![]),
            &base_state(),
            &remote(base_remote(Some(RemoteObservation::Missing))),
        );
        assert!(blocked.changes().is_empty(), "{required}");
        assert_eq!(
            blocked.diagnostics()[0].code(),
            PlanDiagnosticCode::MissingCreateProperty
        );
    }
}

#[test]
fn schedule_deletion_is_ordered_before_target_deletion_through_stored_dependencies() {
    let state = stored_with_schedule(false);
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from_iter([
            plain("project.platform", &[]),
            plain("environment.production", &[]),
        ]),
    )
    .expect("desired state is valid")
    .with_removals(Vec::<RemovalDirective>::new());
    let observed = schedule_observation("application.api", "0 3 * * *");

    let removal = plan_for(
        &desired,
        &state,
        &remote(base_remote(Some(present("schedule-1", observed)))),
    );

    let order = removal
        .changes()
        .iter()
        .map(|change| (change.address().to_string(), change.kind()))
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            ("schedule.nightly".to_owned(), ChangeKind::Delete),
            ("application.api".to_owned(), ChangeKind::Delete)
        ]
    );
}

#[test]
fn command_rotation_is_a_write_only_update_and_receipts_stay_opaque() {
    let state = stored_with_schedule(false);
    let observed = schedule_observation("application.api", "0 3 * * *");
    let rotated_properties = {
        let mut properties = schedule_properties("application.api", "0 3 * * *");
        properties.insert(PropertyPath::Command, intent(0xb6));
        properties
    };

    let stable = plan_for(
        &desired_with(
            Some(schedule_properties("application.api", "0 3 * * *")),
            vec![],
        ),
        &state,
        &remote(base_remote(Some(present("schedule-1", observed.clone())))),
    );
    assert!(stable.changes().is_empty() && stable.applyable());

    let rotated = plan_for(
        &desired_with(Some(rotated_properties), vec![]),
        &state,
        &remote(base_remote(Some(present("schedule-1", observed)))),
    );
    assert_eq!(schedule_change(&rotated).kind(), ChangeKind::Update);
    let json = String::from_utf8(rotated.to_json_bytes()).unwrap();
    assert!(!json.contains(COMMAND_CANARY));
    assert!(!json.contains(SCRIPT_CANARY));

    let checkpoint = schedule_change(&rotated)
        .checkpoint()
        .present()
        .expect("update has a checkpoint");
    let materialized = checkpoint
        .materialize(
            &address("schedule.nightly"),
            RemoteId::new("schedule-1").unwrap(),
        )
        .expect("checkpoint materializes");
    let managed = materialized.last_applied().as_json().as_object().unwrap();
    assert!(!managed.contains_key("command"));
    assert!(!managed.contains_key("script"));
    assert_eq!(materialized.sensitive_inputs().paths().count(), 1);
    assert_eq!(
        materialized.dependencies(),
        &[address("application.api")],
        "the target remains an inferred dependency"
    );
    assert_eq!(materialized.containment(), Some(&environment()));
}

#[test]
fn a_script_missing_remotely_is_drift_that_plans_a_write_only_update() {
    let mut state = base_state();
    state_resource(
        &mut state,
        "schedule.nightly",
        "schedule-1",
        json!({
            "target": "application.api",
            "name": "nightly",
            "cron_expression": "0 3 * * *",
            "shell_type": "bash",
            "enabled": false
        }),
        &["command", "script"],
        &["application.api"],
        false,
    );
    let mut properties = schedule_properties("application.api", "0 3 * * *");
    properties.insert(PropertyPath::Script, intent(0xa5));
    let mut observed = schedule_observation("application.api", "0 3 * * *");
    observed.insert(
        PropertyPath::Script,
        PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
    );
    let stable = plan_for(
        &desired_with(Some(properties.clone()), vec![]),
        &state,
        &remote(base_remote(Some(present("schedule-1", observed.clone())))),
    );
    assert!(stable.changes().is_empty());

    observed.insert(PropertyPath::Script, PropertyObservation::KnownAbsent);
    let drifted = plan_for(
        &desired_with(Some(properties), vec![]),
        &state,
        &remote(base_remote(Some(present("schedule-1", observed)))),
    );
    assert_eq!(schedule_change(&drifted).kind(), ChangeKind::Update);
}
