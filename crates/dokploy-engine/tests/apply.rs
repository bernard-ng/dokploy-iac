//! The executor, exercised on settings kinds against the simulator: create, update by write
//! group, remove, the failure modes, and the guarantee that no secret reaches disk.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{compile, engine, specs};
use dokploy_engine::{ApplyError, Engine};
use dokploy_sim::{Fault, FaultKind, Sim};
use dokploy_state::{DocumentId, StateStore};
use serde_json::{Value, json};

const PASSWORD: &str = "ghcr-password-canary-1";
const ROTATED: &str = "ghcr-password-canary-2";

const DOCUMENT: &str = "\
version: 2
settings:
  registries:
    ghcr:
      type: cloud
      url: ghcr.io
      username: ci
      password:
        env: GHCR_TOKEN
      image_prefix: acme
";

fn sim() -> Sim {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6");
    Sim::new(Arc::new(specs())).with_fixtures(&fixtures)
}

struct Workspace {
    directory: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().expect("a temporary workspace"),
        }
    }

    fn store(&self, engine: &Engine<&Sim>) -> StateStore {
        StateStore::for_document(
            self.directory.path().canonicalize().unwrap(),
            engine.instance().clone(),
            DocumentId::Settings,
        )
        .expect("a state store")
    }

    /// Everything written below the workspace, as text.
    fn everything(&self) -> String {
        fn walk(path: &Path, out: &mut String) {
            let Ok(entries) = std::fs::read_dir(path) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push_str(&text);
                }
            }
        }
        let mut out = String::new();
        walk(self.directory.path(), &mut out);
        out
    }
}

async fn apply(
    engine: &Engine<&Sim>,
    store: &StateStore,
    text: &str,
    token: &str,
) -> Result<dokploy_engine::ApplySummary, ApplyError> {
    let compiled = compile(engine, text, &[("GHCR_TOKEN", token)]);
    engine.apply(&compiled, store, |_| true).await
}

fn only_registry(sim: &Sim) -> Value {
    let mut all = sim.objects("registry");
    assert_eq!(all.len(), 1, "exactly one registry: {all:?}");
    all.remove(0)
}

#[tokio::test]
async fn a_create_sends_the_document_and_the_next_plan_is_empty() {
    let sim = sim();
    let engine = engine(&sim);
    let workspace = Workspace::new();
    let store = workspace.store(&engine);

    let summary = apply(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .expect("applies");
    assert_eq!(summary.applied(), 1);

    let remote = only_registry(&sim);
    assert_eq!(
        remote["registryName"], "ghcr",
        "the name defaults to the key"
    );
    assert_eq!(remote["registryUrl"], "ghcr.io");
    assert_eq!(remote["username"], "ci");
    assert_eq!(remote["imagePrefix"], "acme");
    assert_eq!(remote["password"], PASSWORD, "the secret reached Dokploy");

    let state = store.inspect().unwrap().expect("state was written");
    assert_eq!(state.document(), &DocumentId::Settings);
    let resource = state.resources().values().next().expect("one resource");
    assert_eq!(
        resource.remote_id().as_str(),
        remote["registryId"].as_str().unwrap()
    );

    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", PASSWORD)]);
    let plan = engine.plan(&compiled, Some(&state)).await.unwrap();
    assert!(plan.applyable(), "{plan:?}");
    assert!(plan.changes().is_empty(), "converged: {plan:?}");

    let again = engine.apply(&compiled, &store, |_| true).await.unwrap();
    assert_eq!(again.applied(), 0);
}

#[tokio::test]
async fn an_update_sends_only_the_group_fields_that_changed() {
    let sim = sim();
    let engine = engine(&sim);
    let workspace = Workspace::new();
    let store = workspace.store(&engine);
    apply(&engine, &store, DOCUMENT, PASSWORD).await.unwrap();
    sim.clear_requests();

    let changed = DOCUMENT.replace("username: ci", "username: deploy");
    apply(&engine, &store, &changed, PASSWORD)
        .await
        .expect("applies");

    let mutations = sim.mutations();
    assert_eq!(mutations.len(), 1, "{mutations:?}");
    assert_eq!(mutations[0].operation(), "registry.update");
    let body = mutations[0].body().unwrap();
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["registryId", "username"],
        "a partial group: the id and the change"
    );
    assert_eq!(only_registry(&sim)["username"], "deploy");
    assert_eq!(
        only_registry(&sim)["password"],
        PASSWORD,
        "untouched fields stay"
    );
}

