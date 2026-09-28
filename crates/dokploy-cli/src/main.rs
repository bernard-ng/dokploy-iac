use std::io;
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
    let config = ConfigRepository::platform()?;
    let credentials = KeyringCredentialStore;
    let stdout = io::stdout();
    let mut output = stdout.lock();

    dokploy_cli::execute(cli, &config, &credentials, &mut output).await
}
