//! The declarative commands that talk to Dokploy, over any transport.
//!
//! They are generic over the engine's transport so the same code runs against the real client
//! and against the simulator in tests.

use std::io::{BufRead, Write};
use std::sync::Arc;

use dokploy_core::Plan;
use dokploy_engine::{
    ApplyError, Compiled, Engine, FingerprintKey, RecoverError, WorkspaceSecrets,
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
    terminal: &mut Terminal<'_>,
    diagnostics: &mut dyn Write,
    operation: &str,
    question: &str,
) -> Result<bool> {
    plan_output::render(plan, diagnostics).into_diagnostic()?;
    if auto_approve || plan.changes().is_empty() || !plan.complete() || !plan.applyable() {
        return Ok(true);
    }

    confirm(terminal, diagnostics, operation, question)
}

/// `apply` and `destroy`: execute the plan, one journaled step at a time.
pub async fn apply<T: Transport>(
    context: Context<'_, T>,
    destroy: bool,
    auto_approve: bool,
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
