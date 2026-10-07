//! The declarative commands that talk to Dokploy, over any transport.
//!
//! They are generic over the engine's transport so the same code runs against the real client
//! and against the simulator in tests.

use std::io::{BufRead, Write};
use std::sync::Arc;

use dokploy_core::Plan;
use dokploy_engine::{
    ApplyError, Compiled, DeployError, DeployOptions, DeploymentPlan, Engine, FingerprintKey,
    ImportInventoryPass, ImportOptions, ImportProgress, ImportSelection, RecoverError,
    UnsupportedCategory, WorkspaceSecrets,
};
use dokploy_sdk::Transport;
use dokploy_state::StateStore;
use miette::{IntoDiagnostic, Result};

use crate::workspace::{Workspace, document_id};
use crate::{CommandStatus, plan_output};

/// Reads an environment variable. Injected so tests do not touch the process environment.
pub type Environment = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Where a command writes: results (machine-readable) and diagnostics (plans, prompts).
pub struct Streams<'a> {
    result: &'a mut dyn Write,
    diagnostics: Option<&'a mut dyn Write>,
}

impl<'a> Streams<'a> {
    pub fn unified(output: &'a mut dyn Write) -> Self {
        Self {
            result: output,
            diagnostics: None,
        }
    }

    pub fn split(result: &'a mut dyn Write, diagnostics: &'a mut dyn Write) -> Self {
        Self {
            result,
            diagnostics: Some(diagnostics),
        }
    }

    pub fn result(&mut self) -> &mut dyn Write {
        self.result
    }

    pub fn diagnostics(&mut self) -> &mut dyn Write {
        match self.diagnostics.as_deref_mut() {
            Some(output) => output,
            None => &mut *self.result,
        }
    }
}

/// How a command is attached to the terminal.
pub struct Terminal<'a> {
    pub input: &'a mut dyn BufRead,
    pub available: bool,
}

/// Approval controls shared by a manifest apply.
pub struct ApplyAllOptions {
    pub auto_approve: bool,
    pub allow_disruptive: bool,
}

/// Execution and approval controls shared by a manifest deploy.
pub struct DeployAllOptions<'a> {
    pub deploy: &'a DeployOptions,
    pub auto_approve: bool,
}

/// Everything a command needs: the engine, the document, and how to read its secrets.
pub struct Context<'a, T> {
    pub engine: &'a Engine<T>,
    pub workspace: &'a Workspace,
    pub key: FingerprintKey,
    pub environment: &'a Environment,
}

struct Prepared {
    compiled: Compiled,
    store: StateStore,
}

fn prepare<T: Transport>(context: Context<'_, T>, secrets_needed: bool) -> Result<Prepared> {
    let Context {
        engine,
        workspace,
        key,
        environment,
    } = context;
    let document = workspace.parse(engine.specs())?;
    let compiled = if secrets_needed {
        let environment = Arc::clone(environment);
        let secrets = WorkspaceSecrets::new(workspace.directory.clone(), move |name: &str| {
            environment(name)
        });
        engine
            .compile(&document, &engine.fingerprinter(key), &secrets)
            .into_diagnostic()?
    } else {
        engine.compile_empty(&document).into_diagnostic()?
    };
    let store = StateStore::for_document(
        &workspace.directory,
        engine.instance().clone(),
        document_id(&document)?,
    )
    .into_diagnostic()?;

    Ok(Prepared { compiled, store })
}

/// `plan`: compare the document, the state, and fresh Dokploy state; change nothing.
pub async fn plan<T: Transport>(
    context: Context<'_, T>,
    json: bool,
    detailed_exitcode: bool,
    streams: &mut Streams<'_>,
) -> Result<CommandStatus> {
    let engine = context.engine;
    let prepared = prepare(context, true)?;
    let state = prepared.store.inspect().into_diagnostic()?;
    let plan = engine
        .plan(&prepared.compiled, state.as_ref())
        .await
        .into_diagnostic()?;

    if json {
        streams
            .result()
            .write_all(&plan.to_json_bytes())
            .into_diagnostic()?;
        writeln!(streams.result()).into_diagnostic()?;
    } else {
        plan_output::render(&plan, streams.result()).into_diagnostic()?;
    }

    Ok(if !plan.complete() || !plan.applyable() {
        CommandStatus::Failure
    } else if detailed_exitcode && !plan.changes().is_empty() {
        CommandStatus::ChangesPresent
    } else {
        CommandStatus::Success
    })
}