#[tokio::test]
async fn rotating_a_secret_sends_the_new_value_once() {
    let sim = sim();
    let engine = engine(&sim);
    let workspace = Workspace::new();
    let store = workspace.store(&engine);
    apply(&engine, &store, DOCUMENT, PASSWORD).await.unwrap();
    sim.clear_requests();

    apply(&engine, &store, DOCUMENT, ROTATED)
        .await
        .expect("applies");

    let mutations = sim.mutations();
    assert_eq!(mutations.len(), 1);
    assert_eq!(mutations[0].body().unwrap()["password"], ROTATED);
    assert_eq!(only_registry(&sim)["password"], ROTATED);
}

#[tokio::test]
async fn a_resource_removed_from_the_document_is_removed_from_dokploy_and_state() {
    let sim = sim();
    let engine = engine(&sim);
    let workspace = Workspace::new();
    let store = workspace.store(&engine);
    apply(&engine, &store, DOCUMENT, PASSWORD).await.unwrap();

    let empty = "version: 2\nsettings: {}\n";
    let summary = apply(&engine, &store, empty, PASSWORD)
        .await
        .expect("applies");
    assert_eq!(summary.applied(), 1);
    assert!(sim.objects("registry").is_empty());
    assert!(store.inspect().unwrap().unwrap().resources().is_empty());
}

#[tokio::test]
async fn a_declined_plan_changes_nothing() {
    let sim = sim();
    let engine = engine(&sim);
    let workspace = Workspace::new();
    let store = workspace.store(&engine);
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", PASSWORD)]);

    let error = engine
        .apply(&compiled, &store, |_| false)
        .await
        .unwrap_err();
    assert!(matches!(error, ApplyError::Declined), "{error}");
    assert!(sim.mutations().is_empty());
    assert!(sim.objects("registry").is_empty());
}

