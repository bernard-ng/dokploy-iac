//! The declarative commands, driven through the same workflows the binary uses, against the
//! simulator instead of a socket.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use dokploy_cli::cli::Cli;
use dokploy_cli::workflow::{self, Context, Environment, Streams, Terminal};
use dokploy_cli::workspace::Workspace;
use dokploy_cli::{CommandStatus, execute_offline};
use dokploy_engine::{Engine, FingerprintKey};
use dokploy_sim::{Fault, FaultKind, Sim};

const KEY: &str = "0199a0c8-2351-7c31-8899-2c8f81983ea5:0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
const TOKEN: &str = "cli-token-canary-1";

const SETTINGS: &str = "\
version: 2
settings:
  registries:
    ghcr:
      type: cloud
      url: ghcr.io
      username: ci
      password: { env: GHCR_TOKEN }
  tags:
    blue: { color: \"#0000ff\" }
";

struct Fixture {
    sim: Sim,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/live/v0.30.6");
        let specs = dokploy_spec::embedded().expect("embedded specs");
        Self {
            sim: Sim::new(Arc::new(specs)).with_fixtures(&fixtures),
            directory: tempfile::tempdir().unwrap(),
        }
    }

    fn engine(&self) -> Engine<&Sim> {
        Engine::new(dokploy_spec::embedded().unwrap(), &self.sim).unwrap()
    }

    fn file(&self, text: &str) -> Workspace {
        let path: PathBuf = self.directory.path().join("dokploy.settings.yaml");
        std::fs::write(&path, text).unwrap();
        Workspace::load(&path).unwrap()
    }
}

fn key() -> FingerprintKey {
    FingerprintKey::parse_explicit(KEY).unwrap()
}

fn environment() -> Environment {
    let variables = BTreeMap::from([("GHCR_TOKEN".to_owned(), TOKEN.to_owned())]);
    Arc::new(move |name: &str| variables.get(name).cloned())
}

struct Run {
    status: Result<CommandStatus, String>,
    result: String,
    diagnostics: String,
}

async fn apply(
    fixture: &Fixture,
    text: &str,
    destroy: bool,
    auto_approve: bool,
    input: &str,
    terminal: bool,
) -> Run {
    let engine = fixture.engine();
    let workspace = fixture.file(text);
    let (mut result, mut diagnostics) = (Vec::new(), Vec::new());
    let mut stdin = Cursor::new(input.as_bytes().to_vec());
    let status = workflow::apply(
        Context {
            engine: &engine,
            workspace: &workspace,
            key: key(),
            environment: &environment(),
        },
        destroy,
        auto_approve,
        Terminal {
            input: &mut stdin,
            available: terminal,
        },
        &mut Streams::split(&mut result, &mut diagnostics),
    )
    .await
    .map_err(|error| format!("{error:?}"));

    Run {
        status,
        result: String::from_utf8(result).unwrap(),
        diagnostics: String::from_utf8(diagnostics).unwrap(),
    }
}

async fn plan(
    fixture: &Fixture,
    text: &str,
    json: bool,
    detailed: bool,
) -> (CommandStatus, String) {
    let engine = fixture.engine();
    let workspace = fixture.file(text);
    let mut result = Vec::new();
    let status = workflow::plan(
        Context {
            engine: &engine,
            workspace: &workspace,
            key: key(),
            environment: &environment(),
        },
        json,
        detailed,
        &mut Streams::unified(&mut result),
    )
    .await
    .expect("plans");

    (status, String::from_utf8(result).unwrap())
}

#[tokio::test]
async fn plan_shows_paths_never_values_and_reports_changes_through_the_exit_status() {
    let fixture = Fixture::new();

    let (status, output) = plan(&fixture, SETTINGS, false, true).await;

    assert_eq!(status, CommandStatus::ChangesPresent);
    assert!(output.contains("create registry.ghcr"), "{output}");
    assert!(output.contains("create tag.blue"), "{output}");
    assert!(output.contains("property: password"), "{output}");
    for value in [TOKEN, "ghcr.io", "#0000ff"] {
        assert!(!output.contains(value), "{value} leaked: {output}");
    }
    let (plain, _) = plan(&fixture, SETTINGS, false, false).await;
    assert_eq!(
        plain,
        CommandStatus::Success,
        "changes are not a failure without the flag"
    );
    let (_, json) = plan(&fixture, SETTINGS, true, false).await;
    serde_json::from_str::<serde_json::Value>(json.trim()).expect("the plan is JSON");
    assert!(
        fixture.sim.mutations().is_empty(),
        "planning changed something"
    );
}

#[tokio::test]
async fn apply_with_approval_creates_then_converges() {
    let fixture = Fixture::new();

    let run = apply(&fixture, SETTINGS, false, false, "yes\n", true).await;

    assert_eq!(
        run.status,
        Ok(CommandStatus::Success),
        "{}",
        run.diagnostics
    );
    assert_eq!(
        run.result, "Apply complete: 2 change(s).\n",
        "results stay machine-safe"
    );
    assert!(run.diagnostics.contains("Type 'yes' to continue"));
    assert_eq!(fixture.sim.objects("registry").len(), 1);
    assert_eq!(fixture.sim.objects("tag").len(), 1);

    let (status, _) = plan(&fixture, SETTINGS, false, true).await;
    assert_eq!(status, CommandStatus::Success, "the next plan is empty");
}

#[tokio::test]
async fn anything_but_yes_cancels_without_changing_anything() {
    let fixture = Fixture::new();

    let run = apply(&fixture, SETTINGS, false, false, "y\n", true).await;

    assert_eq!(run.status, Ok(CommandStatus::Success));
    assert!(run.diagnostics.contains("Apply cancelled."));
    assert!(run.result.is_empty());
    assert!(fixture.sim.mutations().is_empty());
}

