//! The engine against a real Dokploy, for the settings kinds.
//!
//! Run through `scripts/integration/test-engine-live.sh`, which needs Docker. These are the
//! live half of the contract the simulator copies (ADR 0015): every behavior the simulator
//! models for `tag` and `registry` is exercised here against the digest-pinned image.

mod common;

use common::{compile_with, specs};
use dokploy_engine::{ApplyError, Engine, RecoveryAction};
use dokploy_sdk::{Dokploy, OperationRequest, Transport};
use dokploy_state::{DocumentId, StateStore};
use serde_json::{Value, json};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live engine test"))
}

fn guard() {
    assert_eq!(
        std::env::var("DOKPLOY_ENGINE_LIVE_TEST").as_deref(),
        Ok("1"),
        "run this test through scripts/integration/test-engine-live.sh"
    );
}

fn client() -> Dokploy {
    Dokploy::builder()
        .url(required("DOKPLOY_URL"))
        .api_key(required("DOKPLOY_API_KEY"))
        .build()
        .expect("live client configuration is valid")
}

async fn call(client: &Dokploy, request: OperationRequest) -> Value {
    client.call(request).await.expect("the live call succeeds")
}

struct Live {
    engine: Engine<Dokploy>,
    store: StateStore,
    _directory: tempfile::TempDir,
}

impl Live {
    fn new() -> Self {
        let engine = Engine::new(specs(), client()).expect("the engine builds");
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::for_document(
            directory.path().canonicalize().unwrap(),
            engine.instance().clone(),
            DocumentId::Settings,
        )
        .unwrap();

        Self {
            engine,
            store,
            _directory: directory,
        }
    }

    async fn apply(&self, text: &str, env: &[(&str, &str)]) -> Result<usize, ApplyError> {
        let compiled = compile_with(&self.engine, text, env);
        self.engine
            .apply(&compiled, &self.store, |_| true)
            .await
            .map(|summary| summary.applied())
    }

    async fn changes(&self, text: &str, env: &[(&str, &str)]) -> usize {
        let compiled = compile_with(&self.engine, text, env);
        let state = self.store.inspect().unwrap();
        let plan = self
            .engine
            .plan(&compiled, state.as_ref())
            .await
            .expect("plans");
        assert!(plan.applyable(), "{plan:?}");

        plan.changes().len()
    }
}

#[tokio::test]
#[ignore = "applies settings to the local Dokploy"]
async fn a_tag_lives_its_whole_life_through_the_engine() {
    guard();
    let live = Live::new();
    let raw = client();
    assert_eq!(
        call(&raw, OperationRequest::new("tag.all")).await,
        json!([]),
        "a fresh instance"
    );
    let document =
        |color: &str| format!("version: 2\nsettings:\n  tags:\n    blue:\n      color: {color}\n");

    assert_eq!(live.apply(&document("\"#0000ff\""), &[]).await.unwrap(), 1);
    let tags = call(&raw, OperationRequest::new("tag.all")).await;
    assert_eq!(tags[0]["name"], "blue", "the name defaults to the key");
    assert_eq!(tags[0]["color"], "#0000ff");
    assert_eq!(
        live.changes(&document("\"#0000ff\""), &[]).await,
        0,
        "converged"
    );

    assert_eq!(live.apply(&document("\"#ff0000\""), &[]).await.unwrap(), 1);
    assert_eq!(
        call(&raw, OperationRequest::new("tag.all")).await[0]["color"],
        "#ff0000"
    );

    // Someone changes it behind the engine's back.
    let id = call(&raw, OperationRequest::new("tag.all")).await[0]["tagId"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        &raw,
        OperationRequest::new("tag.update").body(json!({"tagId": id, "color": "#00ff00"})),
    )
    .await;
    assert_eq!(
        live.changes(&document("\"#ff0000\""), &[]).await,
        1,
        "drift is a change"
    );
    assert_eq!(live.apply(&document("\"#ff0000\""), &[]).await.unwrap(), 1);
    assert_eq!(
        call(&raw, OperationRequest::new("tag.all")).await[0]["color"],
        "#ff0000"
    );

    // An explicit null clears it.
    assert_eq!(live.apply(&document("null"), &[]).await.unwrap(), 1);
    assert_eq!(
        call(&raw, OperationRequest::new("tag.all")).await[0]["color"],
        Value::Null
    );
    assert_eq!(live.changes(&document("null"), &[]).await, 0);

    // An unmanaged tag with the same name blocks a second workspace from taking it over.
    let other = Live::new();
    assert!(matches!(
        other.apply(&document("null"), &[]).await,
        Err(ApplyError::Blocked { .. })
    ));

    assert_eq!(
        live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap(),
        1
    );
    assert_eq!(
        call(&raw, OperationRequest::new("tag.all")).await,
        json!([])
    );
    assert!(
        live.store
            .inspect()
            .unwrap()
            .unwrap()
            .resources()
            .is_empty()
    );
}

