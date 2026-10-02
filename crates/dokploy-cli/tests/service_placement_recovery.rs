#[path = "support/placement.rs"]
mod fake;

use std::fs;
use std::path::Path;

use dokploy_cli::executor::{ApplyWorkspaceError, apply_workspace};
use dokploy_cli::recovery::{
    RecoverWorkspaceError, RecoveryAction, recover_workspace_with_approval,
};
use dokploy_state::{RecoveryStatus, ResourceKind, StateStore};
use fake::{Fake, KINDS, Kind, Mode, SECRET_CANARY, named, seed_workspace, write_secrets};

fn workspace(fake: &Fake, yaml: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    seed_workspace(directory.path(), fake.instance(), Vec::new());
    write_secrets(directory.path());
    let config_file = directory.path().join("dokploy.yaml");
    fs::write(&config_file, yaml).unwrap();
    (directory, config_file)
}

fn store(fake: &Fake, directory: &Path) -> StateStore {
    StateStore::new(directory, fake.instance()).unwrap()
}

fn durable_text(directory: &Path) -> String {
    fn walk(path: &Path, output: &mut String) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                walk(&entry.unwrap().path(), output);
            }
        } else if let Ok(bytes) = fs::read(path) {
            output.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    let mut output = String::new();
    walk(&directory.join(".dokploy"), &mut output);
    output
}

fn assert_no_external_material(text: &str) {
    for needle in [
        SECRET_CANARY,
        "server-1",
        "server-2",
        "server-9",
        "placement-password-secret",
    ] {
        assert!(!text.contains(needle), "{needle}");
    }
}

/// Kinds whose by-name observation reads the full record, so recovery can prove an
/// uncertain create. PostgreSQL and Redis observe an unstored record by name only, so
/// every one of their uncertain creates already needs an operator decision.
fn adoptable(kind: &Kind) -> bool {
    !matches!(kind.resource, ResourceKind::Postgres | ResourceKind::Redis)
}

async fn uncertain_create(
    fake: &Fake,
    kind: &Kind,
    server: &str,
) -> (tempfile::TempDir, std::path::PathBuf) {
    fake.world()
        .fault(&format!("{}.create", kind.endpoint), Mode::DropAfterApply);
    let (directory, config_file) = workspace(fake, &kind.config(&named(server), ""));
    let error = apply_workspace(&fake.client(), &config_file)
        .await
        .expect_err("a dropped create response is an unknown outcome");
    assert!(
        matches!(error, ApplyWorkspaceError::RemoteMutation { .. }),
        "{}: {error:?}",
        kind.name
    );
    assert!(matches!(
        store(fake, directory.path()).recovery_status().unwrap(),
        RecoveryStatus::RecoveryRequired(_)
    ));
    (directory, config_file)
}

#[tokio::test]
async fn an_uncertain_placed_create_is_adopted_only_when_the_server_matches() {
    for kind in KINDS.iter().filter(|kind| adoptable(kind)) {
        let fake = Fake::start();
        let (directory, config_file) = uncertain_create(&fake, kind, "edge-1").await;
        assert_eq!(fake.world().count(kind.endpoint), 1, "the create applied");

        let mut action = None;
        let result = recover_workspace_with_approval(&fake.client(), &config_file, |preview| {
            action = Some(preview.action());
            Ok(true)
        })
        .await
        .unwrap_or_else(|error| panic!("{}: {error:?}", kind.name));

        assert_eq!(result.recovered_steps(), 1, "{}", kind.name);
        assert_eq!(action, Some(RecoveryAction::AdoptCreatedResource));
        assert_eq!(fake.count(&kind.create_line()), 1, "never retried");
        let stored = store(&fake, directory.path())
            .inspect()
            .unwrap()
            .expect("state exists");
        assert_eq!(
            stored
                .resource(&kind.address())
                .unwrap()
                .remote_id()
                .as_str(),
            kind.first_id()
        );
        assert_eq!(
            store(&fake, directory.path()).recovery_status().unwrap(),
            RecoveryStatus::Clean
        );
        assert_eq!(
            apply_workspace(&fake.client(), &config_file)
                .await
                .unwrap()
                .applied(),
            0,
            "{}",
            kind.name
        );
        assert_no_external_material(&durable_text(directory.path()));
        fake.assert_inert();
    }
}

#[tokio::test]
async fn an_uncertain_create_on_a_different_server_than_selected_needs_manual_intervention() {
    for kind in KINDS {
        let fake = Fake::start();
        let (directory, config_file) = uncertain_create(&fake, &kind, "edge-1").await;
        // The selector now names a different record than the one the create used.
        fake.world().service_mut(kind.endpoint, "main").server = Some("server-2".to_owned());
        fake.forget_requests();

        let error = recover_workspace_with_approval(&fake.client(), &config_file, |_| Ok(true))
            .await
            .expect_err("a contradicting placement cannot be adopted");

        assert!(
            matches!(error, RecoverWorkspaceError::ManualIntervention),
            "{}: {error:?}",
            kind.name
        );
        assert!(
            fake.lines()
                .iter()
                .all(|line| line.starts_with("GET /api/"))
        );
        assert!(matches!(
            store(&fake, directory.path()).recovery_status().unwrap(),
            RecoveryStatus::RecoveryRequired(_)
        ));
    }
}

