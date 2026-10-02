#[path = "support/backup.rs"]
mod fake;

use dokploy_cli::desired::{CompiledDesired, compile_desired};
use dokploy_cli::remote::{
    BackupTopologyAuthority, DiscoverRemoteError, DiscoveryAuthority, discover_remote,
};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, ExternalResolution, ExternalSelectorFailure, PlanDiagnosticCode,
    PropertyObservation, PropertyPath, RemoteFailureKind, RemoteObservation, StoredState, plan,
};
use dokploy_state::{StateFile, StateStore};
use fake::{Fake, NIGHTLY, SECRET_CANARY, address, backup_state, config, row, seed_workspace};

fn desired(yaml: &str) -> CompiledDesired {
    compile_desired(
        &DokployConfig::parse(yaml).unwrap(),
        ConfigDigest::parse("a".repeat(64)).unwrap(),
    )
    .unwrap()
}

fn state(fake: &Fake, extra: Vec<(&str, dokploy_state::ResourceState)>) -> StateFile {
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(directory.path(), fake.instance(), extra);
    StateStore::new(directory.path(), fake.instance())
        .unwrap()
        .inspect()
        .unwrap()
        .expect("state exists")
}

fn managed(fake: &Fake) -> StateFile {
    state(
        fake,
        vec![(
            "backup.nightly",
            backup_state("backup-1", "postgres.main", "offsite", "nightly", false),
        )],
    )
}

async fn observe(
    fake: &Fake,
    state: &StateFile,
    desired: &CompiledDesired,
    authority: DiscoveryAuthority,
) -> Result<dokploy_core::RemoteState, DiscoverRemoteError> {
    discover_remote(&fake.client(), desired, state, authority).await
}

fn partial() -> DiscoveryAuthority {
    DiscoveryAuthority {
        backups: BackupTopologyAuthority::Partial,
        ..DiscoveryAuthority::reconciliation()
    }
}

fn backup_observation(remote: &dokploy_core::RemoteState) -> &RemoteObservation {
    remote
        .observation(&address("backup.nightly"))
        .expect("the Backup is observed")
}

fn plan_for(
    desired: &CompiledDesired,
    state: &StateFile,
    remote: &dokploy_core::RemoteState,
) -> dokploy_core::Plan {
    plan(
        desired.desired_state(),
        &StoredState::try_from_state(state).unwrap(),
        remote,
    )
}

fn seeded_row(fake: &Fake) {
    fake.world()
        .add_row(row("backup-1", "postgres-1", "destination-1", "nightly"));
}

fn assert_read_only(fake: &Fake) {
    assert!(
        fake.requests()
            .iter()
            .all(|request| request.starts_with("GET /api/")),
        "{:?}",
        fake.lines()
    );
    fake.assert_inert();
}

#[tokio::test]
async fn authoritative_target_collection_absence_plans_a_create() {
    let fake = Fake::start();
    let state = state(&fake, vec![]);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let planned = plan_for(&desired, &state, &remote);

    assert!(matches!(
        backup_observation(&remote),
        RemoteObservation::Missing
    ));
    assert!(planned.applyable(), "{:?}", planned.diagnostics());
    assert!(
        planned
            .changes()
            .iter()
            .any(|change| change.address() == &address("backup.nightly")
                && change.kind() == ChangeKind::Create)
    );
    assert_eq!(fake.count("GET /api/destination.all"), 1);
    assert_eq!(fake.count("GET /api/backup.one"), 0);
    assert_read_only(&fake);
}

#[tokio::test]
async fn partial_authority_downgrades_absence_to_an_unavailable_observation() {
    let fake = Fake::start();
    let state = state(&fake, vec![]);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(&fake, &state, &desired, partial()).await.unwrap();
    let planned = plan_for(&desired, &state, &remote);

    assert!(matches!(
        backup_observation(&remote),
        RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
    ));
    assert!(!planned.applyable());
    assert!(
        planned
            .changes()
            .iter()
            .all(|change| change.address() != &address("backup.nightly"))
    );
    assert_read_only(&fake);
}

