//! The instance web server is a singleton whose disruptive write groups run last.

mod common;

use std::sync::Arc;

use common::{compile_in, engine, specs};
use dokploy_core::ChangeKind;
use dokploy_sdk::{OperationRequest, Transport};
use dokploy_sim::Sim;
use dokploy_state::{DocumentId, StateStore};
use serde_json::json;

const DOCUMENT: &str = r#"
version: 2
settings:
  web_server:
    domain:
      host: dokploy.example.test
      https: true
      certificate_type: letsencrypt
      letsencrypt_email: ops@example.test
    server_ip: 203.0.113.10
    builds_concurrency: 2
    request_logging: true
    cleanup:
      docker: false
      logs_cron: "0 2 * * *"
    traefik:
      dashboard: true
      additional_ports:
        - { target_port: 8080, published_port: 8080, protocol: tcp }
      files:
        iac-contract: { file: traefik/iac-contract.yml }
"#;

#[tokio::test]
async fn the_web_server_is_adopted_and_non_disruptive_groups_run_first() {
    let sim = Sim::new(Arc::new(specs()));
    let current = sim.objects("web_server").into_iter().next().unwrap();
    let id = current["id"].as_str().unwrap();
    sim.patch(
        "web_server",
        id,
        &json!({
            "host": null,
            "https": false,
            "certificateType": "none",
            "letsEncryptEmail": null,
            "serverIp": "127.0.0.1",
            "buildsConcurrency": 1,
            "requestLogging": false,
            "enableDockerCleanup": true,
            "logCleanupCron": "0 0 * * *",
            "traefikDashboard": false,
            "additionalPorts": [],
            "traefikEnv": "",
            "traefikConfig": "entryPoints: {}",
            "webServerTraefikConfig": "http: {}",
            "middlewareTraefikConfig": "http: {}"
        }),
    );
    let engine = engine(&sim);
    let observed = sim
        .call(OperationRequest::new("settings.getWebServerSettings"))
        .await
        .unwrap();
    assert!(observed.is_object(), "{observed:?}");
    assert_eq!(observed["id"], id, "{observed:?}");
    for operation in [
        "settings.haveActivateRequests",
        "settings.haveTraefikDashboardPortEnabled",
        "settings.getTraefikPorts",
        "settings.readTraefikEnv",
        "settings.readTraefikConfig",
        "settings.readWebServerTraefikConfig",
        "settings.readMiddlewareTraefikConfig",
    ] {
        assert!(
            sim.call(OperationRequest::new(operation)).await.is_ok(),
            "{operation}"
        );
    }
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("traefik")).unwrap();
    std::fs::write(
        directory.path().join("traefik/iac-contract.yml"),
        "http: {}\n",
    )
    .unwrap();
    let compiled = compile_in(&engine, directory.path(), DOCUMENT, &[]);
    let store = StateStore::for_document(
        directory.path().canonicalize().unwrap(),
        engine.instance().clone(),
        DocumentId::Settings,
    )
    .unwrap();

    let plan = engine.plan(&compiled, None).await.unwrap();
    assert_eq!(plan.changes().len(), 1, "{plan:?}");
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Adopt);
    assert!(plan.changes()[0].disruptive());
    assert!(plan.has_disruptive_changes());

    engine.apply(&compiled, &store, |_| true).await.unwrap();
    assert_eq!(
        sim.mutations()
            .iter()
            .map(|request| request.operation())
            .collect::<Vec<_>>(),
        [
            "settings.updateBuildsConcurrency",
            "settings.updateDockerCleanup",
            "settings.updateLogCleanup",
            "settings.assignDomainServer",
            "settings.updateServerIp",
            "settings.toggleRequests",
            "settings.toggleDashboard",
            "settings.updateTraefikPorts",
            "settings.updateTraefikFile",
        ]
    );
    let state = store.inspect().unwrap().unwrap();
    assert!(
        engine
            .plan(&compiled, Some(&state))
            .await
            .unwrap()
            .changes()
            .is_empty()
    );

    sim.call(
        OperationRequest::new("settings.updateWebServerTraefikConfig")
            .body(json!({ "traefikConfig": "http:\n  routers: {}\n" })),
    )
    .await
    .unwrap();
    let projected = sim.objects("web_server").remove(0);
    assert_eq!(
        projected["webServerTraefikConfig"],
        "http:\n  routers: {}\n"
    );
    assert_eq!(projected["traefikConfig"], "entryPoints: {}");
    assert_eq!(projected["middlewareTraefikConfig"], "http: {}");
}
