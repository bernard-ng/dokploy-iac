#[path = "support/placement.rs"]
mod fake;

use dokploy_cli::desired::{CompiledDesired, compile_desired_for_instance};
use dokploy_cli::remote::{DiscoveryAuthority, discover_remote};
use dokploy_cli::saved_plan::{SavedPlan, SavedPlanError};
use dokploy_config::DokployConfig;
use dokploy_core::{
    ChangeKind, ConfigDigest, ExternalResolution, ExternalSelectorFailure, Plan,
    PlanDiagnosticCode, PropertyPath, RemoteState, ReplacementOrder, StoredState, plan,
};
use dokploy_state::{ResourceKind, StateFile};
use fake::{
    Fake, KINDS, Kind, LOCAL, SECRET_CANARY, named, placed_state, state_with, write_secrets,
};

const NAME_CANARY: &str = "selector-name-canary-91bc";

struct Discovery {
    remote: RemoteState,
    plan: Plan,
    instance: dokploy_state::InstanceIdentity,
}

fn compile(fake: &Fake, yaml: &str) -> (CompiledDesired, tempfile::TempDir) {
    let workspace = tempfile::tempdir().unwrap();
    write_secrets(workspace.path());
    let config = DokployConfig::parse(yaml).expect("configuration is valid");
    let compiled = compile_desired_for_instance(
        &config,
        ConfigDigest::parse("a".repeat(64)).unwrap(),
        fake.instance(),
        workspace.path(),
    )
    .expect("configuration compiles");
    (compiled, workspace)
}

async fn discover_with(
    fake: &Fake,
    compiled: &CompiledDesired,
    state: &StateFile,
    authority: DiscoveryAuthority,
) -> Discovery {
    let remote = discover_remote(&fake.client(), compiled, state, authority)
        .await
        .expect("discovery succeeds");
    let stored = StoredState::try_from_state(state).expect("state projects");
    let plan = plan(compiled.desired_state(), &stored, &remote);
    Discovery {
        remote,
        plan,
        instance: fake.instance(),
    }
}

async fn discover(fake: &Fake, yaml: &str, state: &StateFile) -> Discovery {
    let (compiled, _workspace) = compile(fake, yaml);
    discover_with(fake, &compiled, state, DiscoveryAuthority::reconciliation()).await
}

/// Registers the remote service that a stored placement observation refers to.
fn remote_service(fake: &Fake, kind: &Kind, server: Option<&str>) {
    let mut fields = serde_json::Map::new();
    for (key, value) in kind.bare_inputs().as_object().unwrap() {
        match key.as_str() {
            "database" => fields.insert("databaseName".into(), value.clone()),
            "username" => fields.insert("databaseUser".into(), value.clone()),
            _ => None,
        };
    }
    fields.insert("description".into(), serde_json::Value::Null);
    fields.insert("replicaSets".into(), serde_json::json!(false));
    fake.world().services.push(fake::Service {
        kind: kind.endpoint,
        id: kind.first_id(),
        name: "main".to_owned(),
        server: server.map(str::to_owned),
        fields,
    });
}

fn assert_no_leaks(text: &str) {
    for canary in [
        SECRET_CANARY,
        NAME_CANARY,
        "server-1",
        "server-2",
        "server-9",
        "203.0.113.7",
    ] {
        assert!(!text.contains(canary), "{canary} leaked");
    }
}

fn api_bytes(plan: &Plan) -> String {
    String::from_utf8(plan.to_json_bytes()).unwrap()
}

#[tokio::test]
async fn a_named_placement_resolves_and_a_stable_plan_has_no_changes() {
    for kind in KINDS {
        let fake = Fake::start();
        remote_service(&fake, &kind, Some("server-1"));
        let state = state_with(
            &fake,
            vec![(
                kind.address().to_string().as_str(),
                placed_state(&kind, &kind.first_id(), "edge-1", true),
            )],
        );

        let discovery = discover(
            &fake,
            &kind.quiet_config(
                &named("edge-1"),
                "        lifecycle:\n          protect: true\n",
            ),
            &state,
        )
        .await;

        assert_eq!(
            discovery
                .remote
                .external_resolution(&kind.address(), &PropertyPath::Server),
            Some(&ExternalResolution::Resolved(
                dokploy_state::RemoteId::new("server-1").unwrap()
            )),
            "{}",
            kind.name
        );
        assert!(
            discovery.plan.applyable(),
            "{}: {:?}",
            kind.name,
            discovery.plan.diagnostics()
        );
        assert!(discovery.plan.changes().is_empty(), "{}", kind.name);
        assert!(discovery.plan.drift().is_empty(), "{}", kind.name);
        assert_eq!(fake.count("GET /api/server.all"), 1, "{}", kind.name);
        assert_no_leaks(&format!("{:?}", discovery.remote));
        assert_no_leaks(&api_bytes(&discovery.plan));
    }
}

