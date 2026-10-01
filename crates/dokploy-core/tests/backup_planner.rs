use std::collections::BTreeMap;

use dokploy_core::{
    ChangeKind, ComparableValue, ConfigDigest, DesiredResource, DesiredState, DesiredStateError,
    ExternalResolution, ExternalSelectorFailure, MutationContract, MutationMode, OwnedValue, Plan,
    PlanDiagnosticCode, PropertyMutation, PropertyObservation, PropertyPath, PropertyUnknownReason,
    RemoteObservation, RemoteResource, RemoteState, RemoteStateError, ReplacementOrder,
    StoredState, StoredStateError, plan,
};
use dokploy_state::{
    InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceState,
    SensitiveInputs, StateFile,
};
use semver::Version;
use serde_json::json;

const DESTINATION_CANARY: &str = "destination-name-canary-never-leak";
const IDENTITY_CANARY: &str = "destination-identity-canary-never-leak";

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

fn owned(json_value: serde_json::Value) -> OwnedValue {
    OwnedValue::Value(value(json_value))
}

fn environment() -> ResourceAddress {
    address("environment.production")
}

fn backup_contract() -> MutationContract {
    let set_only = PropertyMutation::new(MutationMode::InPlace, MutationMode::Unsupported);
    let in_place = PropertyMutation::new(MutationMode::InPlace, MutationMode::InPlace);
    let replace = PropertyMutation::new(MutationMode::Replace, MutationMode::Unsupported);
    MutationContract::deny_all(ReplacementOrder::DeleteBeforeCreate)
        .requiring(PropertyPath::Target)
        .requiring(PropertyPath::Destination)
        .requiring(PropertyPath::Schedule)
        .requiring(PropertyPath::Prefix)
        .requiring(PropertyPath::Database)
        .requiring(PropertyPath::Enabled)
        .requiring(PropertyPath::IncludeEncryptionKey)
        .allowing_on_create(PropertyPath::KeepLatest)
        .with_property(PropertyPath::Target, replace)
        .with_property(PropertyPath::Destination, set_only)
        .with_property(PropertyPath::Schedule, set_only)
        .with_property(PropertyPath::Prefix, set_only)
        .with_property(PropertyPath::Database, set_only)
        .with_property(PropertyPath::Enabled, set_only)
        .with_property(PropertyPath::KeepLatest, in_place)
        .with_property(PropertyPath::IncludeEncryptionKey, set_only)
        .with_containment(MutationMode::StateOnly)
}

fn properties(target: &str, destination: &str) -> BTreeMap<PropertyPath, OwnedValue> {
    BTreeMap::from([
        (PropertyPath::Target, owned(json!(target))),
        (
            PropertyPath::Destination,
            owned(json!({ "name": destination })),
        ),
        (PropertyPath::Schedule, owned(json!("0 3 * * *"))),
        (PropertyPath::Prefix, owned(json!("nightly"))),
        (PropertyPath::Database, owned(json!("app"))),
        (PropertyPath::Enabled, owned(json!(false))),
        (PropertyPath::KeepLatest, owned(json!(7))),
        (PropertyPath::IncludeEncryptionKey, owned(json!(false))),
    ])
}

fn observation(target: &str, destination: &str) -> BTreeMap<PropertyPath, PropertyObservation> {
    BTreeMap::from([
        (
            PropertyPath::Target,
            PropertyObservation::Known(value(json!(target))),
        ),
        (
            PropertyPath::Destination,
            PropertyObservation::Known(value(json!({ "name": destination }))),
        ),
        (
            PropertyPath::Schedule,
            PropertyObservation::Known(value(json!("0 3 * * *"))),
        ),
        (
            PropertyPath::Prefix,
            PropertyObservation::Known(value(json!("nightly"))),
        ),
        (
            PropertyPath::Database,
            PropertyObservation::Known(value(json!("app"))),
        ),
        (
            PropertyPath::Enabled,
            PropertyObservation::Known(value(json!(false))),
        ),
        (
            PropertyPath::KeepLatest,
            PropertyObservation::Known(value(json!(7))),
        ),
        (
            PropertyPath::IncludeEncryptionKey,
            PropertyObservation::Known(value(json!(false))),
        ),
    ])
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

fn desired_resource(
    properties: BTreeMap<PropertyPath, OwnedValue>,
    target: &str,
) -> DesiredResource {
    DesiredResource::new(properties)
        .with_containment(Some(environment()))
        .with_dependencies(vec![address(target)])
}

fn state_resource(
    state: &mut StateFile,
    address_text: &str,
    remote_id: &str,
    managed: serde_json::Value,
    dependencies: &[&str],
    protected: bool,
) {
    let address = address(address_text);
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
                SensitiveInputs::default(),
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
        ("postgres.main", "postgres-1", json!({})),
        ("postgres.other", "postgres-2", json!({})),
    ] {
        state_resource(&mut state, name, id, managed, &[], false);
    }
    state
}

