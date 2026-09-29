use std::io;
use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use dokploy_cli::cli::Cli;
use dokploy_cli::config::ConfigRepository;
use dokploy_cli::credentials::KeyringCredentialStore;
use miette::Result;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:?}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    dokploy_cli::telemetry::initialize()?;

    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut output = stdout.lock();

    if cli.is_offline() {
        let terminal_available = io::stdin().is_terminal() && stdout.is_terminal();
        return dokploy_cli::execute_offline(cli, &mut output, terminal_available);
    }

    let config = ConfigRepository::platform()?;
    let credentials = KeyringCredentialStore;

    dokploy_cli::execute(cli, &config, &credentials, &mut output).await
}
