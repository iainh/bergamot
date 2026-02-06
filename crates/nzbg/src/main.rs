mod app;
mod cli;
mod download;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

use crate::cli::Cli;

struct StubFetcher;

#[async_trait::async_trait]
impl download::ArticleFetcher for StubFetcher {
    async fn fetch_body(&self, message_id: &str) -> Result<Vec<Vec<u8>>> {
        anyhow::bail!("no NNTP connection configured for article {message_id}")
    }
}

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
    let fetcher: Arc<dyn download::ArticleFetcher> = Arc::new(StubFetcher);
    app::run(config, fetcher).await
}
