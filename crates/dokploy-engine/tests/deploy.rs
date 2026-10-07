//! Deploy planning and execution against the same simulator used by apply.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::project::ProjectWorld;
use common::{compile, engine};
use dokploy_engine::{DeployError, DeployOptions, DeployReason};
use dokploy_sdk::{Error as SdkError, OperationRequest, Transport};
use dokploy_sim::{Fault, FaultKind};
use serde_json::Value;
use url::Url;

const PASSWORD: &str = "database-password-canary";

fn document(description: &str, replicas: u8) -> String {
    format!(
        "version: 2
project:
  slug: shop
  environments:
    production:
      postgres:
        db:
          image: postgres:17
          database: shop
          username: shop
          password: {{ env: DB_PASSWORD }}
          lifecycle: {{ deploy: on_change }}
      applications:
        web:
          description: {description}
          replicas: {replicas}
          source: {{ type: github, owner: acme, repository: shop, branch: main }}
          depends_on: [postgres.db]
          lifecycle: {{ deploy: on_change }}
"
    )
}

async fn apply(world: &ProjectWorld, text: &str) {
    world
        .apply_text(text, &[("DB_PASSWORD", PASSWORD)])
        .await
        .expect("configuration applies");
}

#[tokio::test]
async fn deploys_are_dependency_ordered_waited_and_idempotent() {
    let world = ProjectWorld::bare();
    let text = document("Storefront", 1);
    apply(&world, &text).await;
    world.sim.clear_requests();

    let engine = world.engine();
    let store = world.store(&engine);
    let compiled = compile(&engine, &text, &[("DB_PASSWORD", PASSWORD)]);
    let plan = engine
        .deployment_plan(&compiled, store.inspect().unwrap().as_ref(), &[])
        .await
        .expect("deployment plans");
    assert_eq!(plan.deployments().len(), 2);
    assert_eq!(plan.deployments()[0].address().kind().as_str(), "postgres");
    assert_eq!(
        plan.deployments()[1].address().kind().as_str(),
        "application"
    );
    assert_eq!(plan.deployments()[0].reason(), DeployReason::Initial);
    assert_eq!(
        plan.deployments()[1].dependencies(),
        &[plan.deployments()[0].address().clone()]
    );

    let summary = engine
        .deploy(
            &compiled,
            &store,
            &DeployOptions {
                wait: true,
                ..Default::default()
            },
            |_| true,
        )
        .await
        .expect("deploys");
    assert_eq!(summary.deployed(), 2);
    let operations: Vec<_> = world
        .sim
        .mutations()
        .into_iter()
        .map(|request| request.operation().to_owned())
        .filter(|operation| operation.ends_with(".deploy"))
        .collect();
    assert_eq!(operations, ["postgres.deploy", "application.deploy"]);
    let state = store.inspect().unwrap().unwrap();
    for deployment in plan.deployments() {
        assert!(state.deployment(deployment.address()).is_some());
    }

    world.sim.clear_requests();
    let again = engine
        .deploy(&compiled, &store, &DeployOptions::default(), |_| true)
        .await
        .expect("a repeated deployment is empty");
    assert_eq!(again.deployed(), 0);
    assert!(world.sim.mutations().is_empty());
}

#[tokio::test]
async fn only_deployment_sensitive_changes_redeploy_unless_targeted() {
    let world = ProjectWorld::bare();
    let before = document("Storefront", 1);
    apply(&world, &before).await;
    let engine = world.engine();
    let store = world.store(&engine);
    let compiled = compile(&engine, &before, &[("DB_PASSWORD", PASSWORD)]);
    engine
        .deploy(&compiled, &store, &DeployOptions::default(), |_| true)
        .await
        .unwrap();

    let description = document("Public storefront", 1);
    apply(&world, &description).await;
    let compiled = compile(&engine, &description, &[("DB_PASSWORD", PASSWORD)]);
    let plan = engine
        .deployment_plan(&compiled, store.inspect().unwrap().as_ref(), &[])
        .await
        .unwrap();
    assert!(
        plan.deployments().is_empty(),
        "description does not rebuild"
    );

    let replicas = document("Public storefront", 2);
    apply(&world, &replicas).await;
    let compiled = compile(&engine, &replicas, &[("DB_PASSWORD", PASSWORD)]);
    let plan = engine
        .deployment_plan(&compiled, store.inspect().unwrap().as_ref(), &[])
        .await
        .unwrap();
    assert_eq!(plan.deployments().len(), 1);
    assert_eq!(plan.deployments()[0].reason(), DeployReason::Changed);
    assert_eq!(
        plan.deployments()[0].address().kind().as_str(),
        "application"
    );

    engine
        .deploy(&compiled, &store, &DeployOptions::default(), |_| true)
        .await
        .unwrap();
    let forced = engine
        .deployment_plan(
            &compiled,
            store.inspect().unwrap().as_ref(),
            &["application.web".to_owned()],
        )
        .await
        .unwrap();
    assert_eq!(forced.deployments().len(), 1);
    assert_eq!(forced.deployments()[0].reason(), DeployReason::Targeted);
    let forced_address = forced.deployments()[0].address().to_string();

    let summary = engine
        .deploy(
            &compiled,
            &store,
            &DeployOptions {
                targets: vec![forced_address],
                ..Default::default()
            },
            |_| true,
        )
        .await
        .expect("an explicit target redeploys even when its digest is unchanged");
    assert_eq!(summary.deployed(), 1);
}