#[tokio::test]
#[ignore = "applies settings to the local Dokploy"]
async fn a_registry_lives_its_whole_life_through_the_engine() {
    guard();
    let host = required("LIVE_REGISTRY_HOST");
    let live = Live::new();
    let raw = client();
    assert_eq!(
        call(&raw, OperationRequest::new("registry.all")).await,
        json!([]),
        "a fresh instance"
    );
    let document = |url: &str, prefix: &str| {
        format!(
            "version: 2\nsettings:\n  registries:\n    local:\n      type: cloud\n      url: {url}\n      username: ci\n      password:\n        env: LIVE_REGISTRY_PASSWORD\n      image_prefix: {prefix}\n"
        )
    };
    let env = [("LIVE_REGISTRY_PASSWORD", "live-password-canary-1")];

    // Create runs `docker login`, which the local registry accepts.
    assert_eq!(live.apply(&document(&host, "acme"), &env).await.unwrap(), 1);
    let all = call(&raw, OperationRequest::new("registry.all")).await;
    assert_eq!(all[0]["registryName"], "local");
    assert_eq!(all[0]["imagePrefix"], "acme");
    assert_eq!(
        live.changes(&document(&host, "acme"), &env).await,
        0,
        "converged, with a secret"
    );

    // A patch changes only the prefix.
    assert_eq!(live.apply(&document(&host, "team"), &env).await.unwrap(), 1);
    let id = all[0]["registryId"].as_str().unwrap().to_owned();
    let one = call(
        &raw,
        OperationRequest::new("registry.one").query("registryId", id.as_str()),
    )
    .await;
    assert_eq!(one["imagePrefix"], "team");
    assert_eq!(one["username"], "ci", "the patch kept the other fields");

    // Rotating the secret sends the new one (the login runs again and succeeds).
    let rotated = [("LIVE_REGISTRY_PASSWORD", "live-password-canary-2")];
    assert_eq!(
        live.apply(&document(&host, "team"), &rotated)
            .await
            .unwrap(),
        1
    );
    let listed = call(&raw, OperationRequest::new("registry.all")).await;
    assert_eq!(listed[0]["password"], "live-password-canary-2");

    // A URL Dokploy cannot log in to is rejected, and Dokploy saves it anyway.
    let error = live
        .apply(&document("unreachable.invalid", "team"), &rotated)
        .await
        .unwrap_err();
    assert!(matches!(error, ApplyError::Rejected { .. }), "{error}");
    let one = call(
        &raw,
        OperationRequest::new("registry.one").query("registryId", id.as_str()),
    )
    .await;
    assert_eq!(
        one["registryUrl"], "unreachable.invalid",
        "the rejected change persisted"
    );

    // Recovery closes the failed journal, and the next plan sees the remote already matches.
    let compiled = compile_with(
        &live.engine,
        &document("unreachable.invalid", "team"),
        &rotated,
    );
    let mut action = None;
    live.engine
        .recover(&compiled, &live.store, |preview| {
            action = Some(preview.action());
            true
        })
        .await
        .expect("recovers");
    assert_eq!(action, Some(RecoveryAction::ResolveOperation));
    assert_eq!(
        live.changes(&document("unreachable.invalid", "team"), &rotated)
            .await,
        1,
        "state is behind the remote, which only needs adopting"
    );

    // Put it right and remove it.
    assert_eq!(
        live.apply(&document(&host, "team"), &rotated)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap(),
        1
    );
    assert_eq!(
        call(&raw, OperationRequest::new("registry.all")).await,
        json!([])
    );
}

