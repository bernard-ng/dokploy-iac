//! The engine against a real Dokploy: the settings kinds, and one project document.
//!
//! Run through `scripts/integration/test-engine-live.sh`, which needs Docker. These are the
//! live half of the contract the simulator copies (ADR 0015): every behavior the simulator
//! models for `tag` and `registry` is exercised here against the digest-pinned image.

mod common;

use common::{compile_in, compile_with, specs};
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
    directory: tempfile::TempDir,
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
            directory,
        }
    }

    /// A file of the workspace, which a `file` source names.
    fn write(&self, path: &str, text: &str) {
        let path = self.directory.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    async fn apply(&self, text: &str, env: &[(&str, &str)]) -> Result<usize, ApplyError> {
        let compiled = compile_in(&self.engine, self.directory.path(), text, env);
        self.engine
            .apply(&compiled, &self.store, |_| true)
            .await
            .map(|summary| summary.applied())
    }

    async fn changes(&self, text: &str, env: &[(&str, &str)]) -> usize {
        let compiled = compile_in(&self.engine, self.directory.path(), text, env);
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

const PROJECT: &str = "version: 2\nproject:\n  slug: iac-live\n  environments:\n    production:\n      applications:\n        api:\n          source: { type: docker, image: \"nginx:alpine\", username: ci, password: { env: LIVE_DOCKER_PASSWORD } }\n          environment:\n            LOG_LEVEL: info\n            API_TOKEN: { secret: { env: LIVE_API_TOKEN } }\n          patches:\n            motd:\n              file_path: README.md\n              type: create\n              content: { file: patches/motd.txt }\n";

/// A project document applied through the engine, its workspace holding the files it names.
struct LiveProject {
    engine: Engine<Dokploy>,
    store: StateStore,
    directory: tempfile::TempDir,
}

impl LiveProject {
    fn new() -> Self {
        let engine = Engine::new(specs(), client()).expect("the engine builds");
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::for_document(
            directory.path().canonicalize().unwrap(),
            engine.instance().clone(),
            DocumentId::Project(dokploy_state::ResourceName::new("iac-live").unwrap()),
        )
        .unwrap();

        Self {
            engine,
            store,
            directory,
        }
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.directory.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn compiled(&self, text: &str, env: &[(&str, &str)]) -> dokploy_engine::Compiled {
        compile_in(&self.engine, self.directory.path(), text, env)
    }

    async fn apply(&self, text: &str, env: &[(&str, &str)]) -> usize {
        let compiled = self.compiled(text, env);
        self.engine
            .apply(&compiled, &self.store, |_| true)
            .await
            .expect("applies")
            .applied()
    }

    async fn changes(&self, text: &str, env: &[(&str, &str)]) -> usize {
        let compiled = self.compiled(text, env);
        let state = self.store.inspect().unwrap();
        let plan = self
            .engine
            .plan(&compiled, state.as_ref())
            .await
            .expect("plans");
        assert!(plan.applyable(), "{:?}", plan.diagnostics());

        plan.changes().len()
    }
}

#[tokio::test]
#[ignore = "applies a project to the local Dokploy"]
async fn an_environment_value_and_a_content_edited_at_dokploy_are_drift_the_engine_repairs() {
    guard();
    let live = LiveProject::new();
    let raw = client();
    let env = [
        ("LIVE_API_TOKEN", "live-token-canary-1"),
        ("LIVE_DOCKER_PASSWORD", "live-docker-canary-1"),
    ];
    live.write("patches/motd.txt", "hello from the repository");

    live.apply(PROJECT, &env).await;
    assert_eq!(
        live.changes(PROJECT, &env).await,
        0,
        "converged against a real Dokploy"
    );
    let applications = call(&raw, OperationRequest::new("project.all")).await;
    let application = applications
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|project| project["environments"].as_array().unwrap().clone())
        .flat_map(|environment| environment["applications"].as_array().unwrap().clone())
        .find(|application| application["name"] == "api")
        .expect("the application exists");
    let application_id = application["applicationId"].as_str().unwrap().to_owned();
    let held = call(
        &raw,
        OperationRequest::new("application.one").query("applicationId", application_id.as_str()),
    )
    .await;
    assert!(
        held["env"].as_str().unwrap().contains("LOG_LEVEL=info"),
        "Dokploy returns the environment text as written"
    );

    // Someone edits a variable in the dashboard.
    call(
        &raw,
        OperationRequest::new("application.saveEnvironment").body(json!({
            "applicationId": application_id,
            "env": "LOG_LEVEL=debug\nAPI_TOKEN=live-token-canary-1\nSTRANGER=1",
            "buildArgs": null, "buildSecrets": null, "createEnvFile": false
        })),
    )
    .await;
    assert_eq!(
        live.changes(PROJECT, &env).await,
        1,
        "the edited value is seen"
    );
    live.apply(PROJECT, &env).await;
    let held = call(
        &raw,
        OperationRequest::new("application.one").query("applicationId", application_id.as_str()),
    )
    .await;
    let text = held["env"].as_str().unwrap();
    assert!(text.contains("LOG_LEVEL=info"), "{text}");
    assert!(
        text.contains("STRANGER=1"),
        "a variable the document never owned stays: {text}"
    );
    assert_eq!(live.changes(PROJECT, &env).await, 0);

    // Someone edits the patch in the dashboard; Dokploy adds a trailing newline when it stores it.
    let patches = call(
        &raw,
        OperationRequest::new("patch.byEntityId")
            .query("id", application_id.as_str())
            .query("type", "application"),
    )
    .await;
    let patch_id = patches[0]["patchId"].as_str().unwrap().to_owned();
    call(
        &raw,
        OperationRequest::new("patch.update")
            .body(json!({ "patchId": patch_id, "content": "hello from the repository" })),
    )
    .await;
    let stored = call(
        &raw,
        OperationRequest::new("patch.one").query("patchId", patch_id.as_str()),
    )
    .await;
    assert_eq!(
        stored["content"], "hello from the repository\n",
        "Dokploy's update adds the newline"
    );
    assert_eq!(
        live.changes(PROJECT, &env).await,
        0,
        "a trailing newline is not a difference"
    );
    call(
        &raw,
        OperationRequest::new("patch.update")
            .body(json!({ "patchId": patch_id, "content": "edited in the dashboard" })),
    )
    .await;
    assert_eq!(
        live.changes(PROJECT, &env).await,
        1,
        "the edited content is seen"
    );
    live.apply(PROJECT, &env).await;
    let stored = call(
        &raw,
        OperationRequest::new("patch.one").query("patchId", patch_id.as_str()),
    )
    .await;
    assert_eq!(
        stored["content"].as_str().unwrap().trim_end(),
        "hello from the repository",
        "restored (an update stores it with a trailing newline)"
    );
    assert_eq!(live.changes(PROJECT, &env).await, 0);

    // The project goes away with its application.
    let empty = "version: 2\nproject:\n  slug: iac-live\n";
    live.apply(empty, &env).await;
}

#[tokio::test]
#[ignore = "applies settings to the local Dokploy"]
async fn an_ssh_key_certificate_and_ai_provider_live_through_the_engine() {
    guard();
    let live = Live::new();
    let raw = client();

    // An SSH key: created with the organization the API key acts in, renamed in place, and
    // replaced when its private half changes.
    let generate = async || {
        call(
            &raw,
            OperationRequest::new("sshKey.generate").body(json!({ "type": "ed25519" })),
        )
        .await
    };
    let first = generate().await;
    let second = generate().await;
    let ssh = |description: &str| {
        format!(
            "version: 2\nsettings:\n  ssh_keys:\n    deploy:\n      description: {description}\n      public_key: \"{{PUBLIC}}\"\n      private_key:\n        env: LIVE_SSH_PRIVATE\n"
        )
    };
    let document = |description: &str, key: &Value| {
        ssh(description).replace("{PUBLIC}", key["publicKey"].as_str().unwrap())
    };
    fn env_of(key: &Value) -> [(&str, &str); 1] {
        [("LIVE_SSH_PRIVATE", key["privateKey"].as_str().unwrap())]
    }
    assert_eq!(
        live.apply(&document("ci", &first), &env_of(&first))
            .await
            .unwrap(),
        1
    );
    let keys = call(&raw, OperationRequest::new("sshKey.all")).await;
    assert_eq!(keys[0]["name"], "deploy");
    assert_eq!(keys[0]["description"], "ci");
    let first_id = keys[0]["sshKeyId"].clone();
    assert_eq!(
        live.changes(&document("ci", &first), &env_of(&first)).await,
        0
    );
    assert_eq!(
        live.apply(&document("renamed", &first), &env_of(&first))
            .await
            .unwrap(),
        1
    );
    let keys = call(&raw, OperationRequest::new("sshKey.all")).await;
    assert_eq!(
        keys[0]["sshKeyId"], first_id,
        "a description change is in place"
    );
    live.apply(&document("renamed", &second), &env_of(&second))
        .await
        .unwrap();
    let keys = call(&raw, OperationRequest::new("sshKey.all")).await;
    assert_eq!(keys.as_array().unwrap().len(), 1);
    assert_ne!(
        keys[0]["sshKeyId"], first_id,
        "a new key replaces the old one"
    );
    live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap();
    assert_eq!(
        call(&raw, OperationRequest::new("sshKey.all")).await,
        json!([])
    );

    // A certificate: its content is compared with what Dokploy holds.
    let certificate = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "/dev/stdout",
            "-days",
            "30",
            "-subj",
            "/CN=live.invalid",
        ])
        .output()
        .expect("openssl runs");
    let pem = String::from_utf8(certificate.stdout).unwrap();
    let (key_part, cert_part) = pem.split_once("-----BEGIN CERTIFICATE-----").unwrap();
    let cert_text = format!("-----BEGIN CERTIFICATE-----{cert_part}");
    live.write("certs/wildcard.pem", &cert_text);
    let cert_document = "version: 2\nsettings:\n  certificates:\n    wildcard:\n      certificate_data: { file: certs/wildcard.pem }\n      private_key: { env: LIVE_CERT_KEY }\n";
    let cert_env = [("LIVE_CERT_KEY", key_part.trim())];
    assert_eq!(live.apply(cert_document, &cert_env).await.unwrap(), 1);
    assert_eq!(live.changes(cert_document, &cert_env).await, 0, "converged");
    let certificates = call(&raw, OperationRequest::new("certificates.all")).await;
    let certificate_id = certificates[0]["certificateId"].clone();
    call(
        &raw,
        OperationRequest::new("certificates.update").body(json!({
            "certificateId": certificate_id, "certificateData": "edited in the dashboard"
        })),
    )
    .await;
    assert_eq!(
        live.changes(cert_document, &cert_env).await,
        1,
        "an edit at Dokploy is seen"
    );
    live.apply(cert_document, &cert_env).await.unwrap();
    assert_eq!(live.changes(cert_document, &cert_env).await, 0);
    live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap();

    // An AI provider: its update is a full write that resends the key.
    let ai = |model: &str| {
        format!(
            "version: 2\nsettings:\n  ai_providers:\n    openai:\n      api_url: https://api.openai.invalid/v1\n      api_key:\n        env: LIVE_AI_KEY\n      model: {model}\n      enabled: true\n"
        )
    };
    let ai_env = [("LIVE_AI_KEY", "live-ai-canary-1")];
    assert_eq!(live.apply(&ai("gpt-4.1"), &ai_env).await.unwrap(), 1);
    assert_eq!(live.changes(&ai("gpt-4.1"), &ai_env).await, 0);
    assert_eq!(live.apply(&ai("gpt-5"), &ai_env).await.unwrap(), 1);
    let providers = call(&raw, OperationRequest::new("ai.getAll")).await;
    assert_eq!(providers[0]["model"], "gpt-5");
    assert_eq!(
        providers[0]["apiKey"], "live-ai-canary-1",
        "the update resent the key"
    );
    live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap();
    assert_eq!(
        call(&raw, OperationRequest::new("ai.getAll")).await,
        json!([])
    );
}

