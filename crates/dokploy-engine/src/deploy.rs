//! Deployment planning and execution (ADR 0012).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use dokploy_model::DeployPolicy;
use dokploy_sdk::{Error as SdkError, OperationRequest, Transport};
use dokploy_spec::DeployWait;
use dokploy_state::{
    DeploymentDigest, ExpectedCheckpoint, FailureCode, JournalAction, JournalError,
    OperationJournal, PlanDigest, ResourceAddress, StateError, StateFile, StateStore,
    StateStoreError,
};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::compile::Compiled;
use crate::{Engine, EngineError};

/// Why a resource appears in a deployment plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployReason {
    /// No successful deployment has been recorded for this resource.
    Initial,
    /// A deployment-sensitive input changed since the last successful deployment.
    Changed,
    /// The caller explicitly selected the resource.
    Targeted,
}

/// One dependency-ordered deployment action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedDeployment {
    address: ResourceAddress,
    reason: DeployReason,
    dependencies: Vec<ResourceAddress>,
    #[serde(skip)]
    digest: DeploymentDigest,
}

impl PlannedDeployment {
    /// The resource to deploy.
    #[must_use]
    pub const fn address(&self) -> &ResourceAddress {
        &self.address
    }

    /// Why the resource is selected.
    #[must_use]
    pub const fn reason(&self) -> DeployReason {
        self.reason
    }

    /// Selected deployments that must complete first.
    #[must_use]
    pub fn dependencies(&self) -> &[ResourceAddress] {
        &self.dependencies
    }
}

/// A redaction-safe deployment plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentPlan {
    format_version: u32,
    deployments: Vec<PlannedDeployment>,
}

impl DeploymentPlan {
    /// Planned deployments in stable dependency order.
    #[must_use]
    pub fn deployments(&self) -> &[PlannedDeployment] {
        &self.deployments
    }

    /// Stable redaction-safe JSON used by journals and `--json` output.
    #[must_use]
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a deployment plan always serializes")
    }
}

/// Runtime controls for a deployment operation.
#[derive(Clone, Debug)]
pub struct DeployOptions {
    /// Address suffixes that force deployment regardless of lifecycle policy.
    pub targets: Vec<String>,
    /// Wait for queued application and Compose deployments to finish.
    pub wait: bool,
    /// Maximum wait for each asynchronous deployment.
    pub timeout: Duration,
    /// Maximum independent deployments allowed to run at once.
    pub parallelism: usize,
}

impl Default for DeployOptions {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            wait: false,
            timeout: Duration::from_secs(20 * 60),
            parallelism: 4,
        }
    }
}

/// What a deployment run completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeploySummary {
    deployed: usize,
    failed: usize,
    skipped: usize,
    timed_out: usize,
}

impl DeploySummary {
    /// Number of successful deployment actions checkpointed in state.
    #[must_use]
    pub const fn deployed(self) -> usize {
        self.deployed
    }

    /// Number of deployments Dokploy rejected or completed with an error.
    #[must_use]
    pub const fn failed(self) -> usize {
        self.failed
    }

    /// Number of deployments skipped because one of their dependencies failed.
    #[must_use]
    pub const fn skipped(self) -> usize {
        self.skipped
    }

    /// Number of queued deployments still running at the timeout.
    #[must_use]
    pub const fn timed_out(self) -> usize {
        self.timed_out
    }

    /// Whether every selected deployment finished successfully.
    #[must_use]
    pub const fn successful(self) -> bool {
        self.failed == 0 && self.skipped == 0 && self.timed_out == 0
    }
}