#[tokio::test]
#[ignore = "applies settings to the local Dokploy"]
async fn a_network_converges_and_is_replaced_through_the_engine() {
    guard();
    let live = Live::new();
    let raw = client();
    assert_eq!(
        call(&raw, OperationRequest::new("network.all")).await,
        json!([]),
        "a fresh instance"
    );
    let document = |internal: bool| {
        format!(
            "version: 2\nsettings:\n  networks:\n    iac-live-backend:\n      driver: bridge\n      internal: {internal}\n"
        )
    };

    assert_eq!(live.apply(&document(false), &[]).await.unwrap(), 1);
    let all = call(&raw, OperationRequest::new("network.all")).await;
    assert_eq!(all[0]["name"], "iac-live-backend");
    assert_eq!(
        all[0]["ipam"],
        json!({}),
        "Dokploy reads an unset IPAM block as an empty object"
    );
    assert_eq!(
        live.changes(&document(false), &[]).await,
        0,
        "converged: an empty IPAM object is not drift, which would replace the network"
    );

    // No update exists: a changed create-only field removes the network and makes it again.
    live.apply(&document(true), &[]).await.unwrap();
    let all = call(&raw, OperationRequest::new("network.all")).await;
    assert_eq!(all.as_array().unwrap().len(), 1);
    assert_eq!(all[0]["internal"], true);
    assert_eq!(live.changes(&document(true), &[]).await, 0);

    live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap();
    assert_eq!(
        call(&raw, OperationRequest::new("network.all")).await,
        json!([])
    );
}

#[tokio::test]
#[ignore = "applies settings to the local Dokploy"]
async fn a_destination_lives_its_whole_life_through_the_engine() {
    guard();
    let live = Live::new();
    let raw = client();
    assert_eq!(
        call(&raw, OperationRequest::new("destination.all")).await,
        json!([]),
        "a fresh instance"
    );
    let document = |bucket: &str| {
        format!(
            "version: 2\nsettings:\n  destinations:\n    offsite:\n      bucket: {bucket}\n      region: us-east-1\n      endpoint: http://s3.invalid:9000\n      access_key:\n        env: LIVE_DESTINATION_ACCESS_KEY\n      secret_access_key:\n        env: LIVE_DESTINATION_SECRET_KEY\n"
        )
    };
    let env = [
        ("LIVE_DESTINATION_ACCESS_KEY", "live-access-canary-1"),
        ("LIVE_DESTINATION_SECRET_KEY", "live-secret-canary-1"),
    ];

    assert_eq!(live.apply(&document("backups"), &env).await.unwrap(), 1);
    let all = call(&raw, OperationRequest::new("destination.all")).await;
    assert_eq!(all[0]["name"], "offsite");
    assert_eq!(all[0]["bucket"], "backups");
    assert_eq!(all[0]["secretAccessKey"], "live-secret-canary-1");
    assert_eq!(
        live.changes(&document("backups"), &env).await,
        0,
        "converged, with two secrets"
    );

    // Update needs every field, so a bucket change resends both secrets.
    assert_eq!(live.apply(&document("archive"), &env).await.unwrap(), 1);
    let all = call(&raw, OperationRequest::new("destination.all")).await;
    assert_eq!(all[0]["bucket"], "archive");
    assert_eq!(all[0]["accessKey"], "live-access-canary-1");
    assert_eq!(all[0]["secretAccessKey"], "live-secret-canary-1");

    let rotated = [
        ("LIVE_DESTINATION_ACCESS_KEY", "live-access-canary-1"),
        ("LIVE_DESTINATION_SECRET_KEY", "live-secret-canary-2"),
    ];
    assert_eq!(live.apply(&document("archive"), &rotated).await.unwrap(), 1);
    assert_eq!(
        call(&raw, OperationRequest::new("destination.all")).await[0]["secretAccessKey"],
        "live-secret-canary-2"
    );

    live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap();
    assert_eq!(
        call(&raw, OperationRequest::new("destination.all")).await,
        json!([])
    );
}
