use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use dokploy_spec::{OperationIndex, Scope, check_ledger};
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
        Command::Coverage { kind } => coverage(kind.as_deref(), output),
        Command::Fmt { file, all, check } => format_documents(file, all, check, output),
        Command::Completions { shell } => {
            let generated = std::thread::Builder::new()
                .name("dokploy-completions".to_owned())
                .stack_size(16 * 1024 * 1024)
                .spawn(move || {
                    let mut generated = Vec::new();
                    clap_complete::generate(shell, &mut Cli::command(), "dokploy", &mut generated);
                    generated
                })
                .into_diagnostic()?
                .join()
                .map_err(|_| miette::miette!("completion generation failed"))?;
            output.write_all(&generated).into_diagnostic()?;
            Ok(())
        }
        Command::Plan { .. }
        | Command::Apply { .. }
        | Command::Deploy { .. }
        | Command::Recover { .. }
        | Command::Destroy { .. }
        | Command::State { .. }
        | Command::Context { .. }
        | Command::Import { .. }
        | Command::Doctor
        | Command::Api { .. } => Err(miette::miette!("command requires connection dispatch")),
    }
}

fn coverage(kind: Option<&str>, output: &mut dyn Write) -> Result<()> {
    let specs = dokploy_spec::embedded().into_diagnostic()?;
    let openapi = dokploy_spec::embedded_openapi().into_diagnostic()?;
    let ledger = check_ledger(&specs, &OperationIndex::from_openapi(&openapi));
    let selected = match kind {
        Some(kind) => {
            specs
                .get(kind)
                .ok_or_else(|| miette::miette!("unknown resource kind `{kind}`"))?;
            ledger
                .kinds
                .iter()
                .filter(|entry| entry.kind == kind)
                .collect::<Vec<_>>()
        }
        None => ledger.kinds.iter().collect(),
    };

    writeln!(
        output,
        "kind\tcoverage\tmapped\treadonly\taction\tignored\tderived\tunclassified"
    )
    .into_diagnostic()?;
    for entry in selected {
        writeln!(
            output,
            "{}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{}",
            entry.kind,
            entry.coverage,
            entry.mapped,
            entry.readonly,
            entry.action,
            entry.ignored,
            entry.derived,
            entry.unclassified.len()
        )
        .into_diagnostic()?;
    }

    if let Some(kind) = kind {
        let spec = specs
            .get(kind)
            .expect("the requested kind was checked above");
        writeln!(output, "\n{} ({kind})", spec.title).into_diagnostic()?;
        writeln!(output, "scope: {:?}", spec.scope).into_diagnostic()?;
        writeln!(output, "class: {:?}", spec.class).into_diagnostic()?;
        writeln!(output, "section: {}", spec.section).into_diagnostic()?;
        if !spec.parents.is_empty() {
            writeln!(output, "parents: {}", spec.parents.join(", ")).into_diagnostic()?;
        }
        writeln!(output, "fields:").into_diagnostic()?;
        for (name, field) in &spec.fields {
            let api = field.api.as_deref().unwrap_or(name);
            writeln!(
                output,
                "  {name}: type={} api={} class={:?} mutability={:?}",
                field.ty, api, field.class, field.mutability
            )
            .into_diagnostic()?;
        }
    }

    Ok(())
}

fn format_documents(
    file: Option<PathBuf>,
    all: bool,
    check: bool,
    output: &mut dyn Write,
) -> Result<()> {
    let specs = dokploy_spec::embedded().into_diagnostic()?;
    let workspaces = if all || file.is_none() {
        let manifest = crate::workspace::WorkspaceManifest::load(Path::new("."))?;
        manifest
            .documents()
            .iter()
            .map(|path| crate::workspace::Workspace::load_in(manifest.directory(), path))
            .collect::<Result<Vec<_>>>()?
    } else {
        vec![crate::workspace::Workspace::load(
            file.as_deref().expect("explicit file is present"),
        )?]
    };

    let mut changed = 0_usize;
    for workspace in &workspaces {
        let canonical = workspace.parse(&specs)?.render();
        let current = fs::read_to_string(workspace.file()).into_diagnostic()?;
        if current == canonical {
            continue;
        }
        changed += 1;
        if !check {
            write_atomic(workspace.file(), canonical.as_bytes())?;
        }
        writeln!(output, "{}", workspace.file().display()).into_diagnostic()?;
    }

    if check && changed > 0 {
        return Err(miette::miette!(
            "{changed} document(s) need canonical formatting"
        ));
    }
    writeln!(output, "Formatted {changed} document(s).").into_diagnostic()?;

    Ok(())
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| miette::miette!("{} has no parent directory", path.display()))?;
    let permissions = fs::metadata(path).into_diagnostic()?.permissions();
    let mut temporary = tempfile::NamedTempFile::new_in(parent).into_diagnostic()?;
    temporary.write_all(content).into_diagnostic()?;
    temporary.as_file_mut().sync_all().into_diagnostic()?;
    temporary
        .as_file()
        .set_permissions(permissions)
        .into_diagnostic()?;
    temporary
        .persist(path)
        .map_err(|error| miette::miette!("cannot replace {}: {}", path.display(), error.error))?;

    Ok(())
}