/// Why deployment planning or execution stopped.
#[derive(Debug, Error)]
pub enum DeployError {
    /// Fresh discovery or ordinary planning failed.
    #[error(transparent)]
    Plan(#[from] EngineError),
    /// Deployment requires an initialized, converged configuration state.
    #[error("configuration is not converged; run `dokploy apply` before deployment")]
    ConfigurationNotConverged,
    /// No initialized state exists for this document.
    #[error("this document has not been applied; run `dokploy apply` before deployment")]
    StateMissing,
    /// A target suffix did not resolve uniquely.
    #[error("deployment target `{target}` does not name exactly one declared resource")]
    Target { target: String },
    /// A selected kind has no deploy operation.
    #[error("`{address}` cannot be deployed")]
    Unsupported { address: ResourceAddress },
    /// The caller declined the plan.
    #[error("the deployment plan was declined; nothing was deployed")]
    Declined,
    /// Parallelism must permit at least one worker.
    #[error("deployment parallelism must be greater than zero")]
    InvalidParallelism,
    /// The mutation may have happened and must be recovered.
    #[error("{address}: the deployment outcome is unknown; recover before deploying again")]
    OutcomeUnknown { address: ResourceAddress },
    /// The durable operation journal failed.
    #[error(transparent)]
    Journal(#[from] JournalError),
    /// The state store failed.
    #[error(transparent)]
    Store(#[from] StateStoreError),
    /// A state transition was refused.
    #[error(transparent)]
    State(#[from] StateError),
}

impl<T: Transport> Engine<T> {
    /// Produces a redaction-safe deployment plan from converged configuration.
    pub async fn deployment_plan(
        &self,
        compiled: &Compiled,
        state: Option<&StateFile>,
        targets: &[String],
    ) -> Result<DeploymentPlan, DeployError> {
        let state = state.ok_or(DeployError::StateMissing)?;
        let configuration = self.plan(compiled, Some(state)).await?;
        if !configuration.complete()
            || !configuration.applyable()
            || !configuration.changes().is_empty()
        {
            return Err(DeployError::ConfigurationNotConverged);
        }

        let forced = resolve_targets(compiled, targets)?;
        let mut selected = BTreeMap::new();
        for (address, metadata) in &compiled.resources {
            let Some(desired) = compiled.desired.resources().get(address) else {
                continue;
            };
            let digest = desired.deployment_digest();
            let targeted = forced.contains(address);
            let reason = if targeted {
                Some(DeployReason::Targeted)
            } else {
                match metadata.deploy_policy {
                    DeployPolicy::Never => None,
                    DeployPolicy::OnCreate if state.deployment(address).is_none() => {
                        Some(DeployReason::Initial)
                    }
                    DeployPolicy::OnCreate => None,
                    DeployPolicy::OnChange if state.deployment(address).is_none() => {
                        Some(DeployReason::Initial)
                    }
                    DeployPolicy::OnChange if state.deployment(address) != Some(&digest) => {
                        Some(DeployReason::Changed)
                    }
                    DeployPolicy::OnChange => None,
                }
            };
            let Some(reason) = reason else {
                continue;
            };
            let deployable = self
                .specs
                .get(&metadata.spec_kind)
                .is_some_and(|spec| spec.api.deploy.is_some());
            if !deployable {
                return Err(DeployError::Unsupported {
                    address: address.clone(),
                });
            }
            selected.insert(address.clone(), (reason, digest));
        }

        let mut pending: BTreeSet<ResourceAddress> = selected.keys().cloned().collect();
        let mut ordered = Vec::with_capacity(pending.len());
        while !pending.is_empty() {
            let ready = pending
                .iter()
                .find(|address| {
                    compiled
                        .desired
                        .resources()
                        .get(*address)
                        .is_some_and(|resource| {
                            resource
                                .dependencies()
                                .iter()
                                .all(|dependency| !pending.contains(dependency))
                        })
                })
                .cloned()
                .expect("compiled desired dependencies are acyclic");
            pending.remove(&ready);
            let resource = compiled
                .desired
                .resources()
                .get(&ready)
                .expect("compiled metadata and desired state agree");
            let dependencies = resource
                .dependencies()
                .iter()
                .filter(|dependency| selected.contains_key(*dependency))
                .cloned()
                .collect();
            let (reason, digest) = selected
                .get(&ready)
                .expect("a pending deployment is selected")
                .clone();
            ordered.push(PlannedDeployment {
                address: ready,
                reason,
                dependencies,
                digest,
            });
        }

        Ok(DeploymentPlan {
            format_version: 1,
            deployments: ordered,
        })
    }

    /// Plans and executes deployments while holding the document writer lock.
    pub async fn deploy(
        &self,
        compiled: &Compiled,
        store: &StateStore,
        options: &DeployOptions,
        approve: impl FnOnce(&DeploymentPlan) -> bool,
    ) -> Result<DeploySummary, DeployError> {
        if options.parallelism == 0 {
            return Err(DeployError::InvalidParallelism);
        }
        let mut session = store.begin_write()?;
        let durable = store.inspect()?.ok_or(DeployError::StateMissing)?;
        let plan = self
            .deployment_plan(compiled, Some(&durable), &options.targets)
            .await?;
        if !approve(&plan) {
            return Err(DeployError::Declined);
        }
        if plan.deployments.is_empty() {
            return Ok(DeploySummary {
                deployed: 0,
                failed: 0,
                skipped: 0,
                timed_out: 0,
            });
        }

        let digest = PlanDigest::parse(hex(Sha256::digest(plan.to_json_bytes())))
            .expect("a SHA-256 digest is valid");
        let mut journal = OperationJournal::begin(&mut session, digest)?;
        let operation_id = journal.operation_id();
        let mut state = durable;
        let mut tokens = Vec::with_capacity(plan.deployments.len());
        let mut titles = Vec::with_capacity(plan.deployments.len());
        for deployment in &plan.deployments {
            let resource = state
                .resource(&deployment.address)
                .cloned()
                .ok_or(DeployError::StateMissing)?;
            let token = journal.start_recoverable_step(
                deployment.address.clone(),
                JournalAction::Deploy,
                ExpectedCheckpoint::deploy(resource, deployment.digest.clone()),
            )?;
            titles.push(deployment_title(operation_id, token.sequence()));
            tokens.push(Some(token));
        }
        let mut deployed = 0_usize;
        let mut failed = 0_usize;
        let mut skipped = 0_usize;
        let mut timed_out = 0_usize;
        let mut outcomes = BTreeMap::<ResourceAddress, bool>::new();
        let mut pending: BTreeSet<usize> = (0..plan.deployments.len()).collect();
        while !pending.is_empty() {
            let blocked: Vec<usize> = pending
                .iter()
                .copied()
                .filter(|index| {
                    plan.deployments[*index]
                        .dependencies
                        .iter()
                        .any(|dependency| outcomes.get(dependency) == Some(&false))
                })
                .collect();
            for index in blocked {
                pending.remove(&index);
                let deployment = &plan.deployments[index];
                let token = tokens[index]
                    .take()
                    .expect("a blocked deployment has one journal token");
                journal.fail(token, FailureCode::DependencyFailed)?;
                outcomes.insert(deployment.address.clone(), false);
                skipped += 1;
            }

            let batch: Vec<usize> = pending
                .iter()
                .copied()
                .filter(|index| {
                    plan.deployments[*index]
                        .dependencies
                        .iter()
                        .all(|dependency| outcomes.get(dependency) == Some(&true))
                })
                .take(options.parallelism)
                .collect();
            if batch.is_empty() {
                if pending.is_empty() {
                    break;
                }
                unreachable!("the compiled dependency graph is acyclic");
            }
            for index in &batch {
                pending.remove(index);
            }
            let work = futures_util::stream::iter(batch.iter().copied().map(|index| {
                let deployment = &plan.deployments[index];
                let resource = state
                    .resource(&deployment.address)
                    .cloned()
                    .expect("planned deployment state exists");
                let title = &titles[index];
                async move {
                    (
                        index,
                        self.perform_deployment(compiled, deployment, &resource, title, options)
                            .await,
                    )
                }
            }))
            .buffer_unordered(options.parallelism);
            let mut results = work.collect::<Vec<_>>().await;
            results.sort_by_key(|(index, _)| *index);
            let mut unknown = Vec::new();
            for (index, result) in results {
                let deployment = &plan.deployments[index];
                let address = &deployment.address;
                match result {
                    RemoteDeployOutcome::Succeeded => {
                        let token = tokens[index]
                            .take()
                            .expect("a successful deployment has one journal token");
                        state.mark_deployed(address.clone(), deployment.digest.clone())?;
                        journal.succeed(token, None, &state)?;
                        outcomes.insert(address.clone(), true);
                        deployed += 1;
                    }
                    RemoteDeployOutcome::Rejected(code) => {
                        let token = tokens[index]
                            .take()
                            .expect("a failed deployment has one journal token");
                        journal.fail(token, code)?;
                        outcomes.insert(address.clone(), false);
                        failed += 1;
                    }
                    RemoteDeployOutcome::TimedOut => {
                        let token = tokens[index]
                            .take()
                            .expect("a timed out deployment has one journal token");
                        journal.fail(token, FailureCode::Timeout)?;
                        outcomes.insert(address.clone(), false);
                        timed_out += 1;
                    }
                    RemoteDeployOutcome::Unknown => unknown.push(index),
                }
            }
            if let Some(first) = unknown.first().copied() {
                for index in pending.iter().copied().collect::<Vec<_>>() {
                    if let Some(token) = tokens[index].take() {
                        journal.fail(token, FailureCode::Internal)?;
                    }
                }
                return Err(DeployError::OutcomeUnknown {
                    address: plan.deployments[first].address.clone(),
                });
            }
        }
        if failed == 0 && skipped == 0 && timed_out == 0 {
            journal.commit()?;
        } else {
            journal.complete_with_failures()?;
        }

        Ok(DeploySummary {
            deployed,
            failed,
            skipped,
            timed_out,
        })
    }

    async fn perform_deployment(
        &self,
        compiled: &Compiled,
        deployment: &PlannedDeployment,
        resource: &dokploy_state::ResourceState,
        title: &str,
        options: &DeployOptions,
    ) -> RemoteDeployOutcome {
        let metadata = compiled
            .resources
            .get(&deployment.address)
            .expect("a planned deployment has compiled metadata");
        let spec = self
            .specs
            .get(&metadata.spec_kind)
            .expect("compiled resource kinds have specs");
        let deploy = spec
            .api
            .deploy
            .as_ref()
            .expect("planned deployments are deployable");
        let mut body = serde_json::Map::new();
        body.insert(
            spec.api.id.clone(),
            Json::String(resource.remote_id().as_str().to_owned()),
        );
        if deploy.wait == Some(DeployWait::Deployment) {
            body.insert("title".to_owned(), Json::String(title.to_owned()));
            body.insert(
                "description".to_owned(),
                Json::String(deployment.address.to_string()),
            );
        }
        if let Err(error) = self
            .transport
            .call(OperationRequest::new(&deploy.op).body(Json::Object(body)))
            .await
        {
            let code = failure_code(&error);
            return if code == FailureCode::TransportOutcomeUnknown {
                RemoteDeployOutcome::Unknown
            } else {
                RemoteDeployOutcome::Rejected(code)
            };
        }
        if !options.wait || deploy.wait != Some(DeployWait::Deployment) {
            return RemoteDeployOutcome::Succeeded;
        }
        match self
            .wait_for_deployment(
                resource.remote_id().as_str(),
                &metadata.spec_kind,
                title,
                options.timeout,
            )
            .await
        {
            Ok(WaitOutcome::Done) => RemoteDeployOutcome::Succeeded,
            Ok(WaitOutcome::Failed) => RemoteDeployOutcome::Rejected(FailureCode::RemoteRejected),
            Ok(WaitOutcome::Timeout) => RemoteDeployOutcome::TimedOut,
            Err(_) => RemoteDeployOutcome::Unknown,
        }
    }

    async fn wait_for_deployment(
        &self,
        remote_id: &str,
        kind: &str,
        title: &str,
        timeout: Duration,
    ) -> Result<WaitOutcome, FailureCode> {
        let deployment_type = match kind {
            "application" => "application",
            "compose" => "compose",
            _ => return Ok(WaitOutcome::Done),
        };
        let started = Instant::now();
        loop {
            let response = self
                .transport
                .call(
                    OperationRequest::new("deployment.allByType")
                        .query("id", remote_id)
                        .query("type", deployment_type),
                )
                .await
                .map_err(|error| failure_code(&error))?;
            if let Some(status) = response
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| row.get("title").and_then(Json::as_str) == Some(title))
                .and_then(|row| row.get("status"))
                .and_then(Json::as_str)
            {
                match status {
                    "done" => return Ok(WaitOutcome::Done),
                    "error" | "cancelled" => {
                        return Ok(WaitOutcome::Failed);
                    }
                    _ => {}
                }
            }
            if started.elapsed() >= timeout {
                return Ok(WaitOutcome::Timeout);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            tokio::time::sleep(remaining.min(Duration::from_millis(250))).await;
        }
    }
}

enum WaitOutcome {
    Done,
    Failed,
    Timeout,
}

enum RemoteDeployOutcome {
    Succeeded,
    Rejected(FailureCode),
    TimedOut,
    Unknown,
}

fn resolve_targets(
    compiled: &Compiled,
    targets: &[String],
) -> Result<BTreeSet<ResourceAddress>, DeployError> {
    let addresses: Vec<&ResourceAddress> = compiled.resources.keys().collect();
    targets
        .iter()
        .map(|target| {
            ResourceAddress::resolve_suffix(addresses.iter().copied(), target)
                .cloned()
                .map_err(|_| DeployError::Target {
                    target: target.clone(),
                })
        })
        .collect()
}

pub(crate) fn deployment_title(operation_id: uuid::Uuid, sequence: u64) -> String {
    format!("dokploy-iac:{operation_id}:{sequence}")
}

fn failure_code(error: &SdkError) -> FailureCode {
    match error {
        SdkError::Api(detail) if matches!(detail.status(), 401 | 403) => FailureCode::Unauthorized,
        SdkError::Api(detail) if matches!(detail.status(), 400 | 422) => FailureCode::Validation,
        SdkError::Api(_) => FailureCode::RemoteRejected,
        SdkError::OutcomeUnknown { .. }
        | SdkError::Decode { .. }
        | SdkError::UnexpectedResponse { .. } => FailureCode::TransportOutcomeUnknown,
        SdkError::InvalidRequest { .. } => FailureCode::Validation,
        SdkError::Request { .. } => FailureCode::Internal,
    }
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        let _ = write!(text, "{byte:02x}");
    }
    text
}