fn stored_backup(protected: bool) -> StateFile {
    let mut state = base_state();
    state_resource(
        &mut state,
        "backup.nightly",
        "backup-1",
        json!({
            "target": "postgres.main",
            "destination": { "name": "offsite" },
            "schedule": "0 3 * * *",
            "prefix": "nightly",
            "database": "app",
            "enabled": false,
            "keep_latest": 7,
            "include_encryption_key": false
        }),
        &["postgres.main"],
        protected,
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

fn remote_with(
    backup: Option<RemoteObservation>,
    resolutions: Vec<ExternalResolution>,
) -> RemoteState {
    let mut observations = vec![
        ("project.platform", present("project-1", BTreeMap::new())),
        (
            "environment.production",
            present("environment-1", BTreeMap::new()),
        ),
        ("postgres.main", present("postgres-1", BTreeMap::new())),
        ("postgres.other", present("postgres-2", BTreeMap::new())),
    ];
    let has_backup = backup.is_some();
    if let Some(backup) = backup {
        observations.push(("backup.nightly", backup));
    }
    let observations = observations
        .into_iter()
        .map(|(name, observation)| (address(name), observation))
        .collect::<Vec<_>>();
    let contracts = observations
        .iter()
        .map(|(address, _)| {
            let contract = if address.kind() == ResourceKind::Backup {
                backup_contract()
            } else {
                MutationContract::permissive()
            };
            (address.clone(), contract)
        })
        .collect::<Vec<_>>();
    let mut remote = RemoteState::try_new_with_contracts(instance(), observations, contracts)
        .expect("remote snapshot is valid");
    if has_backup && !resolutions.is_empty() {
        remote = remote
            .with_external_resolutions(resolutions.into_iter().map(|resolution| {
                (
                    (address("backup.nightly"), PropertyPath::Destination),
                    resolution,
                )
            }))
            .expect("resolution attaches");
    }
    remote
}

fn resolved() -> Vec<ExternalResolution> {
    vec![ExternalResolution::Resolved(
        RemoteId::new("destination-1").unwrap(),
    )]
}

fn desired_with(properties: Option<BTreeMap<PropertyPath, OwnedValue>>) -> DesiredState {
    desired_for("postgres.main", properties)
}

fn desired_for(
    target: &str,
    properties: Option<BTreeMap<PropertyPath, OwnedValue>>,
) -> DesiredState {
    let mut resources = BTreeMap::from_iter([
        plain("project.platform", &[]),
        plain("environment.production", &[]),
        plain("postgres.main", &[]),
        plain("postgres.other", &[]),
    ]);
    if let Some(properties) = properties {
        resources.insert(
            address("backup.nightly"),
            desired_resource(properties, target),
        );
    }
    DesiredState::try_new(digest(), resources).expect("desired state is valid")
}

fn plan_for(desired: &DesiredState, state: &StateFile, remote: &RemoteState) -> Plan {
    plan(
        desired,
        &StoredState::try_from_state(state).expect("state projects"),
        remote,
    )
}

fn backup_change(plan: &Plan) -> &dokploy_core::PlannedChange {
    plan.changes()
        .iter()
        .find(|change| change.address().kind() == ResourceKind::Backup)
        .unwrap_or_else(|| panic!("plan contains a Backup change: {:?}", plan.diagnostics()))
}

#[test]
fn backup_property_paths_are_stable_scoped_and_destination_is_an_external_selector() {
    for (path, text) in [
        (PropertyPath::Destination, "destination"),
        (PropertyPath::Schedule, "schedule"),
        (PropertyPath::Prefix, "prefix"),
        (PropertyPath::Enabled, "enabled"),
        (PropertyPath::KeepLatest, "keep_latest"),
        (PropertyPath::IncludeEncryptionKey, "include_encryption_key"),
    ] {
        assert_eq!(path.to_string(), text);
        assert_eq!(text.parse::<PropertyPath>().unwrap(), path);
        assert!(!path.is_sensitive());
        assert_eq!(path.is_external_selector(), text == "destination");
    }

    for path in [PropertyPath::Schedule, PropertyPath::Destination] {
        let wrong_kind = DesiredState::try_new(
            digest(),
            BTreeMap::from([(
                address("postgres.main"),
                DesiredResource::new(BTreeMap::from([(path, owned(json!("x")))]))
                    .with_containment(Some(environment())),
            )]),
        )
        .expect_err("Backup paths are not database properties");
        assert!(matches!(
            wrong_kind,
            DesiredStateError::InvalidPropertyPath { .. }
        ));
    }
}

#[test]
fn desired_seam_validates_every_backup_value() {
    desired_with(Some(properties("postgres.main", "offsite")));

    let mut cleared = properties("postgres.main", "offsite");
    cleared.insert(PropertyPath::KeepLatest, OwnedValue::Null);
    desired_with(Some(cleared));

    for (path, invalid) in [
        (PropertyPath::Target, OwnedValue::Null),
        (PropertyPath::Target, owned(json!("compose.stack"))),
        (PropertyPath::Target, owned(json!("redis.cache"))),
        (PropertyPath::Target, owned(json!("application.api"))),
        (PropertyPath::Target, owned(json!("backup.other"))),
        (PropertyPath::Target, owned(json!("not-an-address"))),
        (PropertyPath::Destination, OwnedValue::Null),
        (PropertyPath::Destination, owned(json!({ "local": true }))),
        (PropertyPath::Destination, owned(json!({ "id": "raw" }))),
        (PropertyPath::Destination, owned(json!({ "name": "" }))),
        (PropertyPath::Destination, owned(json!({ "name": " pad" }))),
        (PropertyPath::Destination, owned(json!("offsite"))),
        (PropertyPath::Destination, OwnedValue::EmptyCollection),
        (PropertyPath::Schedule, owned(json!(""))),
        (PropertyPath::Schedule, owned(json!("@daily"))),
        (PropertyPath::Schedule, owned(json!("0 3 * *"))),
        (PropertyPath::Schedule, owned(json!(5))),
        (PropertyPath::Prefix, owned(json!(""))),
        (PropertyPath::Prefix, owned(json!("a\nb"))),
        (PropertyPath::Database, owned(json!(""))),
        (PropertyPath::Database, OwnedValue::Null),
        (PropertyPath::Enabled, OwnedValue::Null),
        (PropertyPath::Enabled, owned(json!("yes"))),
        (PropertyPath::IncludeEncryptionKey, owned(json!(1))),
        (PropertyPath::KeepLatest, owned(json!(0))),
        (PropertyPath::KeepLatest, owned(json!("7"))),
        (PropertyPath::KeepLatest, owned(json!(-1))),
        (PropertyPath::KeepLatest, OwnedValue::EmptyCollection),
    ] {
        let mut candidate = properties("postgres.main", "offsite");
        candidate.insert(path.clone(), invalid);
        let error = DesiredState::try_new(
            digest(),
            BTreeMap::from([
                plain("project.platform", &[]),
                plain("environment.production", &[]),
                plain("postgres.main", &[]),
                (
                    address("backup.nightly"),
                    desired_resource(candidate, "postgres.main"),
                ),
            ]),
        )
        .expect_err("invalid Backup values fail at the desired-state seam");
        assert!(
            matches!(error, DesiredStateError::InvalidPropertyValue { .. }),
            "{path}: {error:?}"
        );
    }
}

#[test]
fn stored_seam_rejects_malformed_backup_state() {
    let stored = StoredState::try_from_state(&stored_backup(false)).expect("valid state projects");
    assert!(
        stored
            .property(&address("backup.nightly"), &PropertyPath::Destination)
            .is_some()
    );

    for (key, bad) in [
        ("target", json!("compose.stack")),
        ("target", json!(null)),
        ("destination", json!({ "local": true })),
        ("destination", json!({ "id": "raw" })),
        ("destination", json!(null)),
        ("schedule", json!("")),
        ("prefix", json!("")),
        ("database", json!(null)),
        ("enabled", json!("false")),
        ("keep_latest", json!(0)),
        ("include_encryption_key", json!(null)),
    ] {
        let mut managed = json!({
            "target": "postgres.main",
            "destination": { "name": "offsite" },
            "schedule": "0 3 * * *",
            "prefix": "nightly",
            "database": "app",
            "enabled": false,
            "keep_latest": 7,
            "include_encryption_key": false
        });
        managed[key] = bad;
        let mut state = base_state();
        state_resource(
            &mut state,
            "backup.nightly",
            "backup-1",
            managed,
            &["postgres.main"],
            false,
        );
        let error = StoredState::try_from_state(&state)
            .expect_err("malformed stored Backup values fail closed");
        assert!(
            matches!(error, StoredStateError::InvalidPropertyValue { .. }),
            "{key}: {error:?}"
        );
    }
}

#[test]
fn remote_seam_rejects_invalid_backup_observations() {
    let observe = |path: PropertyPath, observation: PropertyObservation| {
        RemoteState::try_new(
            instance(),
            [(
                address("backup.nightly"),
                present("backup-1", BTreeMap::from([(path, observation)])),
            )],
        )
    };
    for (path, observation) in [
        (PropertyPath::Target, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Target,
            PropertyObservation::Known(value(json!("compose.stack"))),
        ),
        (
            PropertyPath::Target,
            PropertyObservation::Known(value(json!("redis.cache"))),
        ),
        (PropertyPath::Destination, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Destination,
            PropertyObservation::Known(value(json!({ "local": true }))),
        ),
        (
            PropertyPath::Destination,
            PropertyObservation::Known(value(json!("raw-id"))),
        ),
        (PropertyPath::Schedule, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Prefix,
            PropertyObservation::Known(value(json!(""))),
        ),
        (PropertyPath::Database, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Enabled,
            PropertyObservation::Known(value(json!("x"))),
        ),
        (
            PropertyPath::IncludeEncryptionKey,
            PropertyObservation::KnownAbsent,
        ),
        (
            PropertyPath::KeepLatest,
            PropertyObservation::Known(value(json!(0))),
        ),
        (
            PropertyPath::Schedule,
            PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
        ),
    ] {
        assert!(
            matches!(
                observe(path.clone(), observation),
                Err(RemoteStateError::InvalidPropertyObservation { .. })
            ),
            "{path}"
        );
    }

    for (path, observation) in [
        (PropertyPath::Enabled, PropertyObservation::KnownAbsent),
        (PropertyPath::KeepLatest, PropertyObservation::KnownAbsent),
        (
            PropertyPath::Destination,
            PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
        ),
        (
            PropertyPath::Schedule,
            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned),
        ),
    ] {
        observe(path, observation).expect("nullable and unknown observations are representable");
    }
}

#[test]
fn matching_backup_is_a_no_op_and_every_mutable_field_updates_in_place() {
    let state = stored_backup(false);
    let observed = || present("backup-1", observation("postgres.main", "offsite"));

    let unchanged = plan_for(
        &desired_with(Some(properties("postgres.main", "offsite"))),
        &state,
        &remote_with(Some(observed()), resolved()),
    );
    assert!(unchanged.changes().is_empty() && unchanged.applyable());

    type Edit = (PropertyPath, OwnedValue);
    let edits: Vec<Edit> = vec![
        (PropertyPath::Schedule, owned(json!("5 4 * * 1"))),
        (PropertyPath::Prefix, owned(json!("weekly"))),
        (PropertyPath::Database, owned(json!("other"))),
        (PropertyPath::Enabled, owned(json!(true))),
        (PropertyPath::KeepLatest, owned(json!(30))),
        (PropertyPath::KeepLatest, OwnedValue::Null),
        (PropertyPath::IncludeEncryptionKey, owned(json!(true))),
        (
            PropertyPath::Destination,
            owned(json!({ "name": "second" })),
        ),
    ];
    for (path, edited) in edits {
        let mut desired = properties("postgres.main", "offsite");
        desired.insert(path.clone(), edited);
        let planned = plan_for(
            &desired_with(Some(desired)),
            &state,
            &remote_with(
                Some(observed()),
                vec![ExternalResolution::Resolved(
                    RemoteId::new("destination-2").unwrap(),
                )],
            ),
        );
        assert!(planned.applyable(), "{path}: {:?}", planned.diagnostics());
        assert_eq!(backup_change(&planned).kind(), ChangeKind::Update, "{path}");
        assert_eq!(backup_change(&planned).replacement_order(), None, "{path}");
    }
}

#[test]
fn drifted_remote_value_converges_to_the_desired_state_in_place() {
    let mut drifted = observation("postgres.main", "offsite");
    drifted.insert(
        PropertyPath::Schedule,
        PropertyObservation::Known(value(json!("1 1 * * *"))),
    );
    drifted.insert(PropertyPath::Enabled, PropertyObservation::KnownAbsent);
    drifted.insert(
        PropertyPath::Destination,
        PropertyObservation::Known(value(json!({ "name": "other-bucket" }))),
    );

    let planned = plan_for(
        &desired_with(Some(properties("postgres.main", "offsite"))),
        &stored_backup(false),
        &remote_with(Some(present("backup-1", drifted)), resolved()),
    );

    assert_eq!(backup_change(&planned).kind(), ChangeKind::Update);
    assert!(planned.drift().iter().any(|drift| {
        drift.address() == &address("backup.nightly")
            && drift.properties().contains(&PropertyPath::Schedule)
    }));
}

#[test]
fn target_change_is_delete_before_create_replacement_unless_protected() {
    let mut desired = properties("postgres.other", "offsite");
    desired.insert(PropertyPath::Target, owned(json!("postgres.other")));
    let observed = || present("backup-1", observation("postgres.main", "offsite"));

    let retarget = plan_for(
        &desired_for("postgres.other", Some(desired.clone())),
        &stored_backup(false),
        &remote_with(Some(observed()), resolved()),
    );
    assert_eq!(backup_change(&retarget).kind(), ChangeKind::Replace);
    assert_eq!(
        backup_change(&retarget).replacement_order(),
        Some(ReplacementOrder::DeleteBeforeCreate)
    );
    assert_eq!(
        backup_change(&retarget)
            .checkpoint()
            .present()
            .unwrap()
            .dependencies(),
        &[address("postgres.other")],
        "the dependency follows the new target"
    );

    let protected = plan_for(
        &desired_for("postgres.other", Some(desired)),
        &stored_backup(true),
        &remote_with(Some(observed()), resolved()),
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
fn creation_requires_the_complete_contract_and_follows_its_target() {
    let empty = StateFile::new(Version::new(0, 1, 0), instance());
    let missing_everything = |backup: RemoteObservation| {
        let observations = vec![
            (address("project.platform"), RemoteObservation::Missing),
            (
                address("environment.production"),
                RemoteObservation::Missing,
            ),
            (address("postgres.main"), RemoteObservation::Missing),
            (address("postgres.other"), RemoteObservation::Missing),
            (address("backup.nightly"), backup),
        ];
        let contracts = observations
            .iter()
            .map(|(address, _)| {
                let contract = if address.kind() == ResourceKind::Backup {
                    backup_contract()
                } else {
                    MutationContract::permissive()
                };
                (address.clone(), contract)
            })
            .collect::<Vec<_>>();
        RemoteState::try_new_with_contracts(instance(), observations, contracts)
            .unwrap()
            .with_external_resolutions([(
                (address("backup.nightly"), PropertyPath::Destination),
                ExternalResolution::Resolved(RemoteId::new("destination-1").unwrap()),
            )])
            .unwrap()
    };
    // A missing Backup still carries the fresh destination resolution, so a create
    // is blocked when the selector is ambiguous or unmatched.
    let created = plan_for(
        &desired_with(Some(properties("postgres.main", "offsite"))),
        &empty,
        &missing_everything(RemoteObservation::Missing),
    );
    let order = created
        .changes()
        .iter()
        .map(|change| change.address().to_string())
        .collect::<Vec<_>>();
    assert!(
        order.iter().position(|item| item == "postgres.main")
            < order.iter().position(|item| item == "backup.nightly"),
        "{order:?}"
    );
    assert_eq!(backup_change(&created).kind(), ChangeKind::Create);

    for required in [
        PropertyPath::Target,
        PropertyPath::Destination,
        PropertyPath::Schedule,
        PropertyPath::Prefix,
        PropertyPath::Database,
        PropertyPath::Enabled,
        PropertyPath::IncludeEncryptionKey,
    ] {
        let mut incomplete = properties("postgres.main", "offsite");
        incomplete.remove(&required);
        let blocked = plan_for(
            &desired_with(Some(incomplete)),
            &base_state(),
            &remote_with(Some(RemoteObservation::Missing), resolved()),
        );
        assert!(blocked.changes().is_empty(), "{required}");
        assert_eq!(
            blocked.diagnostics()[0].code(),
            PlanDiagnosticCode::MissingCreateProperty,
            "{required}"
        );
    }
}

#[test]
fn backup_deletion_is_ordered_before_target_deletion_through_stored_dependencies() {
    let desired = DesiredState::try_new(
        digest(),
        BTreeMap::from_iter([
            plain("project.platform", &[]),
            plain("environment.production", &[]),
        ]),
    )
    .unwrap();

    let removal = plan_for(
        &desired,
        &stored_backup(false),
        &remote_with(
            Some(present("backup-1", observation("postgres.main", "offsite"))),
            Vec::new(),
        ),
    );

    let order = removal
        .changes()
        .iter()
        .filter(|change| {
            matches!(
                change.address().kind(),
                ResourceKind::Backup | ResourceKind::Postgres
            )
        })
        .map(|change| (change.address().to_string(), change.kind()))
        .collect::<Vec<_>>();
    assert_eq!(order[0], ("backup.nightly".to_owned(), ChangeKind::Delete));
    assert!(
        order[1..]
            .iter()
            .all(|(_, kind)| *kind == ChangeKind::Delete)
    );
}

#[test]
fn unresolved_destination_blocks_only_the_backup_with_a_value_free_diagnostic() {
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
                dokploy_core::RemoteFailureKind::Unavailable,
            )),
            ExternalSelectorFailure::Unavailable,
        ),
        (None, ExternalSelectorFailure::Unobserved),
    ] {
        let mut desired = properties("postgres.main", DESTINATION_CANARY);
        desired.insert(PropertyPath::Schedule, owned(json!("9 9 * * *")));
        let planned = plan_for(
            &desired_with(Some(desired)),
            &stored_backup(false),
            &remote_with(
                Some(present("backup-1", observation("postgres.main", "offsite"))),
                resolution.into_iter().collect(),
            ),
        );
        assert!(!planned.applyable());
        assert!(
            planned
                .changes()
                .iter()
                .all(|change| change.address().kind() != ResourceKind::Backup)
        );
        let diagnostic = planned
            .diagnostics()
            .iter()
            .find(|issue| issue.code() == PlanDiagnosticCode::UnresolvedExternalSelector)
            .expect("the unresolved selector is diagnosed");
        assert_eq!(diagnostic.address(), Some(&address("backup.nightly")));
        assert_eq!(diagnostic.property(), Some(&PropertyPath::Destination));
        assert_eq!(diagnostic.selector_failure(), Some(expected));
        let rendered = format!("{planned:?}{diagnostic:?}")
            + &String::from_utf8(planned.to_json_bytes()).unwrap();
        assert!(!rendered.contains(DESTINATION_CANARY));
    }
}

