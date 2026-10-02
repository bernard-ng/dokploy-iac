#[path = "support/placement.rs"]
mod fake;

use dokploy_cli::cli::ImportKind;
use dokploy_cli::import::{ImportError, ImportRequest, import_resource};
use dokploy_cli::planning::plan_workspace;
use dokploy_config::{DokployConfig, ExternalSelector, Field};
use dokploy_state::StateStore;
use fake::{Fake, KINDS, Kind, SECRET_CANARY};

fn import_kind(kind: &Kind) -> ImportKind {
    match kind.name {
        "compose" => ImportKind::Compose,
        "postgres" => ImportKind::Postgres,
        "mysql" => ImportKind::MySql,
        "mariadb" => ImportKind::MariaDb,
        "mongo" => ImportKind::Mongo,
        "libsql" => ImportKind::LibSql,
        _ => ImportKind::Redis,
    }
}

fn request(kind: &Kind, config_file: std::path::PathBuf) -> ImportRequest {
    ImportRequest {
        kind: import_kind(kind),
        remote_id: kind.first_id(),
        address: kind.address(),
        config_file,
    }
}

/// Registers the existing remote service the import adopts.
fn remote_service(fake: &Fake, kind: &Kind, server: Option<&str>) {
    let mut fields = serde_json::Map::new();
    fields.insert("databaseName".into(), serde_json::json!("app"));
    fields.insert("databaseUser".into(), serde_json::json!("app"));
    fields.insert("replicaSets".into(), serde_json::json!(false));
    fields.insert("description".into(), serde_json::json!("Imported"));
    fake.world().services.push(fake::Service {
        kind: kind.endpoint,
        id: kind.first_id(),
        name: "main".to_owned(),
        server: server.map(str::to_owned),
        fields,
    });
}

fn assert_no_identities(text: &str) {
    for needle in [
        SECRET_CANARY,
        "server-1",
        "server-2",
        "server-9",
        "203.0.113.7",
    ] {
        assert!(!text.contains(needle), "{needle} leaked");
    }
}

#[tokio::test]
async fn import_writes_a_name_selector_and_converges_without_identities() {
    for kind in KINDS {
        let fake = Fake::start();
        remote_service(&fake, &kind, Some("server-1"));
        let client = fake.client();
        let workspace = tempfile::tempdir().unwrap();
        let config_file = workspace.path().join("dokploy.yaml");

        import_resource(&client, request(&kind, config_file.clone()))
            .await
            .unwrap_or_else(|error| panic!("{}: {error:?}", kind.name));
        let plan = plan_workspace(&client, &config_file).await.unwrap();

        assert!(plan.complete(), "{}", kind.name);
        assert!(plan.applyable(), "{}: {:?}", kind.name, plan.diagnostics());
        assert!(plan.changes().is_empty(), "{}", kind.name);
        let source = std::fs::read_to_string(&config_file).unwrap();
        assert_no_identities(&source);
        let config = DokployConfig::parse(&source).expect("config is canonical and valid");
        let resource = config.resource(&kind.address()).unwrap();
        assert_eq!(
            resource.server_placement(),
            Some(&Field::Set(ExternalSelector::named("edge-1"))),
            "{}",
            kind.name
        );
        let state = StateStore::new(workspace.path(), fake.instance())
            .unwrap()
            .inspect()
            .unwrap()
            .unwrap();
        let stored = state.resource(&kind.address()).unwrap();
        assert!(stored.is_protected(), "imported services are protected");
        assert_eq!(
            stored.last_applied().as_json()["server"],
            serde_json::json!({"name": "edge-1"}),
            "{}",
            kind.name
        );
        assert_no_identities(&stored.last_applied().as_json().to_string());
        assert!(
            fake.lines()
                .iter()
                .all(|line| line.starts_with("GET /api/")),
            "import is read-only"
        );
    }
}

#[tokio::test]
async fn a_local_or_omitted_placement_stays_unmanaged_and_reads_no_server_collection() {
    for kind in KINDS {
        for omit in [false, true] {
            let fake = Fake::start();
            remote_service(&fake, &kind, None);
            fake.world().omit_server_field = omit;
            let workspace = tempfile::tempdir().unwrap();
            let config_file = workspace.path().join("dokploy.yaml");

            import_resource(&fake.client(), request(&kind, config_file.clone()))
                .await
                .unwrap_or_else(|error| panic!("{}: {error:?}", kind.name));

            let source = std::fs::read_to_string(&config_file).unwrap();
            assert!(!source.contains("server"), "{}", kind.name);
            assert_eq!(fake.count("GET /api/server.all"), 0, "{}", kind.name);
        }
    }
}

#[tokio::test]
async fn unknown_ambiguous_unreadable_or_ungrammatical_servers_fail_closed_without_writing() {
    for kind in KINDS {
        for setup in ["unknown", "ambiguous", "unreadable", "padded"] {
            let fake = Fake::start();
            remote_service(&fake, &kind, Some("server-1"));
            match setup {
                "unknown" => fake
                    .world()
                    .servers
                    .retain(|server| server.id != "server-1"),
                "ambiguous" => fake.world().add_server("server-9", "edge-1"),
                "unreadable" => fake.world().servers_unavailable = true,
                _ => fake.world().servers[0].name = " padded ".to_owned(),
            }
            let workspace = tempfile::tempdir().unwrap();
            let config_file = workspace.path().join("dokploy.yaml");

            let error = import_resource(&fake.client(), request(&kind, config_file.clone()))
                .await
                .expect_err("a placement without a unique name cannot be imported");

            assert!(
                matches!(error, ImportError::ExternalAssociation),
                "{} {setup}: {error:?}",
                kind.name
            );
            let rendered = format!("{error} {error:?}");
            assert_no_identities(&rendered);
            assert!(!config_file.exists(), "{} {setup}", kind.name);
            assert!(
                !workspace.path().join(".dokploy/state.json").exists(),
                "{} {setup}",
                kind.name
            );
            assert!(
                fake.lines()
                    .iter()
                    .all(|line| line.starts_with("GET /api/"))
            );
        }
    }
}