/// Plan every manifest document in order against its own state lineage.
pub async fn plan_all<T: Transport>(
    engine: &Engine<T>,
    manifest: &crate::workspace::WorkspaceManifest,
    key: FingerprintKey,
    environment: &Environment,
    json: bool,
    detailed_exitcode: bool,
    streams: &mut Streams<'_>,
) -> Result<CommandStatus> {
    let mut json_plans = Vec::new();
    let mut failure = false;
    let mut changes = false;
    for relative in manifest.documents() {
        let workspace = Workspace::load_in(manifest.directory(), relative)?;
        let prepared = prepare(
            Context {
                engine,
                workspace: &workspace,
                key: key.clone(),
                environment,
            },
            true,
        )?;
        let state = prepared.store.inspect().into_diagnostic()?;
        let plan = engine
            .plan(&prepared.compiled, state.as_ref())
            .await
            .into_diagnostic()?;
        failure |= !plan.complete() || !plan.applyable();
        changes |= !plan.changes().is_empty();
        if json {
            let value: serde_json::Value =
                serde_json::from_slice(&plan.to_json_bytes()).expect("a plan serializes as JSON");
            json_plans.push(serde_json::json!({
                "document": relative.to_string_lossy(),
                "plan": value,
            }));
        } else {
            writeln!(streams.result(), "{}:", relative.display()).into_diagnostic()?;
            plan_output::render(&plan, streams.result()).into_diagnostic()?;
        }
    }
    if json {
        serde_json::to_writer_pretty(&mut *streams.result(), &json_plans).into_diagnostic()?;
        writeln!(streams.result()).into_diagnostic()?;
    }

    Ok(if failure {
        CommandStatus::Failure
    } else if detailed_exitcode && changes {
        CommandStatus::ChangesPresent
    } else {
        CommandStatus::Success
    })
}

/// Asks the person at the terminal; closed input is never consent.
fn confirm(
    terminal: &mut Terminal<'_>,
    diagnostics: &mut dyn Write,
    operation: &str,
    question: &str,
) -> Result<bool> {
    if !terminal.available {
        return Err(miette::miette!(
            "{operation} approval requires a terminal; use --auto-approve"
        ));
    }
    write!(diagnostics, "{question} Type 'yes' to continue: ").into_diagnostic()?;
    diagnostics.flush().into_diagnostic()?;
    let mut answer = String::new();
    terminal.input.read_line(&mut answer).into_diagnostic()?;

    Ok(answer.trim() == "yes")
}

/// Shows the plan and decides whether to go ahead. Nothing to do, or nothing safe to do,
/// needs no approval: the apply itself refuses a blocked plan.
fn approval(
    plan: &Plan,
    auto_approve: bool,
    allow_disruptive: bool,
    terminal: &mut Terminal<'_>,
    diagnostics: &mut dyn Write,
    operation: &str,
    question: &str,
) -> Result<bool> {
    plan_output::render(plan, diagnostics).into_diagnostic()?;
    if plan.changes().is_empty() || !plan.complete() || !plan.applyable() {
        return Ok(true);
    }

    if auto_approve {
        if plan.has_disruptive_changes() && !allow_disruptive {
            return Err(miette::miette!(
                "apply includes disruptive changes; add --allow-disruptive to --auto-approve"
            ));
        }
        return Ok(true);
    }

    confirm(terminal, diagnostics, operation, question)
}

