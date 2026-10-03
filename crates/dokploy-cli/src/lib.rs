//! Presentation and local configuration for the `dokploy` command-line interface.

pub mod cli;
pub mod config;
pub mod credentials;
mod declarative;
mod fingerprint_key;
mod imperative;
mod imperative_generated;
mod plan_output;
mod redaction;
pub mod settings;
mod state_command;
pub mod telemetry;
pub mod workflow;
pub mod workspace;

/// Process-level outcome selected by a successfully dispatched command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandStatus {
    Success,
    Failure,
    ChangesPresent,
}

use std::io::{BufRead, Write};

use cli::{Cli, Command, ContextCommand};
use config::ConfigRepository;
use credentials::{ApiKey, CredentialStore};
use dokploy_sdk::Dokploy;
use miette::{IntoDiagnostic, Result};
use settings::{ConnectionOptions, ProcessEnvironment, resolve_connection, resolve_instance_url};
use std::sync::Arc;

pub use declarative::execute as execute_offline;

/// Executes one parsed command against injected local configuration services.
///
/// Keeping dispatch independent from process globals makes command behavior
/// testable without accessing the user's configuration directory or keyring.
pub async fn execute(
    cli: Cli,
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
    output: &mut dyn Write,
) -> Result<CommandStatus> {
    execute_with_input(cli, config, credentials, &mut std::io::empty(), output).await
}