#[tokio::test]
#[ignore = "applies settings to the local Dokploy"]
async fn a_dns_provider_lives_through_every_arm_of_its_config() {
    guard();
    let live = Live::new();
    let raw = client();
    let env = [
        ("LIVE_DNS_ONE", "live-dns-canary-1"),
        ("LIVE_DNS_TWO", "live-dns-canary-2"),
    ];
    let document = |config: &str| {
        format!("version: 2\nsettings:\n  dns_providers:\n    edge:\n      config:\n{config}")
    };
    let arms = [
        (
            "cloudflare",
            "        type: cloudflare\n        api_token: { env: LIVE_DNS_ONE }\n",
        ),
        (
            "route53",
            "        type: route53\n        access_key_id: AKIALIVECANARY\n        secret_access_key: { env: LIVE_DNS_ONE }\n",
        ),
        (
            "porkbun",
            "        type: porkbun\n        api_key: { env: LIVE_DNS_ONE }\n        secret_api_key: { env: LIVE_DNS_TWO }\n",
        ),
        (
            "infomaniak",
            "        type: infomaniak\n        api_token: { env: LIVE_DNS_ONE }\n",
        ),
        (
            "ovh",
            "        type: ovh\n        endpoint: ovh-eu\n        application_key: live-application-key\n        application_secret: { env: LIVE_DNS_ONE }\n        consumer_key: { env: LIVE_DNS_TWO }\n",
        ),
    ];

    let mut first = true;
    for (arm, config) in arms {
        let text = document(config);
        assert_eq!(live.apply(&text, &env).await.unwrap(), 1, "{arm}");
        let providers = call(&raw, OperationRequest::new("dnsProvider.all")).await;
        assert_eq!(providers.as_array().unwrap().len(), 1, "{arm}");
        assert_eq!(providers[0]["providerType"], arm);
        assert_eq!(providers[0]["name"], "edge");
        assert_eq!(
            live.changes(&text, &env).await,
            0,
            "{arm} converges (its secrets are masked, never compared)"
        );
        if first {
            first = false;
        } else {
            assert_ne!(
                providers[0]["config"]["providerType"], "cloudflare",
                "{arm}"
            );
        }
    }

    // A secret that is rotated is sent again, whole object included.
    let rotated = [
        ("LIVE_DNS_ONE", "live-dns-canary-rotated"),
        ("LIVE_DNS_TWO", "live-dns-canary-2"),
    ];
    let ovh = document(arms[4].1);
    assert_eq!(live.apply(&ovh, &rotated).await.unwrap(), 1);

    live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap();
    assert_eq!(
        call(&raw, OperationRequest::new("dnsProvider.all")).await,
        json!([])
    );
}

