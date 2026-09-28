use miette::{Result, miette};
use tracing_subscriber::EnvFilter;

const DOKPLOY_LOG: &str = "DOKPLOY_LOG";

pub fn initialize() -> Result<()> {
    let filter = EnvFilter::try_from_env(DOKPLOY_LOG)
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| miette!("failed to initialize tracing: {error}"))
}
