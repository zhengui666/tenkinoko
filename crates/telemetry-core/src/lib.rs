use anyhow::Result;
use tracing_subscriber::EnvFilter;

pub fn init() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize telemetry: {error}"))?;
    Ok(())
}