#[tokio::test]
async fn without_a_terminal_apply_needs_auto_approve_and_closed_input_is_not_consent() {
    let fixture = Fixture::new();

    let run = apply(&fixture, SETTINGS, false, false, "yes\n", false).await;

    let error = run.status.expect_err("approval requires a terminal");
    assert!(error.contains("--auto-approve"), "{error}");
    assert!(fixture.sim.mutations().is_empty());

    let run = apply(&fixture, SETTINGS, false, true, "", false).await;
    assert_eq!(run.status, Ok(CommandStatus::Success));
    assert_eq!(fixture.sim.objects("tag").len(), 1);
}

#[tokio::test]
async fn destroy_removes_what_the_state_tracks_after_confirmation() {
    let fixture = Fixture::new();
    apply(&fixture, SETTINGS, false, true, "", false)
        .await
        .status
        .unwrap();

    let run = apply(&fixture, SETTINGS, true, false, "yes\n", true).await;

    assert_eq!(
        run.status,
        Ok(CommandStatus::Success),
        "{}",
        run.diagnostics
    );
    assert_eq!(run.result, "Destroy complete: 2 resource(s) deleted.\n");
    assert!(run.diagnostics.contains("Destroy all tracked resources?"));
    assert!(fixture.sim.objects("registry").is_empty());
    assert!(fixture.sim.objects("tag").is_empty());
}

#[tokio::test]
async fn recover_settles_an_apply_whose_response_was_lost() {
    let fixture = Fixture::new();
    fixture
        .sim
        .inject(Fault::new("tag.create", FaultKind::DropAfter));
    let run = apply(&fixture, SETTINGS, false, true, "", false).await;
    assert!(
        run.status
            .expect_err("the outcome is unknown")
            .contains("unknown")
    );

    let engine = fixture.engine();
    let workspace = fixture.file(SETTINGS);
    let (mut result, mut diagnostics) = (Vec::new(), Vec::new());
    let mut stdin = Cursor::new(Vec::new());
    let status = workflow::recover(
        Context {
            engine: &engine,
            workspace: &workspace,
            key: key(),
            environment: &environment(),
        },
        true,
        Terminal {
            input: &mut stdin,
            available: false,
        },
        &mut Streams::split(&mut result, &mut diagnostics),
    )
    .await
    .expect("recovers");

    assert_eq!(status, CommandStatus::Success);
    assert_eq!(
        String::from_utf8(result).unwrap(),
        "Recovery complete: 1 interrupted step(s) resolved.\n"
    );
    assert!(
        String::from_utf8(diagnostics)
            .unwrap()
            .contains("AdoptCreatedResource")
    );
    assert_eq!(fixture.sim.objects("tag").len(), 1, "never created twice");
}

fn offline(args: &[&str]) -> Result<String, String> {
    let cli = Cli::try_parse_from(std::iter::once("dokploy").chain(args.iter().copied())).unwrap();
    let mut output = Vec::new();
    execute_offline(cli, &mut output, false)
        .map(|()| String::from_utf8(output).unwrap())
        .map_err(|error| format!("{error:?}"))
}

#[test]
fn init_writes_documents_that_validate_and_never_overwrites() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("dokploy.yaml");
    let settings = directory.path().join("settings.yaml");
    let project_arg = project.to_str().unwrap();
    let settings_arg = settings.to_str().unwrap();

    offline(&["init", "--file", project_arg]).unwrap();
    offline(&["init", "--settings", "--file", settings_arg]).unwrap();
    assert_eq!(
        offline(&["validate", "--file", project_arg]).unwrap(),
        "Document is valid.\n"
    );
    assert_eq!(
        offline(&["validate", "--file", settings_arg]).unwrap(),
        "Document is valid.\n"
    );

    let again = offline(&["init", "--file", project_arg]).unwrap_err();
    assert!(again.contains("cannot create"), "{again}");
    let empty = directory.path().join("empty.yaml");
    offline(&["init", "--empty", "--file", empty.to_str().unwrap()]).unwrap();
    assert!(
        std::fs::read_to_string(empty)
            .unwrap()
            .starts_with("version: 2")
    );
}

#[test]
fn validate_reports_every_problem_with_its_place_and_never_echoes_a_secret() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("bad.yaml");
    std::fs::write(
        &file,
        "version: 2\nsettings:\n  registries:\n    ghcr:\n      type: cloud\n      password: literal-secret-canary\n      colour: red\n",
    )
    .unwrap();

    let error = offline(&["validate", "--file", file.to_str().unwrap()]).unwrap_err();

    assert!(error.contains("DOKDOC008"), "{error}");
    assert!(error.contains("DOKDOC004"), "{error}");
    assert!(error.contains("6:17"), "{error}");
    assert!(!error.contains("literal-secret-canary"), "{error}");
}

#[test]
fn validate_refuses_a_missing_file_and_a_first_engine_document() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("nope.yaml");
    assert!(
        offline(&["validate", "--file", missing.to_str().unwrap()])
            .unwrap_err()
            .contains("cannot read")
    );

    let old = directory.path().join("old.yaml");
    std::fs::write(&old, "version: 1\nproject:\n  name: platform\n").unwrap();
    let error = offline(&["validate", "--file", old.to_str().unwrap()]).unwrap_err();
    assert!(error.contains("DOKDOC002"), "{error}");
}

#[test]
fn schema_describes_both_documents_from_the_specs() {
    for (document, kind) in [("project", "redirect"), ("settings", "registry")] {
        let schema = offline(&["schema", "--document", document]).unwrap();
        let value: serde_json::Value = serde_json::from_str(&schema).unwrap();
        assert!(value.is_object());
        assert!(schema.contains(kind), "{document} schema lacks {kind}");
    }
}
