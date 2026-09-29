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
            Command::Init { .. } | Command::Schema | Command::Validate { .. }
        )
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a starter declarative configuration.
    Init {
        /// Configuration file to create.
        #[arg(
            long,
            default_value = dokploy_config::DEFAULT_CONFIG_FILE,
            value_name = "PATH"
        )]
        file: PathBuf,

        /// Create the canonical empty configuration without prompting.
        #[arg(long)]
        empty: bool,
    },

    /// Print the declarative configuration JSON Schema.
    Schema,

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

    /// Inspect or select local connection contexts.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },

    /// Call an operation from the pinned Dokploy API contract.
    #[command(flatten)]
    Imperative(Box<ImperativeCommand>),
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;

    use super::{Cli, Command, ContextCommand};

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
            Command::Init { file, empty: true }
                if file.as_path() == Path::new("dokploy.yaml")
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

        assert!(matches!(cli.command, Command::Schema));
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