#[tokio::test]
async fn managed_backup_agreeing_with_its_collection_is_observed_and_a_no_op() {
    let fake = Fake::start();
    seeded_row(&fake);
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    let RemoteObservation::Present(resource) = backup_observation(&remote) else {
        panic!("the Backup is present");
    };
    assert_eq!(resource.remote_id().as_str(), "backup-1");
    for (path, expected) in [
        (PropertyPath::Target, serde_json::json!("postgres.main")),
        (
            PropertyPath::Destination,
            serde_json::json!({ "name": "offsite" }),
        ),
        (PropertyPath::Schedule, serde_json::json!("0 3 * * *")),
        (PropertyPath::Prefix, serde_json::json!("nightly")),
        (PropertyPath::Database, serde_json::json!("app")),
        (PropertyPath::Enabled, serde_json::json!(false)),
        (PropertyPath::KeepLatest, serde_json::json!(7)),
        (PropertyPath::IncludeEncryptionKey, serde_json::json!(false)),
    ] {
        let Some(PropertyObservation::Known(value)) = resource.property(&path) else {
            panic!("{path} is observed");
        };
        assert_eq!(
            value,
            &dokploy_core::ComparableValue::try_from_json(expected).unwrap(),
            "{path}"
        );
    }
    assert!(matches!(
        remote.external_resolution(&address("backup.nightly"), &PropertyPath::Destination),
        Some(ExternalResolution::Resolved(_))
    ));
    let planned = plan_for(&desired, &state, &remote);
    assert!(planned.applyable() && planned.changes().is_empty());
    assert!(fake.count("GET /api/backup.one") >= 1);
    assert_read_only(&fake);
}

#[tokio::test]
async fn nullable_remote_fields_observe_as_absent_and_converge_by_update() {
    let fake = Fake::start();
    seeded_row(&fake);
    {
        let mut world = fake.world();
        let row = world.row_mut("backup-1");
        row.enabled = None;
        row.keep = None;
    }
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let planned = plan_for(&desired, &state, &remote);

    let RemoteObservation::Present(resource) = backup_observation(&remote) else {
        panic!("the Backup is present");
    };
    assert_eq!(
        resource.property(&PropertyPath::Enabled),
        Some(&PropertyObservation::KnownAbsent)
    );
    assert_eq!(
        resource.property(&PropertyPath::KeepLatest),
        Some(&PropertyObservation::KnownAbsent)
    );
    assert!(planned.applyable());
    assert!(
        planned
            .changes()
            .iter()
            .any(|change| change.address() == &address("backup.nightly")
                && change.kind() == ChangeKind::Update)
    );
}

#[tokio::test]
async fn drifted_destination_association_is_observed_by_name_and_planned_in_place() {
    let fake = Fake::start();
    seeded_row(&fake);
    fake.world().row_mut("backup-1").destination_id = "destination-2".to_owned();
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let planned = plan_for(&desired, &state, &remote);

    let RemoteObservation::Present(resource) = backup_observation(&remote) else {
        panic!("the Backup is present");
    };
    let Some(PropertyObservation::Known(value)) = resource.property(&PropertyPath::Destination)
    else {
        panic!("the destination is observed");
    };
    assert_eq!(
        value,
        &dokploy_core::ComparableValue::try_from_json(serde_json::json!({ "name": "archive" }))
            .unwrap()
    );
    assert!(
        planned
            .drift()
            .iter()
            .any(|drift| drift.properties().contains(&PropertyPath::Destination))
    );
    assert!(
        planned
            .changes()
            .iter()
            .any(|change| change.address() == &address("backup.nightly")
                && change.kind() == ChangeKind::Update)
    );
}