#[tokio::test]
#[ignore = "applies settings to the local Dokploy"]
async fn notifications_live_through_every_provider() {
    guard();
    let live = Live::new();
    let raw = client();
    let env = [
        (
            "LIVE_NOTIFICATION_SECRET",
            "https://hooks.invalid/live-canary",
        ),
        (
            "LIVE_NOTIFICATION_HEADERS",
            r#"{"Authorization":"Bearer live-canary"}"#,
        ),
    ];
    let document = |deploy: bool| {
        format!(
            r##"version: 2
settings:
  notifications:
    slack:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: slack, webhook_url: {{ env: LIVE_NOTIFICATION_SECRET }}, channel: "#ops" }}
    telegram:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: telegram, bot_token: {{ env: LIVE_NOTIFICATION_SECRET }}, chat_id: "-100123", message_thread_id: "" }}
    discord:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: discord, webhook_url: {{ env: LIVE_NOTIFICATION_SECRET }}, decoration: true }}
    email:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: email, smtp_server: smtp.invalid, smtp_port: 587, username: mailer, password: {{ env: LIVE_NOTIFICATION_SECRET }}, from_address: dokploy@example.test, to_addresses: [ops@example.test] }}
    resend:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: resend, api_key: {{ env: LIVE_NOTIFICATION_SECRET }}, from_address: dokploy@example.test, to_addresses: [ops@example.test] }}
    gotify:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: gotify, server_url: https://gotify.invalid, app_token: {{ env: LIVE_NOTIFICATION_SECRET }}, priority: 5, decoration: true }}
    ntfy:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: ntfy, server_url: https://ntfy.invalid, topic: live, access_token: {{ env: LIVE_NOTIFICATION_SECRET }}, priority: 3 }}
    mattermost:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: mattermost, webhook_url: {{ env: LIVE_NOTIFICATION_SECRET }}, channel: ops, username: dokploy }}
    custom:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: custom, endpoint: https://custom.invalid/dokploy, headers: {{ env: LIVE_NOTIFICATION_HEADERS }} }}
    lark:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: lark, webhook_url: {{ env: LIVE_NOTIFICATION_SECRET }} }}
    teams:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: teams, webhook_url: {{ env: LIVE_NOTIFICATION_SECRET }} }}
    pushover:
      events: {{ app_deploy: {deploy} }}
      config: {{ type: pushover, user_key: {{ env: LIVE_NOTIFICATION_SECRET }}, api_token: {{ env: LIVE_NOTIFICATION_SECRET }}, priority: 0, retry: 60, expire: 3600 }}