/// `apply` and `destroy`: execute the plan, one journaled step at a time.
pub async fn apply<T: Transport>(
    context: Context<'_, T>,
    destroy: bool,
    auto_approve: bool,
    allow_disruptive: bool,
    mut terminal: Terminal<'_>,
    streams: &mut Streams<'_>,
) -> Result<CommandStatus> {
    let engine = context.engine;
    let prepared = prepare(context, !destroy)?;
    let (operation, question, done) = if destroy {
        (
            "destroy",
            "Destroy all tracked resources?",
            "Destroy complete: {} resource(s) deleted.",
        )
    } else {
        (
            "apply",
            "Apply these changes?",
            "Apply complete: {} change(s).",
        )
    };
    let compiled = prepared.compiled;

    // The callback cannot return an error, so a failure to ask is kept and reported after.
    let mut asking_failed = None;
    let outcome = engine
        .apply(&compiled, &prepared.store, |plan| {
            match approval(
                plan,
                auto_approve,
                allow_disruptive,
                &mut terminal,
                streams.diagnostics(),
                operation,
                question,
            ) {
                Ok(approved) => approved,
                Err(error) => {
                    asking_failed = Some(error);
                    false
                }
            }
        })
        .await;
    if let Some(error) = asking_failed {
        return Err(error);
    }

    match outcome {
        Ok(summary) => {
            writeln!(
                streams.result(),
                "{}",
                done.replace("{}", &summary.applied().to_string())
            )
            .into_diagnostic()?;
            Ok(CommandStatus::Success)
        }
        Err(ApplyError::Declined) => {
            writeln!(
                streams.diagnostics(),
                "{} cancelled.",
                capitalized(operation)
            )
            .into_diagnostic()?;
            Ok(CommandStatus::Success)
        }
        Err(error) => Err(error).into_diagnostic(),
    }
}

/// Applies every manifest document in dependency order.
pub async fn apply_all<T: Transport>(
    engine: &Engine<T>,
    manifest: &crate::workspace::WorkspaceManifest,
    key: FingerprintKey,
    environment: &Environment,
    options: &ApplyAllOptions,
    terminal: &mut Terminal<'_>,
    streams: &mut Streams<'_>,
) -> Result<CommandStatus> {
    for relative in manifest.documents() {
        let workspace = Workspace::load_in(manifest.directory(), relative)?;
        writeln!(streams.diagnostics(), "{}:", relative.display()).into_diagnostic()?;
        apply(
            Context {
                engine,
                workspace: &workspace,
                key: key.clone(),
                environment,
            },
            false,
            options.auto_approve,
            options.allow_disruptive,
            Terminal {
                input: &mut *terminal.input,
                available: terminal.available,
            },
            streams,
        )
        .await?;
    }

    Ok(CommandStatus::Success)
}

fn render_deployment_plan(plan: &DeploymentPlan, output: &mut dyn Write) -> std::io::Result<()> {
    writeln!(output, "Deploy: {} resource(s)", plan.deployments().len())?;
    for deployment in plan.deployments() {
        writeln!(
            output,
            "  deploy {} ({:?})",
            deployment.address(),
            deployment.reason()
        )?;
    }

    Ok(())
}

/// `deploy`: deploy eligible resources after proving configuration convergence.
pub async fn deploy<T: Transport>(
    context: Context<'_, T>,
    options: &DeployOptions,
    auto_approve: bool,
    terminal: &mut Terminal<'_>,
    streams: &mut Streams<'_>,
) -> Result<CommandStatus> {
    let engine = context.engine;
    let prepared = prepare(context, true)?;
    deploy_prepared(engine, prepared, options, auto_approve, terminal, streams).await
}

async fn deploy_prepared<T: Transport>(
    engine: &Engine<T>,
    prepared: Prepared,
    options: &DeployOptions,
    auto_approve: bool,
    terminal: &mut Terminal<'_>,
    streams: &mut Streams<'_>,
) -> Result<CommandStatus> {
    let mut asking_failed = None;
    let outcome = engine
        .deploy(&prepared.compiled, &prepared.store, options, |plan| {
            let shown = render_deployment_plan(plan, streams.diagnostics()).into_diagnostic();
            let approved = shown.and_then(|()| {
                if plan.deployments().is_empty() || auto_approve {
                    Ok(true)
                } else {
                    confirm(
                        terminal,
                        streams.diagnostics(),
                        "deploy",
                        "Deploy these resources?",
                    )
                }
            });
            match approved {
                Ok(approved) => approved,
                Err(error) => {
                    asking_failed = Some(error);
                    false
                }
            }
        })
        .await;
    if let Some(error) = asking_failed {
        return Err(error);
    }

    match outcome {
        Ok(summary) => {
            writeln!(
                streams.result(),
                "Deploy complete: {} succeeded, {} failed, {} skipped, {} timed out.",
                summary.deployed(),
                summary.failed(),
                summary.skipped(),
                summary.timed_out()
            )
            .into_diagnostic()?;
            Ok(if summary.successful() {
                CommandStatus::Success
            } else {
                CommandStatus::Failure
            })
        }
        Err(DeployError::Declined) => {
            writeln!(streams.diagnostics(), "Deploy cancelled.").into_diagnostic()?;
            Ok(CommandStatus::Success)
        }
        Err(error) => Err(error).into_diagnostic(),
    }
}

