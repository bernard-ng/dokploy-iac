use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use clap::CommandFactory;
use miette::{IntoDiagnostic, Result};

use crate::cli::{Cli, Command};

/// Executes a declarative command without constructing connection dependencies.
pub fn execute(cli: Cli, output: &mut dyn Write, terminal_available: bool) -> Result<()> {
    match cli.command {
        Command::Init { file, empty } => {
            if !empty && !terminal_available {
                return Err(miette::miette!(
                    "non-interactive init requires the explicit `--empty` option"
                ));
            }

            dokploy_config::initialize(file).into_diagnostic()?;
            writeln!(output, "Created dokploy configuration.").into_diagnostic()?;

            Ok(())
        }
        Command::Schema => {
            serde_json::to_writer_pretty(
                &mut *output,
                &dokploy_config::DokployConfig::json_schema(),
            )
            .into_diagnostic()?;
            writeln!(output).into_diagnostic()?;

            Ok(())
        }
        Command::Validate { file } => validate_configuration(&file, output),
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "dokploy", output);
            Ok(())
        }
        Command::Plan { .. }
        | Command::Apply { .. }
        | Command::State { .. }
        | Command::Context { .. }
        | Command::Imperative(_) => Err(miette::miette!("command requires connection dispatch")),
    }
}

fn validate_configuration(path: &Path, output: &mut dyn Write) -> Result<()> {
    match dokploy_config::load(path) {
        Ok(_) => {
            writeln!(output, "Configuration is valid.").into_diagnostic()?;
            Ok(())
        }
        Err(dokploy_config::ConfigFileError::Configuration(error))
            if !error.diagnostics().is_empty() =>
        {
            let mut rendered = "Configuration failed semantic validation:".to_owned();

            for diagnostic in error.diagnostics() {
                let issue = diagnostic.issue();
                let location = diagnostic.location();
                write!(
                    rendered,
                    "\n  {} at line {}, column {}: {}",
                    issue.code(),
                    location.line(),
                    location.column(),
                    issue.message()
                )
                .expect("writing to a string cannot fail");
            }

            Err(miette::miette!(rendered))
        }
        Err(error) => Err(miette::miette!(error.to_string())),
    }
}