"##
        )
    };

    assert_eq!(live.apply(&document(true), &env).await.unwrap(), 12);
    let notifications = call(&raw, OperationRequest::new("notification.all")).await;
    let held_types: std::collections::BTreeSet<&str> = notifications
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|notification| notification["notificationType"].as_str())
        .collect();
    assert_eq!(
        held_types,
        [
            "slack",
            "telegram",
            "discord",
            "email",
            "resend",
            "gotify",
            "ntfy",
            "mattermost",
            "custom",
            "lark",
            "teams",
            "pushover",
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(live.changes(&document(true), &env).await, 0);
    assert_eq!(live.apply(&document(false), &env).await.unwrap(), 12);
    assert!(
        call(&raw, OperationRequest::new("notification.all"))
            .await
            .as_array()
            .unwrap()
            .iter()
            .all(|notification| notification["appDeploy"] == false)
    );

    live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap();
    assert_eq!(
        call(&raw, OperationRequest::new("notification.all")).await,
        json!([])
    );
}

#[tokio::test]
#[ignore = "applies settings to the local Dokploy"]
async fn a_vault_provider_lives_through_every_arm_and_resolves_assignments() {
    guard();
    let live = Live::new();
    let raw = client();
    let project = call(
        &raw,
        OperationRequest::new("project.create").body(json!({ "name": "vault-assignment-project" })),
    )
    .await;
    let project_id = project["project"]["projectId"].as_str().unwrap();
    let environment_id = project["environment"]["environmentId"].as_str().unwrap();
    let env = [("LIVE_VAULT_SECRET", "live-vault-canary")];
    let document = |config: &str| {
        format!(
            "version: 2\nsettings:\n  vault_providers:\n    secrets:\n      config:\n{config}      assignments:\n        vault-assignment-project:\n          - {{ name: production }}\n"
        )
    };
    let arms = [
        "        type: hashicorp\n        url: https://vault.invalid\n        token: { env: LIVE_VAULT_SECRET }\n        namespace: ops\n        mount: secret\n",
        "        type: infisical\n        site_url: https://app.infisical.com\n        client_id: live-client\n        client_secret: { env: LIVE_VAULT_SECRET }\n        project_id: live-project\n        environment_slug: staging\n        secret_path: /\n",
        "        type: aws\n        region: us-east-1\n        access_key_id: AKIALIVECANARY\n        secret_access_key: { env: LIVE_VAULT_SECRET }\n        endpoint: https://aws.invalid\n",
        "        type: aws-parameter-store\n        region: us-east-1\n        access_key_id: AKIALIVECANARY\n        secret_access_key: { env: LIVE_VAULT_SECRET }\n        endpoint: https://ssm.invalid\n        parameter_path: /dokploy\n",
        "        type: doppler\n        service_token: { env: LIVE_VAULT_SECRET }\n        project: live\n        config: staging\n",
        "        type: azure\n        vault_uri: https://live.vault.azure.net\n        tenant_id: live-tenant\n        client_id: live-client\n        client_secret: { env: LIVE_VAULT_SECRET }\n",
        "        type: scaleway\n        region: fr-par\n        project_id: live-project\n        secret_key: { env: LIVE_VAULT_SECRET }\n        api_url: https://api.scaleway.com\n",
        "        type: phase\n        token: { env: LIVE_VAULT_SECRET }\n        app_id: live-app\n        environment_slug: staging\n        path: /\n        api_url: https://api.phase.dev\n",
    ];

    for (index, config) in arms.into_iter().enumerate() {
        let text = document(config);
        assert_eq!(live.apply(&text, &env).await.unwrap(), 1, "arm {index}");
        let providers = call(&raw, OperationRequest::new("vaultProvider.all")).await;
        assert_eq!(providers.as_array().unwrap().len(), 1);
        assert_eq!(providers[0]["assignments"][0]["projectId"], project_id);
        assert_eq!(
            providers[0]["assignments"][0]["environmentIds"][0],
            environment_id
        );
        assert_eq!(live.changes(&text, &env).await, 0, "arm {index}");
    }

    live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap();
    call(
        &raw,
        OperationRequest::new("project.remove").body(json!({ "projectId": project_id })),
    )
    .await;
}

#[tokio::test]
#[ignore = "applies settings to the local Dokploy"]
async fn a_server_selects_its_ssh_key_by_name_and_is_updated_in_place() {
    guard();
    let live = Live::new();
    let raw = client();
    let key = call(
        &raw,
        OperationRequest::new("sshKey.generate").body(json!({ "type": "ed25519" })),
    )
    .await;
    let env = [("LIVE_SERVER_KEY", key["privateKey"].as_str().unwrap())];
    let document = |port: u16, description: &str, concurrency: u32| {
        format!(
            "version: 2\nsettings:\n  ssh_keys:\n    deploy:\n      public_key: \"{}\"\n      private_key:\n        env: LIVE_SERVER_KEY\n  servers:\n    build-1:\n      description: {description}\n      ip_address: 203.0.113.50\n      port: {port}\n      username: root\n      ssh_key: {{ name: deploy }}\n      type: build\n      docker_cleanup: true\n      command: echo live\n      builds_concurrency: {concurrency}\n",
            key["publicKey"].as_str().unwrap()
        )
    };

    // The key is made before the server, and the server names it.
    assert_eq!(
        live.apply(&document(22, "first", 2), &env).await.unwrap(),
        2,
        "the key, then the server (whose create carries the rest in its own step)"
    );
    let servers = call(&raw, OperationRequest::new("server.all")).await;
    assert_eq!(servers[0]["name"], "build-1");
    assert_eq!(servers[0]["port"], 22);
    assert_eq!(servers[0]["buildsConcurrency"], 2);
    assert_eq!(servers[0]["command"], "echo live");
    let keys = call(&raw, OperationRequest::new("sshKey.all")).await;
    assert_eq!(servers[0]["sshKeyId"], keys[0]["sshKeyId"]);
    assert_eq!(
        live.changes(&document(22, "first", 2), &env).await,
        0,
        "converged"
    );

    // An update needs every field, so it resends them all, and leaves the concurrency alone.
    live.apply(&document(2222, "second", 2), &env)
        .await
        .unwrap();
    let servers = call(&raw, OperationRequest::new("server.all")).await;
    assert_eq!(servers[0]["port"], 2222);
    assert_eq!(servers[0]["description"], "second");
    assert_eq!(servers[0]["buildsConcurrency"], 2);
    live.apply(&document(2222, "second", 4), &env)
        .await
        .unwrap();
    assert_eq!(
        call(&raw, OperationRequest::new("server.all")).await[0]["buildsConcurrency"],
        4
    );

    // The server goes before the key it names.
    live.apply("version: 2\nsettings: {}\n", &[]).await.unwrap();
    assert_eq!(
        call(&raw, OperationRequest::new("server.all")).await,
        json!([])
    );
    assert_eq!(
        call(&raw, OperationRequest::new("sshKey.all")).await,
        json!([])
    );
}