#[tokio::test]
async fn direct_404_with_authoritative_absence_is_missing_and_partial_authority_is_not() {
    let fake = Fake::start();
    // The stored Backup does not exist remotely: `backup.one` is 404 and the
    // collection does not list it.
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    let missing = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    assert!(matches!(
        backup_observation(&missing),
        RemoteObservation::Missing
    ));
    // A deleted Backup is recreated rather than forgotten.
    let planned = plan_for(&desired, &state, &missing);
    assert!(
        planned
            .changes()
            .iter()
            .any(|change| change.address() == &address("backup.nightly")
                && change.kind() == ChangeKind::Create)
    );

    let downgraded = observe(&fake, &state, &desired, partial()).await.unwrap();
    assert!(matches!(
        backup_observation(&downgraded),
        RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
    ));
}

#[tokio::test]
async fn direct_404_while_the_collection_lists_the_identity_is_contradictory() {
    let fake = Fake::start();
    seeded_row(&fake);
    fake.world().direct_404.push("backup-1".to_owned());
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        backup_observation(&remote),
        RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
    ));
}

#[tokio::test]
async fn a_direct_read_missing_from_its_collection_is_not_trusted() {
    let fake = Fake::start();
    seeded_row(&fake);
    fake.world().hide_from_collections = true;
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        backup_observation(&remote),
        RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
    ));
}

#[tokio::test]
async fn direct_and_collection_records_that_disagree_block_planning() {
    let fake = Fake::start();
    seeded_row(&fake);
    fake.world().collection_skew = true;
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    // The SDK refuses a direct record that differs from its authoritative
    // collection, so the observation is unavailable rather than guessed.
    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let planned = plan_for(&desired, &state, &remote);

    assert!(matches!(
        backup_observation(&remote),
        RemoteObservation::Unavailable(RemoteFailureKind::InvalidResponse)
    ));
    assert!(!planned.applyable());
    assert!(
        planned
            .changes()
            .iter()
            .all(|change| change.address() != &address("backup.nightly"))
    );
}

#[test]
fn conflict_diagnostics_have_stable_codes() {
    for (error, code) in [
        (DiscoverRemoteError::BackupTarget, "DOKREM100"),
        (DiscoverRemoteError::InvalidBackupId, "DOKREM101"),
        (DiscoverRemoteError::DuplicateBackupCollision, "DOKREM102"),
        (DiscoverRemoteError::DuplicateBackupId, "DOKREM103"),
        (DiscoverRemoteError::BackupTopologyConflict, "DOKREM104"),
    ] {
        assert!(error.to_string().starts_with(code), "{error}");
    }
}

#[tokio::test]
async fn one_identity_in_two_target_collections_fails_closed() {
    let fake = Fake::start();
    seeded_row(&fake);
    let mut duplicate = row("backup-1", "mysql-1", "destination-1", "other");
    duplicate.kind = "mysql".to_owned();
    fake.world().add_row(duplicate);
    let state = state(
        &fake,
        vec![(
            "backup.nightly",
            backup_state("backup-1", "postgres.main", "offsite", "nightly", false),
        )],
    );
    let desired = desired(&config(&format!(
        "{NIGHTLY}      second:\n        target: mysql.sql\n        destination: {{ name: offsite }}\n        schedule: \"0 3 * * *\"\n        prefix: other\n        database: app\n"
    )));

    let error = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("a duplicated identity fails closed");

    assert!(matches!(error, DiscoverRemoteError::DuplicateBackupId));
    assert!(error.to_string().starts_with("DOKREM103"));
}

#[tokio::test]
async fn an_unmanaged_backup_at_the_collision_key_is_observed_and_blocks_as_a_collision() {
    let fake = Fake::start();
    // The runtime-normalized prefix `/nightly/` occupies the desired key.
    fake.world()
        .add_row(row("backup-9", "postgres-1", "destination-1", "/nightly/"));
    let state = state(&fake, vec![]);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let planned = plan_for(&desired, &state, &remote);

    assert!(matches!(
        backup_observation(&remote),
        RemoteObservation::Present(resource) if resource.remote_id().as_str() == "backup-9"
    ));
    assert!(!planned.applyable());
    assert!(
        planned
            .diagnostics()
            .iter()
            .any(|issue| issue.code() == PlanDiagnosticCode::UnmanagedAddressCollision)
    );
    assert_read_only(&fake);
}