#[tokio::test]
async fn a_definitive_rejection_fails_the_step_and_checkpoints_nothing() {
    let sim = sim();
    let engine = engine(&sim);
    let workspace = Workspace::new();
    let store = workspace.store(&engine);
    sim.inject(Fault::new(
        "registry.create",
        FaultKind::Reject { status: 400 },
    ));

    let error = apply(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .unwrap_err();
    assert!(matches!(error, ApplyError::Rejected { .. }), "{error}");
    assert!(sim.objects("registry").is_empty());
    let state = store.inspect().unwrap().expect("the lineage was started");
    assert!(state.resources().is_empty(), "no resource was recorded");
}

#[tokio::test]
async fn a_lost_response_is_an_unknown_outcome_and_the_mutation_is_not_retried() {
    let sim = sim();
    let engine = engine(&sim);
    let workspace = Workspace::new();
    let store = workspace.store(&engine);
    sim.inject(Fault::new("registry.create", FaultKind::DropAfter));

    let error = apply(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .unwrap_err();
    assert!(
        matches!(error, ApplyError::OutcomeUnknown { .. }),
        "{error}"
    );
    let creates = sim
        .mutations()
        .iter()
        .filter(|m| m.operation() == "registry.create")
        .count();
    assert_eq!(creates, 1, "never retried");
    assert_eq!(sim.objects("registry").len(), 1, "it did happen");
    assert!(
        store.inspect().unwrap().unwrap().resources().is_empty(),
        "but state was not advanced: recovery decides"
    );
    assert!(
        !matches!(
            store.recovery_status().unwrap(),
            dokploy_state::RecoveryStatus::Clean
        ),
        "the open step requires recovery"
    );
}

#[tokio::test]
async fn a_failed_read_before_the_change_leaves_no_trace() {
    let sim = sim();
    let engine = engine(&sim);
    let workspace = Workspace::new();
    let store = workspace.store(&engine);
    sim.inject(Fault::new("registry.all", FaultKind::Unavailable));

    let error = apply(&engine, &store, DOCUMENT, PASSWORD)
        .await
        .unwrap_err();
    assert!(matches!(error, ApplyError::Blocked { .. }), "{error}");
    assert!(sim.mutations().is_empty());
}

#[tokio::test]
async fn no_secret_reaches_state_journal_errors_or_debug_output() {
    let sim = sim();
    let engine = engine(&sim);
    let workspace = Workspace::new();
    let store = workspace.store(&engine);
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", PASSWORD)]);
    assert!(!format!("{compiled:?}").contains(PASSWORD));

    apply(&engine, &store, DOCUMENT, PASSWORD).await.unwrap();
    apply(&engine, &store, DOCUMENT, ROTATED).await.unwrap();
    sim.inject(Fault::new(
        "registry.update",
        FaultKind::Reject { status: 500 },
    ));
    let error = apply(&engine, &store, &DOCUMENT.replace("ci", "other"), ROTATED)
        .await
        .unwrap_err();

    for secret in [PASSWORD, ROTATED] {
        assert!(
            !workspace.everything().contains(secret),
            "{secret} reached disk"
        );
        assert!(
            !format!("{error} {error:?}").contains(secret),
            "{secret} reached an error"
        );
    }
}

/// A transport that changes what the registry's direct read returns, to model a Dokploy that
/// stores something other than what it was sent.
struct Normalizing<'a>(&'a Sim);

impl dokploy_sdk::Transport for Normalizing<'_> {
    fn base_url(&self) -> &url::Url {
        self.0.base_url()
    }

    async fn call(
        &self,
        request: dokploy_sdk::OperationRequest,
    ) -> Result<Value, dokploy_sdk::Error> {
        let operation = request.operation().to_owned();
        let mut response = self.0.call(request).await?;
        if operation == "registry.one" {
            response["registryUrl"] = json!("normalized.example");
        }
        Ok(response)
    }
}

#[tokio::test]
async fn a_change_that_does_not_read_back_is_reported_after_it_is_recorded() {
    let sim = sim();
    let engine = Engine::new(specs(), Normalizing(&sim)).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = StateStore::for_document(
        workspace.path().canonicalize().unwrap(),
        engine.instance().clone(),
        DocumentId::Settings,
    )
    .unwrap();
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", PASSWORD)]);

    let error = engine.apply(&compiled, &store, |_| true).await.unwrap_err();
    assert!(
        matches!(&error, ApplyError::Verification { property, .. } if property == "url"),
        "{error}"
    );
    let state = store.inspect().unwrap().unwrap();
    assert_eq!(state.resources().len(), 1, "what was written is recorded");
    assert!(
        matches!(
            store.recovery_status().unwrap(),
            dokploy_state::RecoveryStatus::Clean
        ),
        "the journal is closed, not left for recovery"
    );
}

/// A synthetic settings kind on the tag operations with one field that cannot change in
/// place, so the executor's replacement path runs without a real kind having one.
const GADGET: &str = "\
kind: gadget
scope: settings
section: gadgets
title: Gadget
class: managed
identity:
  key: name
  collision: [name]
  address: \"gadget.{key}\"
api:
  id: tagId
  create: { op: tag.create }
  update: { op: tag.update }
  remove: { op: tag.remove }
  read:
    list: { op: tag.all, authority: authoritative }
    one: { op: tag.one, id_param: tagId, agree: true }
  create_identity:
    from_response: /tagId
fields:
  name:
    type: text
    min_len: 1
    default: key
  color:
    type: text
    mutability: create_only
    nullable: true
write:
  - op: tag.update
    fields: [name]
    shape: partial
ledger:
  readonly: [createdAt, organizationId]
";