#[tokio::test]
async fn a_lost_queue_response_is_recovered_from_the_unique_deployment_title() {
    let world = ProjectWorld::bare();
    let text = document("Storefront", 1);
    apply(&world, &text).await;
    let engine = world.engine();
    let store = world.store(&engine);
    let compiled = compile(&engine, &text, &[("DB_PASSWORD", PASSWORD)]);
    world
        .sim
        .inject(Fault::new("application.deploy", FaultKind::DropAfter));

    let error = engine
        .deploy(&compiled, &store, &DeployOptions::default(), |_| true)
        .await
        .unwrap_err();
    assert!(
        matches!(error, DeployError::OutcomeUnknown { .. }),
        "{error}"
    );

    let summary = engine
        .recover(&compiled, &store, |_| true)
        .await
        .expect("the titled deployment proves the queue action happened");
    assert_eq!(summary.recovered_steps(), 1);
    let plan = engine
        .deployment_plan(&compiled, store.inspect().unwrap().as_ref(), &[])
        .await
        .unwrap();
    assert!(plan.deployments().is_empty());
}

#[tokio::test]
async fn a_failed_dependency_skips_its_dependent_and_closes_the_journal() {
    let world = ProjectWorld::bare();
    let text = document("Storefront", 1);
    apply(&world, &text).await;
    let engine = world.engine();
    let store = world.store(&engine);
    let compiled = compile(&engine, &text, &[("DB_PASSWORD", PASSWORD)]);
    world.sim.clear_requests();
    world.sim.inject(Fault::new(
        "postgres.deploy",
        FaultKind::Reject { status: 500 },
    ));

    let summary = engine
        .deploy(&compiled, &store, &DeployOptions::default(), |_| true)
        .await
        .expect("known failures are reported in the summary");
    assert_eq!(summary.failed(), 1);
    assert_eq!(summary.skipped(), 1);
    assert_eq!(summary.deployed(), 0);
    let operations: Vec<_> = world
        .sim
        .mutations()
        .into_iter()
        .map(|request| request.operation().to_owned())
        .filter(|operation| operation.ends_with(".deploy"))
        .collect();
    assert_eq!(operations, ["postgres.deploy"]);
    assert!(matches!(
        store.recovery_status().unwrap(),
        dokploy_state::RecoveryStatus::Clean
    ));
}

#[tokio::test]
async fn a_wait_timeout_is_reported_without_recording_success() {
    let world = ProjectWorld::bare();
    let text = document("Storefront", 1);
    apply(&world, &text).await;
    let engine = world.engine();
    let store = world.store(&engine);
    let compiled = compile(&engine, &text, &[("DB_PASSWORD", PASSWORD)]);
    world.sim.tamper("deployment.allByType", |response| {
        for deployment in response.as_array_mut().into_iter().flatten() {
            deployment["status"] = serde_json::json!("running");
        }
    });

    let summary = engine
        .deploy(
            &compiled,
            &store,
            &DeployOptions {
                wait: true,
                timeout: std::time::Duration::from_millis(1),
                ..Default::default()
            },
            |_| true,
        )
        .await
        .expect("a timeout is a deployment result");
    assert_eq!(summary.deployed(), 1, "the synchronous database finished");
    assert_eq!(summary.timed_out(), 1);
    let state = store.inspect().unwrap().unwrap();
    let application = compiled
        .addresses()
        .find(|address| address.kind().as_str() == "application")
        .unwrap();
    assert!(state.deployment(application).is_none());
    assert!(matches!(
        store.recovery_status().unwrap(),
        dokploy_state::RecoveryStatus::Clean
    ));
}

struct DelayedDeploy<'a> {
    sim: &'a dokploy_sim::Sim,
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
}

impl Transport for DelayedDeploy<'_> {
    fn base_url(&self) -> &Url {
        self.sim.base_url()
    }

    async fn call(&self, request: OperationRequest) -> Result<Value, SdkError> {
        if request.operation().ends_with(".deploy") {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            let result = self.sim.call(request).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        } else {
            self.sim.call(request).await
        }
    }
}

#[tokio::test]
async fn independent_deployments_run_up_to_the_parallelism_limit() {
    let world = ProjectWorld::bare();
    let text = "version: 2
project:
  slug: shop
  environments:
    production:
      applications:
        api:
          source: { type: github, owner: acme, repository: api, branch: main }
          lifecycle: { deploy: on_create }
        web:
          source: { type: github, owner: acme, repository: web, branch: main }
          lifecycle: { deploy: on_create }
";
    apply(&world, text).await;
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let transport = DelayedDeploy {
        sim: &world.sim,
        active,
        maximum: Arc::clone(&maximum),
    };
    let engine = engine(transport);
    let store = world.store(&world.engine());
    let compiled = compile(&engine, text, &[("DB_PASSWORD", PASSWORD)]);

    let summary = engine
        .deploy(
            &compiled,
            &store,
            &DeployOptions {
                parallelism: 2,
                ..Default::default()
            },
            |_| true,
        )
        .await
        .unwrap();
    assert_eq!(summary.deployed(), 2);
    assert_eq!(maximum.load(Ordering::SeqCst), 2);
}