#[tokio::test]
async fn a_backup_on_another_destination_or_prefix_is_not_a_collision() {
    let fake = Fake::start();
    fake.world()
        .add_row(row("backup-8", "postgres-1", "destination-2", "nightly"));
    fake.world()
        .add_row(row("backup-9", "postgres-1", "destination-1", "weekly"));
    let state = state(&fake, vec![]);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        backup_observation(&remote),
        RemoteObservation::Missing
    ));
}

#[tokio::test]
async fn a_key_change_landing_on_another_backup_fails_closed() {
    let fake = Fake::start();
    seeded_row(&fake);
    fake.world()
        .add_row(row("backup-9", "postgres-1", "destination-1", "weekly"));
    let state = managed(&fake);
    let desired = desired(&config(
        &NIGHTLY.replace("prefix: nightly", "prefix: weekly"),
    ));

    let error = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("an update must not land on another Backup");

    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateBackupCollision
    ));
    assert!(error.to_string().starts_with("DOKREM102"));
}

#[tokio::test]
async fn a_target_change_landing_on_another_backup_fails_closed() {
    let fake = Fake::start();
    seeded_row(&fake);
    let mut occupant = row("backup-9", "mysql-1", "destination-1", "nightly");
    occupant.kind = "mysql".to_owned();
    fake.world().add_row(occupant);
    let state = managed(&fake);
    let desired = desired(&config(&NIGHTLY.replace("postgres.main", "mysql.sql")));

    let error = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .expect_err("a replacement must not land on another Backup");

    assert!(matches!(
        error,
        DiscoverRemoteError::DuplicateBackupCollision
    ));
}

#[tokio::test]
async fn target_change_is_planned_as_delete_before_create_replacement() {
    let fake = Fake::start();
    seeded_row(&fake);
    let state = managed(&fake);
    let desired = desired(&config(&NIGHTLY.replace("postgres.main", "mysql.sql")));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let planned = plan_for(&desired, &state, &remote);

    let change = planned
        .changes()
        .iter()
        .find(|change| change.address() == &address("backup.nightly"))
        .expect("the Backup is replaced");
    assert_eq!(change.kind(), ChangeKind::Replace);
    assert_eq!(
        change.replacement_order(),
        Some(dokploy_core::ReplacementOrder::DeleteBeforeCreate)
    );
}

#[tokio::test]
async fn unresolved_destinations_block_planning_with_value_free_diagnostics() {
    for (destinations, expected) in [
        (Vec::new(), ExternalSelectorFailure::Unmatched),
        (
            vec![
                ("destination-1".to_owned(), "offsite".to_owned()),
                ("destination-9".to_owned(), "offsite".to_owned()),
            ],
            ExternalSelectorFailure::Ambiguous,
        ),
    ] {
        for managed_backup in [false, true] {
            let fake = Fake::start();
            fake.world().destinations = destinations.clone();
            let state = if managed_backup {
                seeded_row(&fake);
                managed(&fake)
            } else {
                state(&fake, vec![])
            };
            let desired = desired(&config(NIGHTLY));

            let remote = observe(
                &fake,
                &state,
                &desired,
                DiscoveryAuthority::reconciliation(),
            )
            .await
            .unwrap();
            let planned = plan_for(&desired, &state, &remote);

            assert!(!planned.applyable());
            let diagnostic = planned
                .diagnostics()
                .iter()
                .find(|issue| issue.code() == PlanDiagnosticCode::UnresolvedExternalSelector)
                .unwrap_or_else(|| panic!("{:?}", planned.diagnostics()));
            assert_eq!(diagnostic.address(), Some(&address("backup.nightly")));
            assert_eq!(diagnostic.property(), Some(&PropertyPath::Destination));
            assert_eq!(diagnostic.selector_failure(), Some(expected));
            let rendered = format!("{:?}", planned.diagnostics())
                + &String::from_utf8(planned.to_json_bytes()).unwrap();
            assert!(!rendered.contains("offsite") && !rendered.contains("destination-"));
        }
    }
}

