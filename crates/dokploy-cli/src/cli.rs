use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::imperative_generated::ImperativeCommand;

/// Manage Dokploy resources from the command line.
#[derive(Parser)]
#[command(name = "dokploy", version, propagate_version = true)]
pub struct Cli {
    /// Override the Dokploy base URL.
    #[arg(long, global = true, value_name = "URL")]
    pub url: Option<String>,

    /// Override the Dokploy API key.
    #[arg(long, global = true, value_name = "KEY")]
    pub api_key: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    #[must_use]
    pub const fn is_offline(&self) -> bool {
        matches!(
            self.command,
            Command::Init { .. }
                | Command::Schema { .. }
                | Command::Validate { .. }
                | Command::Completions { .. }
        )
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a starter declarative configuration.
    Init {
        /// Configuration file to create. Defaults to `dokploy.yaml`, or to
        /// `dokploy.settings.yaml` with `--settings`.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,

        /// Create the canonical empty configuration without prompting.
        #[arg(long)]
        empty: bool,

        /// Create an instance settings document instead of a project document.
        #[arg(long)]
        settings: bool,
    },

    /// Print the declarative configuration JSON Schema.
    Schema {
        /// Which document the schema describes.
        #[arg(long, value_enum, default_value_t = SchemaDocument::Project)]
        document: SchemaDocument,
    },

    /// Parse and validate a declarative configuration offline.
    Validate {
        /// Configuration file to validate.
        #[arg(
            long,
            default_value = dokploy_config::DEFAULT_CONFIG_FILE,
            value_name = "PATH"
        )]
        file: PathBuf,
    },

    /// Generate a shell completion script on standard output.
    Completions {
        /// Shell whose completion format should be generated.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Compare configuration, durable state, and fresh Dokploy state without mutating anything.
    Plan {
        /// Configuration file to plan.
        #[arg(
            long,
            default_value = dokploy_config::DEFAULT_CONFIG_FILE,
            value_name = "PATH"
        )]
        file: PathBuf,

        /// Print the deterministic redaction-safe JSON plan.
        #[arg(long)]
        json: bool,

        /// Return status 2 when a complete, applyable plan contains changes.
        #[arg(long)]
        detailed_exitcode: bool,

        /// Save a bound plan envelope for a later verified apply.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
    },

    /// Preview and reconcile configuration against fresh Dokploy state.
    Apply {
        /// Saved plan to verify against fresh evidence before applying.
        #[arg(value_name = "PLAN")]
        plan: Option<PathBuf>,

        /// Configuration file to apply.
        #[arg(
            long,
            default_value = dokploy_config::DEFAULT_CONFIG_FILE,
            value_name = "PATH"
        )]
        file: PathBuf,

        /// Maximum number of independent remote operations in flight.
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=64))]
        parallelism: u8,

        /// Apply a complete plan without interactive confirmation.
        #[arg(long)]
        auto_approve: bool,
    },

    /// Reconcile an interrupted apply from durable journal evidence.
    Recover {
        /// Configuration file whose workspace contains the interrupted operation.
        #[arg(
            long,
            default_value = dokploy_config::DEFAULT_CONFIG_FILE,
            value_name = "PATH"
        )]
        file: PathBuf,

        /// Complete a fully verified recovery without interactive confirmation.
        #[arg(long)]
        auto_approve: bool,
    },

    /// Delete every resource tracked by this workspace from Dokploy.
    Destroy {
        /// Configuration file whose directory owns the state.
        #[arg(
            long,
            default_value = dokploy_config::DEFAULT_CONFIG_FILE,
            value_name = "PATH"
        )]
        file: PathBuf,

        /// Execute a complete destruction plan without interactive confirmation.
        #[arg(long)]
        auto_approve: bool,
    },

    /// Adopt an existing Dokploy project into canonical configuration and state.
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },

    /// Inspect resources tracked in the local workspace state.
    State {
        /// Configuration file whose directory owns the state.
        #[arg(
            long,
            default_value = dokploy_config::DEFAULT_CONFIG_FILE,
            value_name = "PATH"
        )]
        file: PathBuf,

        #[command(subcommand)]
        command: StateCommand,
    },

    /// Inspect or select local connection contexts.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },

    /// Call an operation from the pinned Dokploy API contract.
    #[command(flatten)]
    Imperative(Box<ImperativeCommand>),
}