#[tokio::test]
async fn the_local_selector_matches_a_null_server_and_an_omitted_field_is_unknown() {
    for kind in KINDS {
        let fake = Fake::start();
        remote_service(&fake, &kind, None);
        let mut stored = placed_state(&kind, &kind.first_id(), "edge-1", true);
        stored = dokploy_state::ResourceState::new(
            kind.resource,
            stored.remote_id().clone(),
            true,
            dokploy_state::ManagedInputs::try_from_json({
                let mut inputs = kind.bare_inputs();
                inputs["server"] = serde_json::json!({"local": true});
                inputs
            })
            .unwrap(),
            stored.containment().cloned(),
            Vec::new(),
        );
        let state = state_with(&fake, vec![(kind.address().to_string().as_str(), stored)]);
        let yaml = kind.quiet_config(LOCAL, "        lifecycle:\n          protect: true\n");

        let discovery = discover(&fake, &yaml, &state).await;

        assert_eq!(
            discovery
                .remote
                .external_resolution(&kind.address(), &PropertyPath::Server),
            Some(&ExternalResolution::Local),
            "{}",
            kind.name
        );
        assert!(
            discovery.plan.applyable(),
            "{}: {:?}",
            kind.name,
            discovery.plan.diagnostics()
        );
        assert!(discovery.plan.changes().is_empty(), "{}", kind.name);

        // A response that omits the field proves nothing about the placement.
        fake.world().omit_server_field = true;
        let unknown = discover(&fake, &yaml, &state).await;
        assert!(!unknown.plan.applyable(), "{}", kind.name);
        assert_eq!(
            unknown.plan.diagnostics()[0].code(),
            PlanDiagnosticCode::UnknownPropertyObservation,
            "{}",
            kind.name
        );
    }
}

#[tokio::test]
async fn a_changed_server_replaces_delete_before_create_unless_protected() {
    for kind in KINDS {
        let fake = Fake::start();
        remote_service(&fake, &kind, Some("server-1"));
        // Compose cannot be unprotected without its document secret, so its replacement
        // plan is covered by the executor suite; the protected plan is covered for all.
        for protected in [true, false] {
            if !protected && kind.resource == ResourceKind::Compose {
                continue;
            }
            let state = state_with(
                &fake,
                vec![(
                    kind.address().to_string().as_str(),
                    placed_state(&kind, &kind.first_id(), "edge-1", protected),
                )],
            );
            let lifecycle = if protected {
                "        lifecycle:\n          protect: true\n"
            } else {
                ""
            };
            let yaml = kind.quiet_config(&named("edge-2"), lifecycle);

            let discovery = discover(&fake, &yaml, &state).await;

            if protected {
                assert!(!discovery.plan.applyable(), "{}", kind.name);
                assert_eq!(
                    discovery.plan.diagnostics()[0].code(),
                    PlanDiagnosticCode::ProtectedDelete,
                    "{}",
                    kind.name
                );
                assert!(discovery.plan.changes().is_empty());
            } else {
                assert!(
                    discovery.plan.applyable(),
                    "{}: {:?}",
                    kind.name,
                    discovery.plan.diagnostics()
                );
                assert_eq!(discovery.plan.changes().len(), 1, "{}", kind.name);
                let change = &discovery.plan.changes()[0];
                assert_eq!(change.kind(), ChangeKind::Replace, "{}", kind.name);
                assert_eq!(
                    change.replacement_order(),
                    Some(ReplacementOrder::DeleteBeforeCreate)
                );
            }
            assert_no_leaks(&api_bytes(&discovery.plan));
        }
    }
}