#[tokio::test]
async fn an_unreadable_destination_collection_blocks_without_guessing() {
    let fake = Fake::start();
    seeded_row(&fake);
    fake.world().destinations_unavailable = true;
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let planned = plan_for(&desired, &state, &remote);

    assert!(!planned.applyable());
    assert!(planned.diagnostics().iter().any(|issue| issue.code()
        == PlanDiagnosticCode::UnresolvedExternalSelector
        && issue.selector_failure() == Some(ExternalSelectorFailure::Unavailable)));
}

#[tokio::test]
async fn malformed_or_unsupported_stored_targets_fail_closed() {
    for target in [
        "compose.stack",
        "redis.cache",
        "application.api",
        "not-an-address",
    ] {
        let fake = Fake::start();
        let state = state(
            &fake,
            vec![(
                "backup.nightly",
                backup_state("backup-1", "postgres.main", "offsite", "nightly", false),
            )],
        );
        // Rewrite the stored target to an unsupported or malformed value.
        let mut tampered = state.clone();
        let stored = tampered
            .resource(&address("backup.nightly"))
            .unwrap()
            .clone();
        let mut inputs = stored.last_applied().as_json().clone();
        inputs["target"] = serde_json::json!(target);
        tampered
            .upsert_resource(
                address("backup.nightly"),
                dokploy_state::ResourceState::new(
                    stored.kind(),
                    stored.remote_id().clone(),
                    false,
                    dokploy_state::ManagedInputs::try_from_json(inputs).unwrap(),
                    stored.containment().cloned(),
                    stored.dependencies().to_vec(),
                ),
            )
            .unwrap();
        let desired = desired(&config(NIGHTLY));

        let error = observe(
            &fake,
            &tampered,
            &desired,
            DiscoveryAuthority::reconciliation(),
        )
        .await
        .expect_err("an unsupported stored target fails closed");

        assert!(
            matches!(error, DiscoverRemoteError::BackupTarget),
            "{target}"
        );
        assert!(error.to_string().starts_with("DOKREM100"));
    }
}

#[tokio::test]
async fn a_missing_target_makes_its_backup_missing_without_reading_a_collection() {
    let fake = Fake::start();
    fake.world()
        .targets
        .retain(|target| target.id != "postgres-1");
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert!(matches!(
        backup_observation(&remote),
        RemoteObservation::Missing
    ));
    assert_eq!(fake.count("GET /api/backup.one"), 0);
}

#[tokio::test]
async fn configurations_without_backups_perform_no_destination_or_backup_reads() {
    let fake = Fake::start();
    let state = state(&fake, vec![]);
    let desired = desired(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      postgres:\n        main: {}\n",
    );

    observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();

    assert_eq!(fake.count("GET /api/destination.all"), 0);
    assert_eq!(fake.count("GET /api/backup.one"), 0);
}

#[tokio::test]
async fn discovery_never_leaks_destination_credentials_or_identities() {
    let fake = Fake::start();
    seeded_row(&fake);
    let state = managed(&fake);
    let desired = desired(&config(NIGHTLY));

    let remote = observe(
        &fake,
        &state,
        &desired,
        DiscoveryAuthority::reconciliation(),
    )
    .await
    .unwrap();
    let planned = plan_for(&desired, &state, &remote);

    let rendered = format!("{remote:?}{planned:?}{desired:?}")
        + &String::from_utf8(planned.to_json_bytes()).unwrap();
    for needle in [SECRET_CANARY, "destination-1", "destination-2"] {
        assert!(!rendered.contains(needle), "{needle}");
    }
}
