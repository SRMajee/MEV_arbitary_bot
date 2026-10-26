//! Low-latency Ethereum MEV atomic backrun arbitrage engine entrypoint.

use eyre::Result;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .init();

    info!("Initializing Low-Latency Atomic MEV Arbitrage Engine v0.1.0...");
    info!("Runtime initialized cleanly. Awaiting module configuration.");

    Ok(())
}
