mod app;
mod cli;

use anyhow::{Context, Result};
use clap::Parser;

use crate::cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let config_path = match cli.config {
        Some(path) => path,
        None => app::default_config_path()
            .context("no config file found; use --config to specify one")?,
    };

    let config = app::load_config(&config_path)?;
    app::init_tracing(&cli.log_level);

    tracing::info!("nzbg starting");
    app::run(config).await
}
