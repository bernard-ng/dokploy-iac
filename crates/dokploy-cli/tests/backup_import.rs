#[path = "support/backup.rs"]
mod fake;

use std::path::Path;

use dokploy_cli::cli::ImportKind;
use dokploy_cli::import::{ImportError, ImportRequest, import_resource};
use dokploy_cli::planning::plan_workspace;
use dokploy_config::{DokployConfig, ExternalSelector, Field};
use dokploy_state::{ResourceAddress, StateStore};
use fake::{Fake, SECRET_CANARY, row};

fn address(value: &str) -> ResourceAddress {
    value.parse().unwrap()
}

async fn import(fake: &Fake, workspace: &Path, remote_id: &str) -> Result<usize, ImportError> {
    import_resource(
        &fake.client(),
        ImportRequest {
            kind: ImportKind::Backup,
            remote_id: remote_id.to_owned(),
            address: address("backup.nightly"),
            config_file: workspace.join("dokploy.yaml"),
        },
    )
    .await
}

fn read_state(fake: &Fake, workspace: &Path) -> dokploy_state::StateFile {
    StateStore::new(workspace, fake.instance())
        .unwrap()
        .inspect()
        .unwrap()
        .unwrap()
}

fn assert_only_reads(fake: &Fake) {
    assert!(fake.unrouted().is_empty(), "{:?}", fake.unrouted());
    assert!(
        fake.requests()
            .iter()
            .all(|request| request.starts_with("GET /api/")),
        "{:?}",
        fake.lines()
    );
}

fn assert_no_external_material(workspace: &Path) {
    for path in ["dokploy.yaml", ".dokploy/state.json"] {
        let text = std::fs::read_to_string(workspace.join(path)).unwrap();
        for needle in [SECRET_CANARY, "destination-1", "destination-2"] {
            assert!(!text.contains(needle), "{path}: {needle}");
        }
    }
}

#[tokio::test]
async fn postgres_backup_import_is_protected_named_and_immediately_convergent() {
    let fake = Fake::start();
    fake.world()
        .add_row(row("backup-1", "postgres-1", "destination-1", "/nightly/"));
    let workspace = tempfile::tempdir().unwrap();

    let count = import(&fake, workspace.path(), "backup-1").await.unwrap();
    let plan = plan_workspace(&fake.client(), &workspace.path().join("dokploy.yaml"))
        .await
        .unwrap();

    assert_eq!(
        count, 4,
        "project, environment, target database, and Backup"
    );
    assert!(plan.complete() && plan.applyable() && plan.changes().is_empty());
    let source = std::fs::read_to_string(workspace.path().join("dokploy.yaml")).unwrap();
    let config = DokployConfig::parse(&source).unwrap();
    let resource = config.resource(&address("backup.nightly")).unwrap();
    assert_eq!(resource.lifecycle().protect(), &Field::Set(true));
    let backup = resource.as_backup().unwrap();
    assert_eq!(backup.target(), &address("postgres.main"));
    assert_eq!(backup.destination(), &ExternalSelector::named("offsite"));
    assert_eq!(backup.schedule(), "0 3 * * *");
    assert_eq!(backup.prefix(), "/nightly/");
    assert_eq!(backup.database(), "app");
    assert!(!backup.enabled());
    assert_eq!(backup.keep_latest(), &Field::Set(7));
    assert!(!backup.include_encryption_key());
    assert_eq!(
        config.parent_of(&address("backup.nightly")),
        Some(&address("environment.production"))
    );

    let state = read_state(&fake, workspace.path());
    let imported = state.resource(&address("backup.nightly")).unwrap();
    assert!(imported.is_protected());
    assert_eq!(imported.remote_id().as_str(), "backup-1");
    assert_eq!(imported.dependencies(), &[address("postgres.main")]);
    assert_eq!(
        imported.containment(),
        Some(&address("environment.production"))
    );
    assert_eq!(
        imported.last_applied().as_json(),
        &serde_json::json!({
            "target": "postgres.main",
            "destination": { "name": "offsite" },
            "schedule": "0 3 * * *",
            "prefix": "/nightly/",
            "database": "app",
            "enabled": false,
            "keep_latest": 7,
            "include_encryption_key": false
        })
    );
    assert_no_external_material(workspace.path());
    assert_only_reads(&fake);
    fake.assert_inert();
}

