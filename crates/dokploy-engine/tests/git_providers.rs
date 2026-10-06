//! Git providers use one shared collection and canonical id, with subtype ids for direct reads.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{compile, engine, specs};
use dokploy_engine::Engine;
use dokploy_sim::Sim;
use dokploy_state::{DocumentId, StateStore};
use serde_json::json;

const DOCUMENT: &str = r#"
version: 2
settings:
  gitlab_providers:
    gitlab:
      url: https://gitlab.invalid
      internal_url: http://gitlab.internal
      application_id: gitlab-app
      secret: { env: GITLAB_SECRET }
      group_name: acme
      redirect_uri: https://dokploy.invalid/gitlab/callback
      shared_with_organization: true
  bitbucket_providers:
    bitbucket:
      username: ci
      email: ci@example.com
      app_password: { env: BITBUCKET_PASSWORD }
      api_token: { env: BITBUCKET_TOKEN }
      workspace: acme
      shared_with_organization: true
  gitea_providers:
    gitea:
      url: https://gitea.invalid
      internal_url: http://gitea.internal
      redirect_uri: https://dokploy.invalid/gitea/callback
      client_id: gitea-client
      client_secret: { env: GITEA_SECRET }
      access_token: { env: GITEA_TOKEN }
      shared_with_organization: true
"#;

const ENVIRONMENT: &[(&str, &str)] = &[
    ("GITLAB_SECRET", "gitlab-secret"),
    ("BITBUCKET_PASSWORD", "bitbucket-password"),
    ("BITBUCKET_TOKEN", "bitbucket-token"),
    ("GITEA_SECRET", "gitea-secret"),
    ("GITEA_TOKEN", "gitea-token"),
];

struct World {
    sim: Sim,
    directory: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6");
        Self {
            sim: Sim::new(Arc::new(specs())).with_fixtures(&fixtures),
            directory: tempfile::tempdir().unwrap(),
        }
    }

    fn engine(&self) -> Engine<&Sim> {
        engine(&self.sim)
    }

    fn store(&self, engine: &Engine<&Sim>) -> StateStore {
        StateStore::for_document(
            self.directory.path().canonicalize().unwrap(),
            engine.instance().clone(),
            DocumentId::Settings,
        )
        .unwrap()
    }

    async fn apply(&self, document: &str) -> dokploy_engine::ApplySummary {
        let engine = self.engine();
        let store = self.store(&engine);
        let compiled = compile(&engine, document, ENVIRONMENT);

        engine.apply(&compiled, &store, |_| true).await.unwrap()
    }
}

#[tokio::test]
async fn each_provider_keeps_the_canonical_id_through_create_update_and_remove() {
    let world = World::new();

    assert_eq!(world.apply(DOCUMENT).await.applied(), 3);
    for (kind, subtype_id) in [
        ("gitlab_provider", "gitlabId"),
        ("bitbucket_provider", "bitbucketId"),
        ("gitea_provider", "giteaId"),
    ] {
        let objects = world.sim.objects(kind);
        assert_eq!(objects.len(), 1, "one {kind}");
        assert!(objects[0]["gitProviderId"].as_str().is_some());
        assert!(objects[0][subtype_id].as_str().is_some());
    }

    world.sim.clear_requests();
    let changed = DOCUMENT
        .replace("group_name: acme", "group_name: platform")
        .replace("workspace: acme", "workspace: platform")
        .replace("client_id: gitea-client", "client_id: gitea-platform");
    assert_eq!(world.apply(&changed).await.applied(), 3);

    let updates = world.sim.mutations();
    assert_eq!(updates.len(), 3, "one full update per provider");
    for update in updates {
        let body = update.body().unwrap();
        assert!(body["gitProviderId"].as_str().is_some());
        let subtype_id = match update.operation() {
            "gitlab.update" => "gitlabId",
            "bitbucket.update" => "bitbucketId",
            "gitea.update" => "giteaId",
            operation => panic!("unexpected update {operation}"),
        };
        assert!(body[subtype_id].as_str().is_some());
    }

    assert_eq!(world.apply("version: 2\nsettings: {}\n").await.applied(), 3);
    assert!(world.sim.objects("gitlab_provider").is_empty());
    assert!(world.sim.objects("bitbucket_provider").is_empty());
    assert!(world.sim.objects("gitea_provider").is_empty());
}

#[tokio::test]
async fn a_project_source_selects_the_generic_provider_id() {
    let world = common::project::ProjectWorld::new();
    let provider_id = world.sim.seed(
        "gitlab_provider",
        &json!({
            "name": "gitlab",
            "gitlabUrl": "https://gitlab.invalid",
        }),
    );
    let document = r#"
version: 2
project:
  slug: shop
  environments:
    production:
      applications:
        api:
          source:
            type: gitlab
            provider: { name: gitlab }
            branch: main
"#;

    world.apply_text(document, &[]).await.unwrap();

    let create = world
        .sim
        .mutations()
        .into_iter()
        .find(|request| request.operation() == "application.saveGitlabProvider")
        .expect("the GitLab source was saved");
    assert_eq!(create.body().unwrap()["gitlabId"], provider_id);
}
