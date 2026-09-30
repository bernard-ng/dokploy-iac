use std::io;
use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use dokploy_cli::CommandStatus;
use dokploy_cli::cli::Cli;
use dokploy_cli::config::ConfigRepository;
use dokploy_cli::credentials::KeyringCredentialStore;
use miette::Result;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(CommandStatus::Success) => ExitCode::SUCCESS,
        Ok(CommandStatus::Failure) => ExitCode::FAILURE,
        Ok(CommandStatus::ChangesPresent) => ExitCode::from(2),
        Err(error) => {
            eprintln!("{error:?}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<CommandStatus> {
    dokploy_cli::telemetry::initialize()?;

    let cli = Cli::parse();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();

    if cli.is_offline() {
        let terminal_available = io::stdin().is_terminal() && stdout.is_terminal();
        dokploy_cli::execute_offline(cli, &mut output, terminal_available)?;
        return Ok(CommandStatus::Success);
    }

    let config = ConfigRepository::platform()?;
    let credentials = KeyringCredentialStore;

    dokploy_cli::execute_with_input(cli, &config, &credentials, &mut input, &mut output).await
}