#[tokio::test]
async fn mysql_backup_with_cleared_retention_imports_and_converges() {
    let fake = Fake::start();
    let mut backup = row("backup-1", "mysql-1", "destination-2", "mysql-daily");
    backup.kind = "mysql".to_owned();
    backup.keep = None;
    backup.include_key = true;
    backup.enabled = Some(true);
    fake.world().add_row(backup);
    let workspace = tempfile::tempdir().unwrap();

    import(&fake, workspace.path(), "backup-1").await.unwrap();
    let plan = plan_workspace(&fake.client(), &workspace.path().join("dokploy.yaml"))
        .await
        .unwrap();

    assert!(plan.complete() && plan.applyable() && plan.changes().is_empty());
    let source = std::fs::read_to_string(workspace.path().join("dokploy.yaml")).unwrap();
    assert!(source.contains("keep_latest: null"));
    let config = DokployConfig::parse(&source).unwrap();
    let backup = config
        .resource(&address("backup.nightly"))
        .unwrap()
        .as_backup()
        .unwrap();
    assert_eq!(backup.target(), &address("mysql.sql"));
    assert_eq!(backup.destination(), &ExternalSelector::named("archive"));
    assert!(backup.enabled() && backup.include_encryption_key());
    assert_eq!(backup.keep_latest(), &Field::Clear);
    assert_no_external_material(workspace.path());
    assert_only_reads(&fake);
}

#[tokio::test]
async fn import_fails_closed_when_the_destination_cannot_be_named_unambiguously() {
    type Setup = fn(&Fake);
    let cases: Vec<(&str, Setup)> = vec![
        ("unknown destination", |fake| {
            fake.world().destinations.clear();
        }),
        ("shared destination name", |fake| {
            fake.world()
                .destinations
                .push(("destination-9".to_owned(), "offsite".to_owned()));
        }),
        ("unreadable destination collection", |fake| {
            fake.world().destinations_unavailable = true;
        }),
        ("name outside the selector grammar", |fake| {
            fake.world().destinations[0].1 = " padded".to_owned();
        }),
    ];
    for (label, setup) in cases {
        let fake = Fake::start();
        fake.world()
            .add_row(row("backup-1", "postgres-1", "destination-1", "nightly"));
        setup(&fake);
        let workspace = tempfile::tempdir().unwrap();

        let error = import(&fake, workspace.path(), "backup-1")
            .await
            .expect_err(label);

        assert!(
            matches!(
                error,
                ImportError::ExternalAssociation | ImportError::Remote(_)
            ),
            "{label}: {error:?}"
        );
        assert!(!workspace.path().join("dokploy.yaml").exists(), "{label}");
        assert!(
            !workspace.path().join(".dokploy/state.json").exists(),
            "{label}"
        );
        assert!(!format!("{error:?}{error}").contains("offsite"));
        assert_only_reads(&fake);
    }
}

#[tokio::test]
async fn import_fails_closed_on_identity_collection_or_shape_contradictions() {
    // A null enabled flag cannot be written as a boolean.
    let fake = Fake::start();
    let mut backup = row("backup-1", "postgres-1", "destination-1", "nightly");
    backup.enabled = None;
    fake.world().add_row(backup);
    let workspace = tempfile::tempdir().unwrap();
    assert!(matches!(
        import(&fake, workspace.path(), "backup-1").await,
        Err(ImportError::InvalidRemoteTopology)
    ));
    assert!(!workspace.path().join("dokploy.yaml").exists());

    // The authoritative collection does not contain the direct record.
    let fake = Fake::start();
    fake.world()
        .add_row(row("backup-1", "postgres-1", "destination-1", "nightly"));
    fake.world().hide_from_collections = true;
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&fake, workspace.path(), "backup-1").await.is_err());
    assert!(!workspace.path().join("dokploy.yaml").exists());

    // The direct and collection records disagree.
    let fake = Fake::start();
    fake.world()
        .add_row(row("backup-1", "postgres-1", "destination-1", "nightly"));
    fake.world().collection_skew = true;
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&fake, workspace.path(), "backup-1").await.is_err());

    // A schedule outside the strict grammar is rejected by the canonical writer.
    let fake = Fake::start();
    let mut backup = row("backup-1", "postgres-1", "destination-1", "nightly");
    backup.schedule = "@daily".to_owned();
    fake.world().add_row(backup);
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&fake, workspace.path(), "backup-1").await.is_err());
    assert!(!workspace.path().join(".dokploy/state.json").exists());

    // A missing Backup surfaces as an error, not an empty import.
    let fake = Fake::start();
    let workspace = tempfile::tempdir().unwrap();
    assert!(import(&fake, workspace.path(), "backup-404").await.is_err());
    assert!(!workspace.path().join(".dokploy/state.json").exists());
}

#[test]
fn cli_accepts_the_backup_import_kind() {
    use clap::Parser;
    let parsed = dokploy_cli::cli::Cli::try_parse_from([
        "dokploy",
        "import",
        "backup",
        "backup-1",
        "--as",
        "backup.nightly",
    ])
    .expect("backup import parses");
    assert!(matches!(
        parsed.command,
        dokploy_cli::cli::Command::Import {
            kind: Some(ImportKind::Backup),
            ..
        }
    ));
}