#[tokio::test]
async fn zero_multiple_or_unreadable_matches_block_with_a_value_free_diagnostic() {
    for kind in KINDS {
        for (setup, expected) in [
            ("unmatched", ExternalSelectorFailure::Unmatched),
            ("ambiguous", ExternalSelectorFailure::Ambiguous),
            ("unavailable", ExternalSelectorFailure::Unavailable),
        ] {
            let fake = Fake::start();
            match setup {
                "ambiguous" => fake.world().add_server("server-9", "edge-1"),
                "unavailable" => fake.world().servers_unavailable = true,
                _ => {}
            }
            let name = if setup == "unmatched" {
                NAME_CANARY
            } else {
                "edge-1"
            };
            let state = state_with(&fake, Vec::new());

            let discovery = discover(&fake, &kind.config(&named(name), ""), &state).await;

            assert!(!discovery.plan.applyable(), "{} {setup}", kind.name);
            assert!(discovery.plan.changes().is_empty(), "{} {setup}", kind.name);
            let diagnostic = &discovery.plan.diagnostics()[0];
            assert_eq!(
                diagnostic.code(),
                PlanDiagnosticCode::UnresolvedExternalSelector,
                "{} {setup}",
                kind.name
            );
            assert_eq!(diagnostic.selector_failure(), Some(expected));
            assert_eq!(diagnostic.property(), Some(&PropertyPath::Server));
            assert_no_leaks(&api_bytes(&discovery.plan));
            assert_no_leaks(&format!("{:?}", discovery.remote));
        }
    }
}

#[tokio::test]
async fn duplicate_names_stay_visible_and_response_order_never_selects_a_record() {
    for kind in KINDS {
        for order in [false, true] {
            let fake = Fake::start();
            {
                let mut world = fake.world();
                world.add_server("server-9", "edge-1");
                if order {
                    world.servers.reverse();
                }
            }
            remote_service(&fake, &kind, Some("server-1"));
            let state = state_with(
                &fake,
                vec![(
                    kind.address().to_string().as_str(),
                    placed_state(&kind, &kind.first_id(), "edge-1", true),
                )],
            );

            let discovery = discover(
                &fake,
                &kind.quiet_config(
                    &named("edge-1"),
                    "        lifecycle:\n          protect: true\n",
                ),
                &state,
            )
            .await;

            assert_eq!(
                discovery
                    .remote
                    .external_resolution(&kind.address(), &PropertyPath::Server),
                Some(&ExternalResolution::Ambiguous),
                "{}",
                kind.name
            );
            assert!(!discovery.plan.applyable());
        }
    }
}

#[tokio::test]
async fn an_attached_identity_missing_from_the_collection_blocks_planning() {
    for kind in KINDS {
        let fake = Fake::start();
        remote_service(&fake, &kind, Some("server-gone"));
        let state = state_with(
            &fake,
            vec![(
                kind.address().to_string().as_str(),
                placed_state(&kind, &kind.first_id(), "edge-1", true),
            )],
        );

        let discovery = discover(
            &fake,
            &kind.quiet_config(
                &named("edge-1"),
                "        lifecycle:\n          protect: true\n",
            ),
            &state,
        )
        .await;

        assert!(!discovery.plan.applyable(), "{}", kind.name);
        assert_eq!(
            discovery.plan.diagnostics()[0].code(),
            PlanDiagnosticCode::UnknownPropertyObservation,
            "{}",
            kind.name
        );
        assert_no_leaks(&api_bytes(&discovery.plan));
    }
}

#[tokio::test]
async fn ignored_and_unmanaged_placements_perform_no_external_reads() {
    for kind in KINDS {
        let fake = Fake::start();
        remote_service(&fake, &kind, Some("server-2"));
        let state = state_with(
            &fake,
            vec![(
                kind.address().to_string().as_str(),
                placed_state(&kind, &kind.first_id(), "edge-1", true),
            )],
        );
        let ignored = discover(
            &fake,
            &kind.quiet_config(
                &named("edge-1"),
                "        lifecycle:\n          protect: true\n          ignore_changes: [server]\n",
            ),
            &state,
        )
        .await;
        assert!(ignored.plan.applyable(), "{}", kind.name);
        assert!(ignored.plan.changes().is_empty(), "{}", kind.name);
        assert_eq!(fake.count("GET /api/server.all"), 0, "{}", kind.name);
        assert!(
            ignored
                .remote
                .external_resolution(&kind.address(), &PropertyPath::Server)
                .is_none()
        );

        let plain_state = state_with(
            &fake,
            vec![(
                kind.address().to_string().as_str(),
                dokploy_state::ResourceState::new(
                    kind.resource,
                    dokploy_state::RemoteId::new(kind.first_id()).unwrap(),
                    true,
                    dokploy_state::ManagedInputs::try_from_json(kind.bare_inputs()).unwrap(),
                    Some("environment.production".parse().unwrap()),
                    Vec::new(),
                ),
            )],
        );
        let unmanaged = discover(
            &fake,
            &kind.quiet_config("", "        lifecycle:\n          protect: true\n"),
            &plain_state,
        )
        .await;
        assert!(unmanaged.plan.applyable(), "{}", kind.name);
        assert_eq!(fake.count("GET /api/server.all"), 0, "{}", kind.name);
    }
}