/// Executes one parsed command with an injected confirmation input stream.
pub async fn execute_with_input(
    cli: Cli,
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<CommandStatus> {
    execute_with_terminal(cli, config, credentials, input, output, false).await
}

/// Executes one command with injectable terminal availability for interactive workflows.
pub async fn execute_with_terminal(
    cli: Cli,
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    terminal_available: bool,
) -> Result<CommandStatus> {
    execute_inner(
        cli,
        config,
        credentials,
        input,
        workflow::Streams::unified(output),
        terminal_available,
    )
    .await
}

/// Executes one command with independent result and diagnostic streams.
///
/// Machine-readable and requested result data is written to `result_output`.
/// Plans, prompts, warnings, and cancellation notices are written to
/// `diagnostic_output` so callers can safely parse standard output.
pub async fn execute_with_terminal_io(
    cli: Cli,
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
    input: &mut dyn BufRead,
    result_output: &mut dyn Write,
    diagnostic_output: &mut dyn Write,
    terminal_available: bool,
) -> Result<CommandStatus> {
    execute_inner(
        cli,
        config,
        credentials,
        input,
        workflow::Streams::split(result_output, diagnostic_output),
        terminal_available,
    )
    .await
}

async fn execute_inner(
    cli: Cli,
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
    input: &mut dyn BufRead,
    mut streams: workflow::Streams<'_>,
    terminal_available: bool,
) -> Result<CommandStatus> {
    if cli.is_offline() {
        execute_offline(cli, streams.result(), terminal_available)?;
        return Ok(CommandStatus::Success);
    }

    let Cli {
        url,
        api_key,
        command,
    } = cli;

    match command {
        Command::Init { .. }
        | Command::Schema { .. }
        | Command::Validate { .. }
        | Command::Completions { .. } => {
            unreachable!("offline commands return before connection dispatch")
        }
        Command::Context { command } => {
            match command {
                ContextCommand::List => list_contexts(config, streams.result())?,
                ContextCommand::Use { name } => use_context(config, &name, streams.result())?,
                ContextCommand::Show { name } => {
                    show_context(config, credentials, name.as_deref(), streams.result())?
                }
            }
            Ok(CommandStatus::Success)
        }
        Command::State { file, command } => {
            let configuration = config.load()?;
            let url = resolve_instance_url(url, &ProcessEnvironment, &configuration)?;
            let instance =
                dokploy_state::InstanceIdentity::parse(url.as_str()).into_diagnostic()?;
            let specs = dokploy_spec::embedded().into_diagnostic()?;
            dokploy_core::register_spec_kinds(&specs).into_diagnostic()?;
            let workspace = workspace::Workspace::load(&file)?;
            let document = workspace::document_id(&workspace.parse(&specs)?)?;
            state_command::execute(&workspace, document, instance, command, streams.result())
                .into_diagnostic()?;

            Ok(CommandStatus::Success)
        }
        Command::Plan {
            file,
            json,
            detailed_exitcode,
        } => {
            let (engine, workspace) = connect(url, api_key, config, credentials, &file)?;
            let key = fingerprint_key::load(engine.instance())?;
            workflow::plan(
                workflow::Context {
                    engine: &engine,
                    workspace: &workspace,
                    key,
                    environment: &process_environment(),
                },
                json,
                detailed_exitcode,
                &mut streams,
            )
            .await
        }
        Command::Apply { file, auto_approve } => {
            let (engine, workspace) = connect(url, api_key, config, credentials, &file)?;
            let key = fingerprint_key::load(engine.instance())?;
            workflow::apply(
                workflow::Context {
                    engine: &engine,
                    workspace: &workspace,
                    key,
                    environment: &process_environment(),
                },
                false,
                auto_approve,
                workflow::Terminal {
                    input,
                    available: terminal_available,
                },
                &mut streams,
            )
            .await
        }
        Command::Destroy { file, auto_approve } => {
            let (engine, workspace) = connect(url, api_key, config, credentials, &file)?;
            let key = fingerprint_key::load(engine.instance())?;
            workflow::apply(
                workflow::Context {
                    engine: &engine,
                    workspace: &workspace,
                    key,
                    environment: &process_environment(),
                },
                true,
                auto_approve,
                workflow::Terminal {
                    input,
                    available: terminal_available,
                },
                &mut streams,
            )
            .await
        }
        Command::Recover { file, auto_approve } => {
            let (engine, workspace) = connect(url, api_key, config, credentials, &file)?;
            let key = fingerprint_key::load(engine.instance())?;
            workflow::recover(
                workflow::Context {
                    engine: &engine,
                    workspace: &workspace,
                    key,
                    environment: &process_environment(),
                },
                auto_approve,
                workflow::Terminal {
                    input,
                    available: terminal_available,
                },
                &mut streams,
            )
            .await
        }
        Command::Api { command } => {
            let invocation = command.into_invocation()?;
            let client = client(url, api_key, config, credentials)?;
            let response = client
                .imperative()
                .execute(invocation.into_request())
                .await
                .into_diagnostic()?;

            let response = redaction::redact_response(response);
            serde_json::to_writer_pretty(&mut *streams.result(), &response).into_diagnostic()?;
            writeln!(streams.result()).into_diagnostic()?;

            Ok(CommandStatus::Success)
        }
    }
}

fn process_environment() -> workflow::Environment {
    Arc::new(|name: &str| std::env::var(name).ok())
}

fn client(
    url: Option<String>,
    api_key: Option<String>,
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
) -> Result<Dokploy> {
    let configuration = config.load()?;
    let settings = resolve_connection(
        ConnectionOptions {
            url,
            api_key: api_key.map(ApiKey::new),
        },
        &ProcessEnvironment,
        &configuration,
        credentials,
    )?;

    Dokploy::builder()
        .url(settings.url().as_str())
        .api_key(settings.api_key().expose())
        .build()
        .into_diagnostic()
}

/// The engine over the connection, and the document it will work on.
fn connect(
    url: Option<String>,
    api_key: Option<String>,
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
    file: &std::path::Path,
) -> Result<(dokploy_engine::Engine<Dokploy>, workspace::Workspace)> {
    let workspace = workspace::Workspace::load(file)?;
    let client = client(url, api_key, config, credentials)?;
    let specs = dokploy_spec::embedded().into_diagnostic()?;
    let engine = dokploy_engine::Engine::new(specs, client).into_diagnostic()?;

    Ok((engine, workspace))
}

fn list_contexts(config: &ConfigRepository, output: &mut dyn Write) -> Result<()> {
    let configuration = config.load()?;

    if configuration.contexts().is_empty() {
        writeln!(output, "No contexts configured.").into_diagnostic()?;
        return Ok(());
    }

    for (name, context) in configuration.contexts() {
        let marker = if configuration.current_context() == Some(name.as_str()) {
            '*'
        } else {
            ' '
        };

        writeln!(output, "{marker} {name}\t{}", context.url()).into_diagnostic()?;
    }

    Ok(())
}

fn use_context(config: &ConfigRepository, name: &str, output: &mut dyn Write) -> Result<()> {
    config.select(name)?;
    writeln!(output, "Switched to context `{name}`.").into_diagnostic()?;

    Ok(())
}

fn show_context(
    config: &ConfigRepository,
    credentials: &dyn CredentialStore,
    requested_name: Option<&str>,
    output: &mut dyn Write,
) -> Result<()> {
    let configuration = config.load()?;
    let (name, context) = configuration.context(requested_name)?;
    let credential_status = if credentials.get(name)?.is_some() {
        "stored"
    } else {
        "not stored"
    };

    writeln!(output, "name: {name}").into_diagnostic()?;
    writeln!(output, "url: {}", context.url()).into_diagnostic()?;
    writeln!(output, "api key: {credential_status}").into_diagnostic()?;

    Ok(())
}
