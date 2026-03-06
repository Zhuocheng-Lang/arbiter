use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use arbiter::app;
use arbiter::cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // ── logging ───────────────────────────────────────────────────────────────
    let log_level = cli.log_level.as_deref().unwrap_or("info");
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level)),
        )
        .init();

    app::run(cli).await
}
