use std::fs;
use std::io::Write;
use std::path::PathBuf;

use clap::CommandFactory;
use dokploy_spec::Scope;
use miette::{IntoDiagnostic, Result};

use crate::cli::{Cli, Command, SchemaDocument};
use crate::workspace::{DEFAULT_PROJECT_FILE, DEFAULT_SETTINGS_FILE, Workspace};

const PROJECT_TEMPLATE: &str = "\
# A Dokploy project: environments, and inside them the services. See `dokploy schema`.
version: 2
project:
  slug: my-project
  # environments:
  #   production:
  #     applications:
  #       web:
  #         redirects:
  #           www: { regex: \"^https?://www\\\\.(.*)\", replacement: \"https://$1\", permanent: true }
";

const SETTINGS_TEMPLATE: &str = "\
# Instance settings: registries and tags. See `dokploy schema --document settings`.
version: 2
settings: {}
  # registries:
  #   ghcr:
  #     type: cloud
  #     url: ghcr.io
  #     username: ci
  #     password: { env: GHCR_TOKEN }
  # tags:
  #   production: { color: \"#e11d48\" }
";

const EMPTY_PROJECT: &str = "version: 2\nproject:\n  slug: my-project\n";
const EMPTY_SETTINGS: &str = "version: 2\nsettings: {}\n";

/// Executes a declarative command without constructing connection dependencies.
pub fn execute(cli: Cli, output: &mut dyn Write, _terminal_available: bool) -> Result<()> {
    match cli.command {
        Command::Init {
            file,
            empty,
            settings,
        } => {
            let (default, template, noun) = match (settings, empty) {
                (false, false) => (DEFAULT_PROJECT_FILE, PROJECT_TEMPLATE, "project"),
                (false, true) => (DEFAULT_PROJECT_FILE, EMPTY_PROJECT, "project"),
                (true, false) => (DEFAULT_SETTINGS_FILE, SETTINGS_TEMPLATE, "settings"),
                (true, true) => (DEFAULT_SETTINGS_FILE, EMPTY_SETTINGS, "settings"),
            };
            let file = file.unwrap_or_else(|| PathBuf::from(default));
            let mut handle = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)
                .map_err(|error| miette::miette!("cannot create {}: {error}", file.display()))?;
            handle.write_all(template.as_bytes()).into_diagnostic()?;
            writeln!(output, "Created dokploy {noun} document.").into_diagnostic()?;

            Ok(())
        }
        Command::Schema { document } => {
            let specs = dokploy_spec::embedded().into_diagnostic()?;
            let scope = match document {
                SchemaDocument::Project => Scope::Project,
                SchemaDocument::Settings => Scope::Settings,
            };
            let schema = dokploy_model::json_schema(&specs, scope);
            serde_json::to_writer_pretty(&mut *output, &schema).into_diagnostic()?;
            writeln!(output).into_diagnostic()?;

            Ok(())
        }
        Command::Validate { file } => {
            let specs = dokploy_spec::embedded().into_diagnostic()?;
            Workspace::load(&file)?.parse(&specs)?;
            writeln!(output, "Document is valid.").into_diagnostic()?;

            Ok(())
        }
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "dokploy", output);
            Ok(())
        }
        Command::Plan { .. }
        | Command::Apply { .. }
        | Command::Recover { .. }
        | Command::Destroy { .. }
        | Command::State { .. }
        | Command::Context { .. }
        | Command::Api { .. } => Err(miette::miette!("command requires connection dispatch")),
    }
}
