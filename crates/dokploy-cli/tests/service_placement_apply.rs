#[path = "support/placement.rs"]
mod fake;

use std::fs;
use std::path::Path;

use dokploy_cli::executor::{ApplyWorkspaceError, apply_workspace};
use dokploy_state::{RecoveryStatus, ResourceKind, StateFile, StateStore};
use fake::{
    Fake, KINDS, Kind, LOCAL, Mode, SECRET_CANARY, dependent_state, named, placed_state,
    seed_workspace, write_secrets,
};

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

fn state(fake: &Fake, directory: &Path) -> StateFile {
    store(fake, directory)
        .inspect()
        .unwrap()
        .expect("state exists")
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
        "placement-root-secret",
    ] {
        assert!(!text.contains(needle), "{needle}");
    }
}

fn create_body(fake: &Fake, kind: &Kind) -> String {
    let creates = fake.matching(&kind.create_line());
    assert_eq!(creates.len(), 1, "{}", kind.name);
    creates[0].clone()
}

fn recorded_server(state: &StateFile, kind: &Kind) -> serde_json::Value {
    state
        .resource(&kind.address())
        .expect("service is in state")
        .last_applied()
        .as_json()["server"]
        .clone()
}

#[tokio::test]
async fn create_places_every_kind_on_the_resolved_server_and_converges_to_a_no_op() {
    for kind in KINDS {
        let fake = Fake::start();
        let (directory, config_file) = workspace(&fake, &kind.config(&named("edge-1"), ""));
        let client = fake.client();

        let summary = apply_workspace(&client, &config_file).await.unwrap();

        assert_eq!(summary.applied(), 1, "{}", kind.name);
        assert!(
            create_body(&fake, &kind).contains(r#""serverId":"server-1""#),
            "{}",
            kind.name
        );
        assert_eq!(
            fake.world()
                .service(kind.endpoint, "main")
                .unwrap()
                .server
                .as_deref(),
            Some("server-1"),
            "{}",
            kind.name
        );
        let stored = state(&fake, directory.path());
        assert_eq!(
            recorded_server(&stored, &kind),
            serde_json::json!({"name": "edge-1"}),
            "{}",
            kind.name
        );
        assert_eq!(
            stored
                .resource(&kind.address())
                .unwrap()
                .remote_id()
                .as_str(),
            kind.first_id()
        );

        let converged = apply_workspace(&client, &config_file).await.unwrap();

        assert_eq!(converged.applied(), 0, "{}", kind.name);
        assert_eq!(fake.count(&kind.create_line()), 1, "{}", kind.name);
        assert_eq!(fake.count(&kind.delete_line()), 0, "{}", kind.name);
        assert_eq!(
            store(&fake, directory.path()).recovery_status().unwrap(),
            RecoveryStatus::Clean
        );
        assert_no_external_material(&durable_text(directory.path()));
        fake.assert_inert();
    }
}

#[tokio::test]
async fn adopting_a_placement_that_already_matches_changes_no_service_and_a_mismatch_replaces() {
    for kind in KINDS {
        for matching in [true, false] {
            let fake = Fake::start();
            let (directory, config_file) = workspace(&fake, &kind.config("", ""));
            let client = fake.client();
            apply_workspace(&client, &config_file).await.unwrap();
            fake.world().service_mut(kind.endpoint, "main").server = Some("server-1".to_owned());
            let selected = if matching { "edge-1" } else { "edge-2" };
            fs::write(&config_file, kind.config(&named(selected), "")).unwrap();
            fake.forget_requests();

            let summary = apply_workspace(&client, &config_file).await.unwrap();

            assert_eq!(
                fake.count(&kind.delete_line()),
                usize::from(!matching),
                "{} {matching}",
                kind.name
            );
            assert_eq!(summary.applied(), 1, "{} {matching}", kind.name);
            assert_eq!(
                recorded_server(&state(&fake, directory.path()), &kind),
                serde_json::json!({"name": selected})
            );
            assert_eq!(
                apply_workspace(&client, &config_file)
                    .await
                    .unwrap()
                    .applied(),
                0
            );
            fake.assert_inert();
        }
    }
}

#[tokio::test]
async fn a_local_selector_creates_with_an_explicit_null_and_converges() {
    for kind in KINDS {
        let fake = Fake::start();
        let (directory, config_file) = workspace(&fake, &kind.config(LOCAL, ""));
        let client = fake.client();

        let summary = apply_workspace(&client, &config_file).await.unwrap();

        assert_eq!(summary.applied(), 1, "{}", kind.name);
        assert!(
            create_body(&fake, &kind).contains(r#""serverId":null"#),
            "{}",
            kind.name
        );
        let stored = state(&fake, directory.path());
        assert_eq!(
            recorded_server(&stored, &kind),
            serde_json::json!({"local": true})
        );
        assert_eq!(
            apply_workspace(&client, &config_file)
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
async fn an_unmanaged_placement_omits_the_server_and_reads_no_external_collection() {
    for kind in KINDS {
        let fake = Fake::start();
        let (directory, config_file) = workspace(&fake, &kind.config("", ""));
        let client = fake.client();

        apply_workspace(&client, &config_file).await.unwrap();

        // `libsql.create` requires the key, so an unmanaged LibSQL placement sends null;
        // every other create omits it.
        let body = create_body(&fake, &kind);
        if kind.resource == ResourceKind::LibSql {
            assert!(body.contains(r#""serverId":null"#), "{}", kind.name);
        } else {
            assert!(!body.contains("serverId"), "{}", kind.name);
        }
        assert_eq!(fake.count("GET /api/server.all"), 0, "{}", kind.name);
        assert!(
            state(&fake, directory.path())
                .resource(&kind.address())
                .unwrap()
                .last_applied()
                .as_json()
                .get("server")
                .is_none()
        );
        // Placing the service out of band is not drift while placement is unmanaged.
        fake.world().service_mut(kind.endpoint, "main").server = Some("server-2".to_owned());
        assert_eq!(
            apply_workspace(&client, &config_file)
                .await
                .unwrap()
                .applied(),
            0
        );
    }
}

#[tokio::test]
async fn a_changed_server_replaces_the_service_delete_before_create() {
    for kind in KINDS {
        let fake = Fake::start();
        let (directory, config_file) = workspace(&fake, &kind.config(&named("edge-1"), ""));
        let client = fake.client();
        apply_workspace(&client, &config_file).await.unwrap();
        fs::write(&config_file, kind.config(&named("edge-2"), "")).unwrap();
        fake.forget_requests();

        let summary = apply_workspace(&client, &config_file).await.unwrap();

        assert_eq!(summary.applied(), 1, "{}", kind.name);
        let lines = fake.lines();
        let delete = lines
            .iter()
            .position(|line| *line == kind.delete_line())
            .unwrap_or_else(|| panic!("{} was not deleted", kind.name));
        let create = lines
            .iter()
            .position(|line| *line == kind.create_line())
            .unwrap_or_else(|| panic!("{} was not re-created", kind.name));
        assert!(delete < create, "{}", kind.name);
        assert_eq!(fake.count(&kind.delete_line()), 1);
        assert_eq!(fake.count(&kind.create_line()), 1);
        assert!(
            create_body(&fake, &kind).contains(r#""serverId":"server-2""#),
            "{}",
            kind.name
        );
        {
            let world = fake.world();
            assert_eq!(world.count(kind.endpoint), 1, "old identity is gone");
            let service = world.service(kind.endpoint, "main").unwrap();
            assert_eq!(service.server.as_deref(), Some("server-2"));
            assert_eq!(service.id, format!("{}-2", kind.name));
        }
        let stored = state(&fake, directory.path());
        assert_eq!(
            stored
                .resource(&kind.address())
                .unwrap()
                .remote_id()
                .as_str(),
            format!("{}-2", kind.name)
        );
        assert_eq!(
            recorded_server(&stored, &kind),
            serde_json::json!({"name": "edge-2"})
        );
        assert_eq!(
            store(&fake, directory.path()).recovery_status().unwrap(),
            RecoveryStatus::Clean
        );
        assert_eq!(
            apply_workspace(&client, &config_file)
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
async fn replacement_is_refused_while_durable_state_holds_dependents() {
    for kind in KINDS {
        for dependent in dependents_of(&kind) {
            let fake = Fake::start();
            let directory = tempfile::tempdir().unwrap();
            let target = kind.address().to_string();
            seed_workspace(
                directory.path(),
                fake.instance(),
                vec![
                    (
                        target.as_str(),
                        placed_state(&kind, &kind.first_id(), "edge-1", false),
                    ),
                    (
                        fake::dependent_address(dependent),
                        dependent_state(dependent, "dependent-1", &target),
                    ),
                ],
            );
            write_secrets(directory.path());
            let config_file = directory.path().join("dokploy.yaml");
            fs::write(&config_file, kind.bare_config(&named("edge-2"), "")).unwrap();
            fake.world().services.push(fake::Service {
                kind: kind.endpoint,
                id: kind.first_id(),
                name: "main".to_owned(),
                server: Some("server-1".to_owned()),
                fields: serde_json::json!({
                    "databaseName": "app",
                    "databaseUser": "app",
                    "replicaSets": false,
                    "description": null,
                })
                .as_object()
                .unwrap()
                .clone(),
            });

            let error = apply_workspace(&fake.client(), &config_file)
                .await
                .expect_err("dependents block the replacement");

            assert!(
                matches!(error, ApplyWorkspaceError::ReplacementBlockedByDependents),
                "{} {dependent:?}: {error:?}",
                kind.name
            );
            assert!(
                fake.lines()
                    .iter()
                    .all(|line| line.starts_with("GET /api/")),
                "{} {dependent:?}: no mutation precedes the refusal: {:?}",
                kind.name,
                fake.lines()
            );
            assert_eq!(fake.world().count(kind.endpoint), 1);
            assert_eq!(
                fake.world()
                    .service(kind.endpoint, "main")
                    .unwrap()
                    .server
                    .as_deref(),
                Some("server-1")
            );
            assert_eq!(
                store(&fake, directory.path()).recovery_status().unwrap(),
                RecoveryStatus::Clean
            );
        }
    }
}

/// The Mount, Schedule, and Backup adapters that can target `kind`.
fn dependents_of(kind: &Kind) -> Vec<ResourceKind> {
    let mut dependents = vec![ResourceKind::Mount];
    match kind.resource {
        ResourceKind::Compose => dependents.push(ResourceKind::Schedule),
        ResourceKind::Redis => {}
        _ => dependents.push(ResourceKind::Backup),
    }
    dependents
}

#[tokio::test]
async fn a_protected_service_is_never_replaced_by_a_placement_change() {
    for kind in KINDS {
        let fake = Fake::start();
        let protect = "        lifecycle:\n          protect: true\n";
        let (directory, config_file) = workspace(&fake, &kind.config(&named("edge-1"), protect));
        let client = fake.client();
        apply_workspace(&client, &config_file).await.unwrap();
        fs::write(&config_file, kind.config(&named("edge-2"), protect)).unwrap();
        fake.forget_requests();

        let error = apply_workspace(&client, &config_file)
            .await
            .expect_err("protection blocks replacement");

        assert!(
            matches!(error, ApplyWorkspaceError::PlanBlocked),
            "{}",
            kind.name
        );
        assert!(
            fake.lines()
                .iter()
                .all(|line| line.starts_with("GET /api/"))
        );
        assert_eq!(fake.world().count(kind.endpoint), 1);
        assert_eq!(
            state(&fake, directory.path())
                .resource(&kind.address())
                .unwrap()
                .remote_id()
                .as_str(),
            kind.first_id()
        );
    }
}

#[tokio::test]
async fn unmatched_ambiguous_and_unreadable_selectors_block_apply_before_any_mutation() {
    for kind in KINDS {
        for setup in ["unmatched", "ambiguous", "unreadable"] {
            let fake = Fake::start();
            match setup {
                "ambiguous" => fake.world().add_server("server-9", "edge-1"),
                "unreadable" => fake.world().servers_unavailable = true,
                _ => {}
            }
            let name = if setup == "unmatched" {
                "ghost"
            } else {
                "edge-1"
            };
            let (directory, config_file) = workspace(&fake, &kind.config(&named(name), ""));

            let error = apply_workspace(&fake.client(), &config_file)
                .await
                .expect_err("unresolved selector blocks apply");

            assert!(
                matches!(error, ApplyWorkspaceError::PlanBlocked),
                "{} {setup}",
                kind.name
            );
            assert!(
                fake.lines()
                    .iter()
                    .all(|line| line.starts_with("GET /api/")),
                "{} {setup}",
                kind.name
            );
            assert_eq!(fake.world().count(kind.endpoint), 0);
            assert_no_external_material(&durable_text(directory.path()));
            assert_no_external_material(&format!("{error:?} {error}"));
        }
    }
}

#[tokio::test]
async fn an_ignored_server_neither_resolves_nor_replaces_but_refuses_a_placed_create() {
    for kind in KINDS {
        let ignore = "        lifecycle:\n          ignore_changes: [server]\n";
        // Created once with the first server, a later placement change is ignored.
        let fake = Fake::start();
        let (directory, config_file) = workspace(&fake, &kind.config(&named("edge-1"), ""));
        let client = fake.client();
        apply_workspace(&client, &config_file).await.unwrap();
        fs::write(&config_file, kind.config(&named("edge-2"), ignore)).unwrap();
        fake.forget_requests();

        let summary = apply_workspace(&client, &config_file).await.unwrap();

        assert_eq!(summary.applied(), 0, "{}", kind.name);
        assert_eq!(fake.count("GET /api/server.all"), 0, "{}", kind.name);
        assert_eq!(fake.world().count(kind.endpoint), 1);
        drop(directory);

        // An ignored selector is never resolved, so a create that needs it is refused
        // before the first mutation instead of guessing a placement.
        let fake = Fake::start();
        let (_directory, config_file) = workspace(&fake, &kind.config(&named("edge-1"), ignore));
        let error = apply_workspace(&fake.client(), &config_file)
            .await
            .expect_err("an unresolved ignored placement cannot be created");
        assert!(
            matches!(error, ApplyWorkspaceError::ExternalResolutionMissing),
            "{}: {error:?}",
            kind.name
        );
        assert!(
            fake.lines()
                .iter()
                .all(|line| line.starts_with("GET /api/"))
        );
        assert_eq!(fake.world().count(kind.endpoint), 0);
    }
}

#[tokio::test]
async fn a_definitive_create_rejection_leaves_no_state_and_no_uncertain_step() {
    for kind in KINDS {
        let fake = Fake::start();
        fake.world().fault(
            &format!("{}.create", kind.endpoint),
            Mode::Reject("400 Bad Request"),
        );
        let (directory, config_file) = workspace(&fake, &kind.config(&named("edge-1"), ""));

        let error = apply_workspace(&fake.client(), &config_file)
            .await
            .expect_err("rejected create fails");

        assert!(
            matches!(error, ApplyWorkspaceError::RemoteMutation { .. }),
            "{}",
            kind.name
        );
        assert!(
            state(&fake, directory.path())
                .resource(&kind.address())
                .is_none()
        );
        // A definitive rejection fails its journal step: nothing is uncertain.
        let RecoveryStatus::RecoveryRequired(summary) =
            store(&fake, directory.path()).recovery_status().unwrap()
        else {
            panic!("{}: a failed step requires acknowledgement", kind.name);
        };
        assert!(summary.uncertain_step().is_none(), "{}", kind.name);
        assert_no_external_material(&durable_text(directory.path()));
    }
}