#[tokio::test]
async fn saved_plans_bind_the_resolved_server_and_refuse_a_changed_resolution() {
    let key = [9_u8; 32];
    for kind in KINDS {
        let fake = Fake::start();
        let state = state_with(&fake, Vec::new());
        let yaml = kind.config(&named("edge-1"), "");
        let (compiled, _workspace) = compile(&fake, &yaml);
        let authority = DiscoveryAuthority::reconciliation;

        let first = discover_with(&fake, &compiled, &state, authority()).await;
        assert!(first.plan.applyable(), "{}", kind.name);
        let same = discover_with(&fake, &compiled, &state, authority()).await;
        fake.world().recreate_server("edge-1", "server-9");
        let recreated = discover_with(&fake, &compiled, &state, authority()).await;

        assert!(recreated.plan.applyable(), "{}", kind.name);
        assert_eq!(
            first.plan.to_json_bytes(),
            recreated.plan.to_json_bytes(),
            "{}: the plan JSON cannot show the identity change",
            kind.name
        );
        let saved = SavedPlan::from_fresh_plan(
            first.instance.clone(),
            &first.plan,
            first.remote.binding_receipt(&key),
        )
        .expect("applyable plans can be saved");
        saved
            .verify_fresh(
                &same.instance,
                &same.plan,
                same.remote.binding_receipt(&key),
            )
            .expect("an unchanged resolution still verifies");
        let refused = saved
            .verify_fresh(
                &recreated.instance,
                &recreated.plan,
                recreated.remote.binding_receipt(&key),
            )
            .expect_err("a re-created server record must invalidate the saved plan");
        assert!(
            matches!(refused, SavedPlanError::StaleEvidence),
            "{}",
            kind.name
        );

        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("saved.plan.json");
        dokploy_cli::saved_plan::write_new(&path, &saved).unwrap();
        let envelope = std::fs::read_to_string(&path).unwrap();
        assert_no_leaks(&envelope);
        assert_no_leaks(&format!("{saved:?}"));
    }
}

#[tokio::test]
async fn removed_renamed_or_duplicated_servers_make_a_saved_plan_unusable() {
    let key = [3_u8; 32];
    for kind in KINDS {
        let fake = Fake::start();
        let state = state_with(&fake, Vec::new());
        let (compiled, _workspace) = compile(&fake, &kind.config(&named("edge-1"), ""));
        let baseline = discover_with(
            &fake,
            &compiled,
            &state,
            DiscoveryAuthority::reconciliation(),
        )
        .await;
        let saved = SavedPlan::from_fresh_plan(
            baseline.instance.clone(),
            &baseline.plan,
            baseline.remote.binding_receipt(&key),
        )
        .expect("saved");

        for change in ["removed", "renamed", "duplicated"] {
            let fake_world = &fake;
            match change {
                "removed" => fake_world.world().servers.retain(|s| s.name != "edge-1"),
                "renamed" => fake_world.world().servers[0].name = "renamed".to_owned(),
                _ => {
                    let mut world = fake_world.world();
                    world.servers.clear();
                    world.add_server("server-1", "edge-1");
                    world.add_server("server-9", "edge-1");
                }
            }
            let fresh = discover_with(
                &fake,
                &compiled,
                &state,
                DiscoveryAuthority::reconciliation(),
            )
            .await;

            assert!(!fresh.plan.applyable(), "{} {change}", kind.name);
            assert!(matches!(
                SavedPlan::from_fresh_plan(
                    fresh.instance.clone(),
                    &fresh.plan,
                    fresh.remote.binding_receipt(&key)
                ),
                Err(SavedPlanError::PlanBlocked)
            ));
            assert!(matches!(
                saved.verify_fresh(
                    &fresh.instance,
                    &fresh.plan,
                    fresh.remote.binding_receipt(&key)
                ),
                Err(SavedPlanError::StaleEvidence)
            ));
        }
    }
}