#[test]
fn resolved_destination_identity_is_bound_to_the_receipt_and_never_serialized() {
    let key = [7_u8; 32];
    let receipt = |identity: &str| {
        remote_with(
            Some(present("backup-1", observation("postgres.main", "offsite"))),
            vec![ExternalResolution::Resolved(
                RemoteId::new(identity).unwrap(),
            )],
        )
        .binding_receipt(&key)
    };

    assert_eq!(receipt("destination-1"), receipt("destination-1"));
    assert_ne!(
        receipt("destination-1"),
        receipt("destination-2"),
        "re-creating a destination under the same name changes the receipt"
    );
    assert_ne!(
        remote_with(
            Some(present("backup-1", observation("postgres.main", "offsite"))),
            vec![ExternalResolution::Ambiguous],
        )
        .binding_receipt(&key),
        receipt("destination-1")
    );

    let planned = plan_for(
        &desired_with(Some(properties("postgres.main", "offsite"))),
        &stored_backup(false),
        &remote_with(
            Some(present("backup-1", observation("postgres.main", "offsite"))),
            vec![ExternalResolution::Resolved(
                RemoteId::new(IDENTITY_CANARY).unwrap(),
            )],
        ),
    );
    assert!(
        !String::from_utf8(planned.to_json_bytes())
            .unwrap()
            .contains(IDENTITY_CANARY)
    );
}