#[tokio::test]
async fn recovery_requires_manual_intervention_when_the_selector_no_longer_resolves_uniquely() {
    for kind in KINDS {
        for change in ["recreated", "ambiguous", "removed", "unreadable"] {
            let fake = Fake::start();
            let (directory, config_file) = uncertain_create(&fake, &kind, "edge-1").await;
            match change {
                // The record was deleted and re-created under the same name.
                "recreated" => fake.world().recreate_server("edge-1", "server-9"),
                "ambiguous" => fake.world().add_server("server-9", "edge-1"),
                "removed" => fake
                    .world()
                    .servers
                    .retain(|server| server.name != "edge-1"),
                _ => fake.world().servers_unavailable = true,
            }
            fake.forget_requests();

            let error = recover_workspace_with_approval(&fake.client(), &config_file, |_| Ok(true))
                .await
                .expect_err("an unprovable placement needs an operator");

            assert!(
                matches!(error, RecoverWorkspaceError::ManualIntervention),
                "{} {change}: {error:?}",
                kind.name
            );
            assert!(
                fake.lines()
                    .iter()
                    .all(|line| line.starts_with("GET /api/")),
                "{} {change}",
                kind.name
            );
            assert_no_external_material(&format!("{error:?} {error}"));
            assert_no_external_material(&durable_text(directory.path()));
        }
    }
}

#[tokio::test]
async fn a_replacement_interrupted_after_the_delete_recovers_and_then_creates_on_the_new_server() {
    for kind in KINDS {
        let fake = Fake::start();
        let (directory, config_file) = workspace(&fake, &kind.config(&named("edge-1"), ""));
        let client = fake.client();
        apply_workspace(&client, &config_file).await.unwrap();
        fs::write(&config_file, kind.config(&named("edge-2"), "")).unwrap();
        fake.world().fault(
            &format!("{}.{}", kind.endpoint, delete_verb(&kind)),
            Mode::DropAfterApply,
        );

        let error = apply_workspace(&client, &config_file)
            .await
            .expect_err("a dropped delete response is an unknown outcome");
        assert!(matches!(error, ApplyWorkspaceError::RemoteMutation { .. }));
        assert_eq!(fake.world().count(kind.endpoint), 0, "the delete applied");

        let mut action = None;
        recover_workspace_with_approval(&fake.client(), &config_file, |preview| {
            action = Some(preview.action());
            Ok(true)
        })
        .await
        .unwrap_or_else(|error| panic!("{}: {error:?}", kind.name));
        assert_eq!(action, Some(RecoveryAction::CheckpointConfirmedSuccess));
        assert_eq!(fake.count(&kind.create_line()), 1, "no create before apply");

        let summary = apply_workspace(&client, &config_file).await.unwrap();

        assert_eq!(summary.applied(), 1, "{}", kind.name);
        let world = fake.world();
        let service = world.service(kind.endpoint, "main").unwrap();
        assert_eq!(service.server.as_deref(), Some("server-2"));
        drop(world);
        assert_eq!(
            store(&fake, directory.path()).recovery_status().unwrap(),
            RecoveryStatus::Clean
        );
        assert_no_external_material(&durable_text(directory.path()));
        fake.assert_inert();
    }
}

fn delete_verb(kind: &Kind) -> &'static str {
    if kind.endpoint == "compose" {
        "delete"
    } else {
        "remove"
    }
}

#[tokio::test]
async fn a_replacement_create_with_an_unknown_outcome_is_adopted_on_the_new_server() {
    for kind in KINDS.iter().filter(|kind| adoptable(kind)) {
        let fake = Fake::start();
        let (directory, config_file) = workspace(&fake, &kind.config(&named("edge-1"), ""));
        let client = fake.client();
        apply_workspace(&client, &config_file).await.unwrap();
        fs::write(&config_file, kind.config(&named("edge-2"), "")).unwrap();
        fake.world()
            .fault(&format!("{}.create", kind.endpoint), Mode::DropAfterApply);

        let error = apply_workspace(&client, &config_file)
            .await
            .expect_err("a dropped create response is an unknown outcome");
        assert!(matches!(error, ApplyWorkspaceError::RemoteMutation { .. }));
        assert_eq!(fake.count(&kind.delete_line()), 1);

        let mut action = None;
        recover_workspace_with_approval(&fake.client(), &config_file, |preview| {
            action = Some(preview.action());
            Ok(true)
        })
        .await
        .unwrap_or_else(|error| panic!("{}: {error:?}", kind.name));

        assert_eq!(action, Some(RecoveryAction::AdoptCreatedResource));
        assert_eq!(fake.count(&kind.create_line()), 2, "never retried");
        assert_eq!(
            apply_workspace(&client, &config_file)
                .await
                .unwrap()
                .applied(),
            0,
            "{}",
            kind.name
        );
        assert_eq!(fake.world().count(kind.endpoint), 1);
        assert_no_external_material(&durable_text(directory.path()));
    }
}