/// Document kinds that have a JSON Schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum SchemaDocument {
    /// A `project:` document.
    Project,
    /// A `settings:` document.
    Settings,
}

#[derive(Subcommand)]
pub enum ImportCommand {
    /// Adopt one whole project, with every environment and resource below it.
    ///
    /// Import only creates a new workspace; it refuses to run when the
    /// configuration file or durable state already exists.
    Project {
        /// Existing Dokploy project identifier. Omit it to choose from a list.
        project_id: Option<String>,

        /// Configuration file to create.
        #[arg(
            long,
            default_value = dokploy_config::DEFAULT_CONFIG_FILE,
            value_name = "PATH"
        )]
        file: PathBuf,
    },

    /// Adopt the instance settings (tags) into a settings document.
    ///
    /// Like project import, this only creates a new workspace: it refuses to
    /// run when the settings file or the settings state already exists.
    Settings {
        /// Settings file to create.
        #[arg(
            long,
            default_value = dokploy_config::DEFAULT_SETTINGS_FILE,
            value_name = "PATH"
        )]
        file: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum ContextCommand {
    /// List configured contexts.
    List,

    /// Select the context used by default.
    Use {
        /// Name of the context to select.
        name: String,
    },

    /// Show a context without revealing its API key.
    Show {
        /// Context to show. Defaults to the selected context.
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum StateCommand {
    /// List tracked logical resource addresses.
    List,

    /// Show one tracked resource without exposing managed values or receipts.
    Show {
        /// Logical resource address, such as `application.api`.
        address: String,
    },

    /// Move one tracked logical address without changing Dokploy.
    Mv {
        /// Existing logical resource address.
        source: String,

        /// New logical resource address of the same kind.
        target: String,
    },

    /// Stop tracking one resource without deleting it from Dokploy.
    Rm {
        /// Logical resource address to forget.
        address: String,
    },

    /// Prevent declarative deletion of one tracked resource.
    Protect {
        /// Logical resource address to protect.
        address: String,
    },

    /// Allow declarative deletion of one tracked resource.
    Unprotect {
        /// Logical resource address to unprotect.
        address: String,
    },
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;

    use super::{Cli, Command, ContextCommand, ImportCommand, SchemaDocument, StateCommand};

    #[test]
    fn parses_direct_and_interactive_project_import() {
        let direct = Cli::try_parse_from([
            "dokploy",
            "import",
            "project",
            "project-1",
            "--file",
            "platform.yaml",
        ])
        .expect("direct project import is valid");
        assert!(matches!(
            direct.command,
            Command::Import {
                command: ImportCommand::Project {
                    project_id: Some(ref id),
                    ref file,
                },
            } if id == "project-1" && file.as_path() == Path::new("platform.yaml")
        ));

        let interactive = Cli::try_parse_from(["dokploy", "import", "project"])
            .expect("interactive project import is valid");
        assert!(matches!(
            interactive.command,
            Command::Import {
                command: ImportCommand::Project {
                    project_id: None,
                    ref file,
                },
            } if file.as_path() == Path::new("dokploy.yaml")
        ));
    }

    #[test]
    fn rejects_the_removed_per_resource_import_forms() {
        for arguments in [
            vec!["dokploy", "import"],
            vec![
                "dokploy",
                "import",
                "postgres",
                "postgres-1",
                "--as",
                "postgres.main",
            ],
            vec![
                "dokploy",
                "import",
                "project",
                "project-1",
                "--as",
                "project.main",
            ],
        ] {
            assert!(
                Cli::try_parse_from(&arguments).is_err(),
                "{arguments:?} must no longer parse"
            );
        }
    }

    #[test]
    fn parses_context_use() {
        let cli = Cli::try_parse_from(["dokploy", "context", "use", "production"])
            .expect("command line is valid");

        assert!(matches!(
            cli.command,
            Command::Context {
                command: ContextCommand::Use { ref name }
            } if name == "production"
        ));
    }

    #[test]
    fn init_uses_the_default_configuration_path() {
        let cli =
            Cli::try_parse_from(["dokploy", "init", "--empty"]).expect("command line is valid");

        assert!(matches!(
            cli.command,
            Command::Init {
                file: None,
                empty: true,
                settings: false
            }
        ));
    }

    #[test]
    fn plain_init_is_not_explicitly_empty() {
        let cli = Cli::try_parse_from(["dokploy", "init"]).expect("command line is valid");

        assert!(matches!(cli.command, Command::Init { empty: false, .. }));
    }

    #[test]
    fn parses_schema_as_an_offline_top_level_command() {
        let cli = Cli::try_parse_from(["dokploy", "schema"]).expect("command line is valid");

        assert!(matches!(
            cli.command,
            Command::Schema {
                document: SchemaDocument::Project
            }
        ));
    }

    #[test]
    fn schema_and_init_select_the_settings_document() {
        let cli = Cli::try_parse_from(["dokploy", "schema", "--document", "settings"])
            .expect("command line is valid");
        assert!(matches!(
            cli.command,
            Command::Schema {
                document: SchemaDocument::Settings
            }
        ));

        let cli = Cli::try_parse_from(["dokploy", "init", "--settings", "--empty"])
            .expect("command line is valid");
        assert!(matches!(
            cli.command,
            Command::Init {
                settings: true,
                file: None,
                ..
            }
        ));
    }

    #[test]
    fn import_settings_defaults_to_the_settings_file() {
        let cli =
            Cli::try_parse_from(["dokploy", "import", "settings"]).expect("command line is valid");
        assert!(matches!(
            cli.command,
            Command::Import { command: ImportCommand::Settings { file } }
                if file.as_path() == Path::new("dokploy.settings.yaml")
        ));
    }

    #[test]
    fn validate_accepts_a_configuration_path_override() {
        let cli = Cli::try_parse_from(["dokploy", "validate", "--file", "custom.yaml"])
            .expect("command line is valid");

        assert!(matches!(
            cli.command,
            Command::Validate { file } if file.as_path() == Path::new("custom.yaml")
        ));
    }

    #[test]
    fn validate_uses_the_default_configuration_path() {
        let cli = Cli::try_parse_from(["dokploy", "validate"]).expect("command line is valid");

        assert!(matches!(
            cli.command,
            Command::Validate { file } if file.as_path() == Path::new("dokploy.yaml")
        ));
    }

    #[test]
    fn completions_selects_a_supported_shell_offline() {
        let cli = Cli::try_parse_from(["dokploy", "completions", "zsh"])
            .expect("completion command line is valid");

        assert!(matches!(
            cli.command,
            Command::Completions {
                shell: clap_complete::Shell::Zsh
            }
        ));
        assert!(cli.is_offline());
    }

    #[test]
    fn plan_supports_json_and_detailed_exit_status() {
        let cli = Cli::try_parse_from([
            "dokploy",
            "plan",
            "--file",
            "stack.yaml",
            "--json",
            "--detailed-exitcode",
            "--out",
            "plan.json",
        ])
        .expect("plan command line is valid");

        assert!(matches!(
            cli.command,
            Command::Plan {
                file,
                json: true,
                detailed_exitcode: true,
                out: Some(out),
            } if file.as_path() == Path::new("stack.yaml")
                && out.as_path() == Path::new("plan.json")
        ));
    }

    #[test]
    fn recover_supports_non_interactive_approval() {
        let cli = Cli::try_parse_from([
            "dokploy",
            "recover",
            "--file",
            "stack.yaml",
            "--auto-approve",
        ])
        .expect("recovery command line is valid");

        assert!(matches!(
            cli.command,
            Command::Recover {
                file,
                auto_approve: true
            } if file.as_path() == Path::new("stack.yaml")
        ));
    }

    #[test]
    fn destroy_supports_non_interactive_approval() {
        let cli = Cli::try_parse_from([
            "dokploy",
            "destroy",
            "--file",
            "stack.yaml",
            "--auto-approve",
        ])
        .expect("destroy command line is valid");

        assert!(matches!(
            cli.command,
            Command::Destroy {
                file,
                auto_approve: true
            } if file.as_path() == Path::new("stack.yaml")
        ));
    }

    #[test]
    fn apply_uses_safe_defaults_and_accepts_bounded_parallelism() {
        let default =
            Cli::try_parse_from(["dokploy", "apply"]).expect("default apply command line is valid");
        assert!(matches!(
            default.command,
            Command::Apply {
                plan: None,
                file,
                parallelism: 4,
                auto_approve: false,
            }
                if file.as_path() == Path::new("dokploy.yaml")
        ));

        let explicit = Cli::try_parse_from([
            "dokploy",
            "apply",
            "plan.json",
            "--file",
            "stack.yaml",
            "--parallelism",
            "8",
            "--auto-approve",
        ])
        .expect("explicit apply command line is valid");
        assert!(matches!(
            explicit.command,
            Command::Apply {
                plan: Some(plan),
                file,
                parallelism: 8,
                auto_approve: true,
            }
                if plan.as_path() == Path::new("plan.json")
                    && file.as_path() == Path::new("stack.yaml")
        ));
        assert!(Cli::try_parse_from(["dokploy", "apply", "--parallelism", "0"]).is_err());
        assert!(Cli::try_parse_from(["dokploy", "apply", "--parallelism", "65"]).is_err());
    }

    #[test]
    fn state_list_and_show_use_the_selected_workspace() {
        let list = Cli::try_parse_from(["dokploy", "state", "--file", "stack.yaml", "list"])
            .expect("state list command line is valid");
        assert!(matches!(
            list.command,
            Command::State {
                file,
                command: StateCommand::List,
            } if file.as_path() == Path::new("stack.yaml")
        ));

        let show = Cli::try_parse_from(["dokploy", "state", "show", "application.api"])
            .expect("state show command line is valid");
        assert!(matches!(
            show.command,
            Command::State {
                command: StateCommand::Show { address },
                ..
            } if address == "application.api"
        ));
    }

    #[test]
    fn parses_generated_query_inputs_for_an_imperative_read() {
        let cli = Cli::try_parse_from([
            "dokploy",
            "application",
            "one",
            "--query-application-id",
            "application-1",
        ])
        .expect("generated command line is valid");

        let Command::Imperative(command) = cli.command else {
            panic!("expected an imperative command");
        };
        let invocation = command.into_invocation().expect("generated input is valid");

        assert_eq!(invocation.operation_id(), "application-one");
        assert_eq!(invocation.path(), "application.one");
    }

    #[test]
    fn raw_json_body_replaces_generated_required_body_fields() {
        let cli = Cli::try_parse_from([
            "dokploy",
            "application",
            "create",
            "--body-json",
            r#"{"name":"API","environmentId":"environment-1"}"#,
        ])
        .expect("raw JSON body satisfies the generated input contract");

        let Command::Imperative(command) = cli.command else {
            panic!("expected an imperative command");
        };
        command
            .into_invocation()
            .expect("raw JSON object is a valid request body");
    }

    #[test]
    fn generated_enum_inputs_reject_unknown_values() {
        let result = Cli::try_parse_from([
            "dokploy",
            "application",
            "create",
            "--body-name",
            "API",
            "--body-environment-id",
            "environment-1",
            "--body-source-type",
            "unknown-provider",
        ]);
        let error = match result {
            Ok(_) => panic!("unknown generated enum values must be rejected"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("possible values"));
    }
}