#[test]
fn local_resolution_is_rejected_for_a_destination() {
    let error = remote_with(
        Some(present("backup-1", observation("postgres.main", "offsite"))),
        Vec::new(),
    )
    .with_external_resolutions([(
        (address("backup.nightly"), PropertyPath::Destination),
        ExternalResolution::Local,
    )])
    .expect_err("a destination can never be the local host");
    assert!(matches!(
        error,
        RemoteStateError::InvalidExternalResolution { .. }
    ));
}

#[test]
fn ignored_in_place_fields_keep_their_stored_checkpoint_values() {
    let mut desired = properties("postgres.main", "offsite");
    desired.insert(PropertyPath::Schedule, owned(json!("1 1 * * 1")));
    desired.insert(PropertyPath::Prefix, owned(json!("weekly")));
    let desired_state = {
        let mut resources = BTreeMap::from_iter([
            plain("project.platform", &[]),
            plain("environment.production", &[]),
            plain("postgres.main", &[]),
            plain("postgres.other", &[]),
        ]);
        resources.insert(
            address("backup.nightly"),
            desired_resource(desired, "postgres.main")
                .with_ignored_changes(vec![PropertyPath::Schedule]),
        );
        DesiredState::try_new(digest(), resources).unwrap()
    };
    let mut observed = observation("postgres.main", "offsite");
    observed.remove(&PropertyPath::Schedule);

    let planned = plan_for(
        &desired_state,
        &stored_backup(false),
        &remote_with(Some(present("backup-1", observed)), resolved()),
    );

    let change = backup_change(&planned);
    assert_eq!(change.kind(), ChangeKind::Update);
    assert_eq!(change.preserved_paths(), &[PropertyPath::Schedule]);
    let checkpoint = change.checkpoint().present().unwrap();
    let materialized = checkpoint
        .materialize(
            &address("backup.nightly"),
            RemoteId::new("backup-1").unwrap(),
        )
        .unwrap();
    assert_eq!(
        materialized.last_applied().as_json()["schedule"],
        "0 3 * * *"
    );
    assert_eq!(materialized.last_applied().as_json()["prefix"], "weekly");
}

#[test]
fn checkpoint_materializes_every_property_without_physical_destination_identity() {
    let created = plan_for(
        &desired_with(Some(properties("postgres.main", "offsite"))),
        &base_state(),
        &remote_with(Some(RemoteObservation::Missing), resolved()),
    );
    let change = backup_change(&created);
    let materialized = change
        .checkpoint()
        .present()
        .unwrap()
        .materialize(
            &address("backup.nightly"),
            RemoteId::new("backup-9").unwrap(),
        )
        .unwrap();

    assert_eq!(
        materialized.last_applied().as_json(),
        &json!({
            "target": "postgres.main",
            "destination": { "name": "offsite" },
            "schedule": "0 3 * * *",
            "prefix": "nightly",
            "database": "app",
            "enabled": false,
            "keep_latest": 7,
            "include_encryption_key": false
        })
    );
    assert_eq!(materialized.dependencies(), &[address("postgres.main")]);
    assert_eq!(materialized.containment(), Some(&environment()));
}
