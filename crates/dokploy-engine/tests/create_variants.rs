//! A union arm can select the operation that creates a resource.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use dokploy_engine::{Engine, WorkspaceSecrets};
use dokploy_spec::{SpecRegistry, parse_spec};
use dokploy_state::{DocumentId, StateStore};

const SPEC: &str = r#"
kind: notification
scope: settings
section: notifications
title: Notice
identity: { key: name, collision: [name], address: "notification.{key}" }
api:
  id: notificationId
  create:
    by_variant: config
    ops: { custom: notification.createCustom, pushover: notification.createPushover }
  remove: { op: notification.remove }
  read: { list: { op: notification.all }, one: { op: notification.one, id_param: notificationId } }
  create_identity: { diff_collection: { key: [name] } }
fields:
  name: { type: text, default: key }
  config:
    api: notificationType
    type: union(type)
    arms:
      custom:
        provider_id: { api: customId, request: customId, type: text, mutability: computed }
        endpoint: { type: text }
      pushover:
        provider_id: { api: pushoverId, request: pushoverId, type: text, mutability: computed }
        user_key: { api: userKey, type: text, class: secret, mutability: write_only }
        api_token: { api: apiToken, type: text, class: secret, mutability: write_only }
write:
  - by_variant: config
    fields: [name]
    ops: { custom: notification.updateCustom, pushover: notification.updatePushover }
"#;

#[tokio::test]
async fn the_active_arm_selects_the_create_operation() {
    for (arm, fields, expected_create, expected_update) in [
        (
            "custom",
            "endpoint: https://example.invalid/hook",
            "notification.createCustom",
            "notification.updateCustom",
        ),
        (
            "pushover",
            "user_key: { env: NOTICE_TOKEN }\n        api_token: { env: NOTICE_TOKEN }",
            "notification.createPushover",
            "notification.updatePushover",
        ),
    ] {
        let registry =
            SpecRegistry::from_specs(vec![parse_spec(SPEC).unwrap()], &BTreeSet::new()).unwrap();
        let sim = dokploy_sim::Sim::new(Arc::new(registry.clone()));
        let engine = Engine::new(registry, &sim).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let text = format!(
            "version: 2\nsettings:\n  notifications:\n    ops:\n      config:\n        type: {arm}\n        {fields}\n"
        );
        let document = engine.parse(&text).unwrap();
        let secrets = WorkspaceSecrets::new(directory.path(), |_| Some("canary-token".to_owned()));
        let compiled = engine
            .compile(&document, &engine.fingerprinter(common::key()), &secrets)
            .unwrap();
        let store = StateStore::for_document(
            directory.path().canonicalize().unwrap(),
            engine.instance().clone(),
            DocumentId::Settings,
        )
        .unwrap();

        engine.apply(&compiled, &store, |_| true).await.unwrap();

        assert_eq!(sim.mutations()[0].operation(), expected_create);

        let remote = sim.objects("notification").remove(0);
        let id = remote["notificationId"].as_str().unwrap();
        sim.patch(
            "notification",
            id,
            &serde_json::json!({ (format!("{arm}Id")): format!("provider-{arm}") }),
        );
        sim.clear_requests();
        let changed = text.replacen("      config:", "      name: renamed\n      config:", 1);
        let document = engine.parse(&changed).unwrap();
        let secrets = WorkspaceSecrets::new(directory.path(), |_| Some("canary-token".to_owned()));
        let compiled = engine
            .compile(&document, &engine.fingerprinter(common::key()), &secrets)
            .unwrap();

        engine.apply(&compiled, &store, |_| true).await.unwrap();

        let mutations = sim.mutations();
        let update = mutations.last().unwrap();
        assert_eq!(update.operation(), expected_update);
        assert_eq!(update.body().unwrap()["name"], "renamed");
    }
}