#[tokio::test]
async fn changing_a_create_only_field_replaces_the_resource_delete_first() {
    let spec = dokploy_spec::parse_spec(GADGET).expect("the spec parses");
    let specs =
        dokploy_spec::SpecRegistry::from_specs(vec![spec], &std::collections::BTreeSet::new())
            .expect("the registry is consistent");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6");
    let sim = Sim::new(Arc::new(specs.clone())).with_fixtures(&fixtures);
    let engine = Engine::new(specs, &sim).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = StateStore::for_document(
        workspace.path().canonicalize().unwrap(),
        engine.instance().clone(),
        DocumentId::Settings,
    )
    .unwrap();
    let document = |color: &str| {
        format!("version: 2\nsettings:\n  gadgets:\n    thing:\n      color: \"{color}\"\n")
    };

    let first = compile(&engine, &document("#111111"), &[]);
    engine
        .apply(&first, &store, |_| true)
        .await
        .expect("creates");
    let first_id = sim.objects("gadget")[0]["tagId"]
        .as_str()
        .unwrap()
        .to_owned();
    sim.clear_requests();

    let second = compile(&engine, &document("#222222"), &[]);
    let summary = engine
        .apply(&second, &store, |_| true)
        .await
        .expect("replaces");

    assert_eq!(summary.applied(), 1);
    let operations: Vec<String> = sim
        .mutations()
        .iter()
        .map(|m| m.operation().to_owned())
        .collect();
    assert_eq!(
        operations,
        ["tag.remove", "tag.create"],
        "delete before create"
    );
    let remote = sim.objects("gadget");
    assert_eq!(remote.len(), 1);
    assert_eq!(remote[0]["color"], "#222222");
    assert_ne!(
        remote[0]["tagId"].as_str().unwrap(),
        first_id,
        "a new identity"
    );
    let state = store.inspect().unwrap().unwrap();
    assert_eq!(
        state
            .resources()
            .values()
            .next()
            .unwrap()
            .remote_id()
            .as_str(),
        remote[0]["tagId"].as_str().unwrap()
    );
}

#[tokio::test]
async fn a_full_group_resends_what_a_fresh_read_returns() {
    let full = GADGET
        .replace(
            "mutability: create_only\n    nullable: true",
            "nullable: true",
        )
        .replace(
            "fields: [name]\n    shape: partial",
            "fields: [name, color]\n    shape: full",
        );
    let spec = dokploy_spec::parse_spec(&full).expect("the spec parses");
    let specs =
        dokploy_spec::SpecRegistry::from_specs(vec![spec], &std::collections::BTreeSet::new())
            .expect("the registry is consistent");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6");
    let sim = Sim::new(Arc::new(specs.clone())).with_fixtures(&fixtures);
    let engine = Engine::new(specs, &sim).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = StateStore::for_document(
        workspace.path().canonicalize().unwrap(),
        engine.instance().clone(),
        DocumentId::Settings,
    )
    .unwrap();
    let document = |description: &str| {
        format!("version: 2\nsettings:\n  gadgets:\n    thing:\n      name: {description}\n")
    };

    // `color` is not in the document, so the document does not manage it; someone set one.
    let initial = compile(&engine, &document("old"), &[]);
    engine
        .apply(&initial, &store, |_| true)
        .await
        .expect("creates");
    let id = sim.objects("gadget")[0]["tagId"]
        .as_str()
        .unwrap()
        .to_owned();
    sim.patch("gadget", &id, &json!({"color": "#abcdef"}));
    sim.clear_requests();

    let renamed = compile(&engine, &document("new"), &[]);
    engine
        .apply(&renamed, &store, |_| true)
        .await
        .expect("applies");

    let updates: Vec<_> = sim
        .mutations()
        .into_iter()
        .filter(|m| m.operation() == "tag.update")
        .collect();
    assert_eq!(updates.len(), 1);
    let body = updates[0].body().unwrap();
    assert_eq!(body["name"], "new");
    assert_eq!(
        body["color"], "#abcdef",
        "the full group re-sent the color it read"
    );
    assert_eq!(body["tagId"], id);
}