/// Deploys every manifest document in order, resolving explicit targets globally.
pub async fn deploy_all<T: Transport>(
    engine: &Engine<T>,
    manifest: &crate::workspace::WorkspaceManifest,
    key: FingerprintKey,
    environment: &Environment,
    options: &DeployAllOptions<'_>,
    terminal: &mut Terminal<'_>,
    streams: &mut Streams<'_>,
) -> Result<CommandStatus> {
    let mut documents = Vec::new();
    for relative in manifest.documents() {
        let workspace = Workspace::load_in(manifest.directory(), relative)?;
        let prepared = prepare(
            Context {
                engine,
                workspace: &workspace,
                key: key.clone(),
                environment,
            },
            true,
        )?;
        documents.push((relative.clone(), prepared));
    }

    let mut document_targets: Vec<Vec<String>> = vec![Vec::new(); documents.len()];
    for target in &options.deploy.targets {
        let matches = documents
            .iter()
            .enumerate()
            .flat_map(|(index, (_, prepared))| {
                prepared
                    .compiled
                    .addresses()
                    .filter(move |address| {
                        let address = address.to_string();
                        address == *target || address.ends_with(&format!("/{target}"))
                    })
                    .map(move |address| (index, address.to_string()))
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(miette::miette!(
                "deployment target `{target}` does not name exactly one declared resource"
            ));
        }
        let (index, address) = matches.into_iter().next().expect("one match was checked");
        document_targets[index].push(address);
    }

    let mut failure = false;
    for (index, (relative, prepared)) in documents.into_iter().enumerate() {
        writeln!(streams.diagnostics(), "{}:", relative.display()).into_diagnostic()?;
        let mut document_options = options.deploy.clone();
        document_options.targets = document_targets[index].clone();
        let status = deploy_prepared(
            engine,
            prepared,
            &document_options,
            options.auto_approve,
            terminal,
            streams,
        )
        .await?;
        failure |= status == CommandStatus::Failure;
    }

    Ok(if failure {
        CommandStatus::Failure
    } else {
        CommandStatus::Success
    })
}

/// `recover`: settle an interrupted apply from fresh evidence.
pub async fn recover<T: Transport>(
    context: Context<'_, T>,
    auto_approve: bool,
    mut terminal: Terminal<'_>,
    streams: &mut Streams<'_>,
) -> Result<CommandStatus> {
    let engine = context.engine;
    let prepared = prepare(context, true)?;
    let mut asking_failed = None;
    let outcome = engine
        .recover(&prepared.compiled, &prepared.store, |preview| {
            let diagnostics = streams.diagnostics();
            let shown = match preview.address() {
                Some(address) => writeln!(
                    diagnostics,
                    "Recovery action for {address}: {:?}",
                    preview.action()
                ),
                None => writeln!(diagnostics, "Recovery action: {:?}", preview.action()),
            };
            let approved = shown.into_diagnostic().and_then(|()| {
                if auto_approve {
                    Ok(true)
                } else {
                    confirm(
                        &mut terminal,
                        streams.diagnostics(),
                        "recovery",
                        "Complete this recovery?",
                    )
                }
            });
            match approved {
                Ok(approved) => approved,
                Err(error) => {
                    asking_failed = Some(error);
                    false
                }
            }
        })
        .await;
    if let Some(error) = asking_failed {
        return Err(error);
    }

    match outcome {
        Ok(summary) => {
            writeln!(
                streams.result(),
                "Recovery complete: {} interrupted step(s) resolved.",
                summary.recovered_steps()
            )
            .into_diagnostic()?;
            Ok(CommandStatus::Success)
        }
        Err(RecoverError::Declined) => {
            writeln!(streams.diagnostics(), "Recovery cancelled.").into_diagnostic()?;
            Ok(CommandStatus::Success)
        }
        Err(error) => Err(error).into_diagnostic(),
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

fn write_import_progress(progress: &ImportProgress, output: &mut dyn Write) -> std::io::Result<()> {
    match progress {
        ImportProgress::InventoryStarted { pass, total_kinds } => match pass {
            ImportInventoryPass::Initial => {
                writeln!(
                    output,
                    "Import: reading Dokploy inventory ({total_kinds} kinds)"
                )?;
            }
            ImportInventoryPass::Verification => {
                writeln!(
                    output,
                    "Import: verifying remote topology ({total_kinds} kinds)"
                )?;
            }
        },
        ImportProgress::InventoryKind {
            pass,
            current,
            total,
            kind,
        } => {
            let action = match pass {
                ImportInventoryPass::Initial => "Reading",
                ImportInventoryPass::Verification => "Checking",
            };
            writeln!(output, "  [{current}/{total}] {action} {kind}")?;
        }
        ImportProgress::BuildingDocuments => {
            writeln!(output, "Import: building canonical documents")?;
        }
        ImportProgress::ProvingDocument {
            current,
            total,
            path,
        } => {
            writeln!(
                output,
                "Import: proving convergence [{current}/{total}] {path}"
            )?;
        }
    }

    output.flush()
}

/// `import`: inventory and prove a new workspace, then persist it unless this is a dry run.
pub async fn import<T: Transport>(
    engine: &Engine<T>,
    key: FingerprintKey,
    selection: ImportSelection,
    options: &ImportOptions,
    destination: &std::path::Path,
    dry_run: bool,
    streams: &mut Streams<'_>,
) -> Result<CommandStatus> {
    if options.environment == dokploy_engine::EnvironmentImportMode::Inline {
        writeln!(
            streams.diagnostics(),
            "Warning: --env=inline writes every environment value into declarative YAML."
        )
        .into_diagnostic()?;
    }
    let mut progress_error = None;
    let bundle = engine
        .import_with_progress(selection, &engine.fingerprinter(key), options, |progress| {
            if progress_error.is_none() {
                progress_error = write_import_progress(progress, streams.diagnostics()).err();
            }
        })
        .await;
    if let Some(error) = progress_error {
        return Err(error).into_diagnostic();
    }
    let bundle = bundle.into_diagnostic()?;
    let documents = bundle.documents().len();
    let resources: usize = bundle
        .documents()
        .iter()
        .map(|document| document.state().resources().len())
        .sum();
    let files = bundle.files().len();

    for category in [
        UnsupportedCategory::UnmanagedByChoice,
        UnsupportedCategory::Unsupported,
        UnsupportedCategory::Unrepresentable,
    ] {
        let entries: Vec<_> = bundle
            .unsupported()
            .iter()
            .filter(|entry| entry.category == category)
            .collect();
        if entries.is_empty() {
            continue;
        }
        let title = match category {
            UnsupportedCategory::UnmanagedByChoice => "Unmanaged by choice",
            UnsupportedCategory::Unsupported => "Unsupported",
            UnsupportedCategory::Unrepresentable => "Unrepresentable",
        };
        writeln!(streams.diagnostics(), "{title}:").into_diagnostic()?;
        for entry in entries {
            writeln!(
                streams.diagnostics(),
                "  {}.{}: {}",
                entry.kind,
                entry.item,
                entry.reason
            )
            .into_diagnostic()?;
        }
    }

    if dry_run {
        writeln!(
            streams.result(),
            "Import dry run complete: {documents} document(s), {resources} resource(s), {files} file(s); nothing written."
        )
        .into_diagnostic()?;
    } else {
        writeln!(streams.diagnostics(), "Import: writing workspace").into_diagnostic()?;
        streams.diagnostics().flush().into_diagnostic()?;
        bundle.persist(destination).into_diagnostic()?;
        writeln!(
            streams.result(),
            "Import complete: {documents} document(s), {resources} resource(s), {files} file(s)."
        )
        .into_diagnostic()?;
    }

    Ok(CommandStatus::Success)
}
